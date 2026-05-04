# picomint-client

Client library for picomint federations. Owns the per-module client state machines (mint, wallet, lightning) and exposes operations as `async fn` calls that submit a federation transaction and surface their progress through an append-only **event log**.

## Event log model

Every public operation (`mint().send`, `wallet().receive`, `ln().send`, …) returns either a result directly or an `OperationId`. The actual progress of long-running operations — federation acceptance, on-chain confirmation, lightning preimage delivery — is reported by writing typed events to a per-client append-only log.

Integrators consume events via:

- `Client::subscribe_operation_events(op)` — stream of all events for a specific operation
- `Client::get_event_log(pos, limit)` — paged read of the global log
- `Client::event_notify()` — `tokio::sync::Notify` handle that fires whenever new events land

Each event carries its `OperationId` and a `(source, kind)` discriminator. Sources are `Core`, `Mint`, `Wallet`, `Ln`, `Gw`. The flow charts below show, per operation, exactly which event sequences are possible.

## Shared events

These come from the transaction-submission and mint state machines and appear across every module:

| Event | Source | Meaning |
|---|---|---|
| `TxAcceptEvent { txid }` | Core | Federation accepted the tx into consensus. |
| `TxRejectEvent { txid, error }` | Core | Federation definitively rejected the tx (double-spend, invalid input, fee too low, …). |
| `MintSuccessEvent { txid }` | Mint | Threshold blind-sig shares aggregated and the resulting `SpendableNote`s written to the local note table. |
| `MintFailureEvent` | Mint | A blind-sig aggregation produced a note that fails verification — should not happen with honest peers. |

Any operation that mints notes (every send/receive in this library, since they all flow through the mint module's tx machinery) ends with either a `MintSuccessEvent` or a `MintFailureEvent` for its outputs, in addition to whatever module-specific events it emits.

## Mint

### `mint().receive(ecash)` — claim out-of-band ecash

```
ReceiveEvent
    │
    ├── TxAcceptEvent ──┬── MintSuccessEvent    (notes landed in NOTE table)
    │                   │
    │                   └── MintFailureEvent  (TBS verify fails)
    │
    └── TxRejectEvent                           (e.g. double-spend)
```

Idempotent: `OperationId` is derived deterministically from the ecash bytes, so replaying the same `receive` call with the same ecash returns the existing op without re-emitting.

### `mint().send(amount)` — produce out-of-band ecash

Two paths. The fast path triggers when the wallet already holds notes whose denominations sum exactly to `amount`; otherwise the slow path reissues notes through the federation first.

```
send(amount)
    │
    ├── SendEvent                                          (fast path: notes already match)
    │
    └── RemintEvent                                        (slow path)
          │
          ├── TxAcceptEvent ──┬── MintSuccessEvent ── SendEvent
          │                   │
          │                   └── MintFailureEvent
          │
          └── TxRejectEvent
```

In the slow path, `send()` blocks until `MintSuccessEvent` lands, then recurses into the fast path to produce the `SendEvent`.

## Wallet

### `wallet().receive()` — peg-in

`receive()` returns a deposit address and emits no events. A background scanner polls the federation for outputs at the wallet's derived addresses; once it sees a deposit it submits a reissuance tx and emits the events:

```
ReceiveEvent                                   ← scanner saw deposit, submitted reissuance tx
    │
    ├── TxAcceptEvent ──┬── MintSuccessEvent   (notes landed)
    │                   │
    │                   └── MintFailureEvent (TBS verify fails)
    │
    └── TxRejectEvent
```

### `wallet().send(address, value, fee)` — peg-out

Submits a tx with a `WalletOutput`, then a wallet-specific `SendStateMachine` tracks the bitcoin-side outcome while the mint state machine handles any change notes in parallel.

```
SendEvent
    │
    ├── TxAcceptEvent ──┬── SendSuccessEvent     (pegout txid observed on bitcoin)
    │                   ├── SendFailureEvent     (federation could not produce a bitcoin tx)
    │                   ├── MintSuccessEvent     (change notes — parallel)
    │                   └── MintFailureEvent   (TBS verify fails for change)
    │
    └── TxRejectEvent                            (e.g. zero-fee aborts)
```

`SendSuccessEvent` and `SendFailureEvent` are alternatives produced by the wallet `SendStateMachine`. `MintSuccessEvent` and `MintFailureEvent` are alternatives produced by the mint state machine for change. The two state machines run concurrently after `TxAcceptEvent` — the events can interleave in either order.

## Lightning

### `ln().receive(amount, expiry, description)` — receive over Lightning

Returns a BOLT11 invoice and emits no events. A background scanner polls `ln_await_incoming_contracts`; when an incoming contract decrypts to the recipient's key it submits the claim tx:

```
ReceiveEvent                                   ← scanner saw paid contract, submitted claim tx
    │
    ├── TxAcceptEvent ──┬── MintSuccessEvent   (notes landed)
    │                   │
    │                   └── MintFailureEvent (TBS verify fails)
    │
    └── TxRejectEvent
```

### `ln().send(invoice)` — pay a BOLT11 invoice

Submits a funding tx that locks an `OutgoingContract`, then a `SendStateMachine` advances `Funding → Funded`. In `Funded` it races the gateway HTTP payment against the federation's preimage stream; whichever finishes first decides between success and refund. If a refund is taken, a second tx is submitted under the same operation id to claim the contract back.

```
SendEvent                                       ← funding tx submitted
    │
    ├── TxAcceptEvent ──┬── MintSuccessEvent    (change notes — parallel)
    │                   ├── MintFailureEvent
    │                   │
    │                   ├── SendSuccessEvent    (gateway returned preimage
    │                   │                        or fed revealed it)
    │                   │
    │                   └── SendRefundEvent ──┬── TxAcceptEvent ──┬── MintSuccessEvent
    │                       (refund claim tx) │                   └── MintFailureEvent
    │                                         │
    │                                         └── TxRejectEvent ──┬── SendSuccessEvent
    │                                                             └── SendFailureEvent
    │
    └── TxRejectEvent
```

Every send terminates in exactly one of:

- `SendSuccessEvent { preimage }` — gateway paid (either reported back during `Funded`, or the preimage was recovered after a refund-tx rejection).
- `MintSuccessEvent` (clean refund tail) — refund tx was accepted and the recovered notes minted.
- `SendFailureEvent` — refund tx was rejected and the federation still doesn't have a preimage we can verify.

The refund-rejection branch fires because the contract input has already been spent — and the only thing that can spend it is the gateway claiming with a preimage. The state machine re-polls the federation once more after refund rejection: if the preimage is now visible, the original send actually succeeded (`SendSuccessEvent`); if not, the operation is genuinely stuck (`SendFailureEvent`).

## Recovery

`Client::init_recovery` seeds a recovery row in the same dbtx that opens the database, so "join + start recovery" commits atomically. The recovery driver then walks the federation's history of issued notes and identifies every spendable note that derives from the wallet's mnemonic. When the scan completes, it submits a single reissuance transaction that consumes all recovered notes as inputs and re-mints them under fresh blinded outputs, all under the operation id returned by `init_recovery`.

```
RecoveryEvent { index: 0, total: None    }            ← seeded by init_recovery
    │
    │  (driver wakes, calls recovery_count to fill in total)
    ▼
RecoveryEvent { index: 0, total: Some(N) }
    │
    │  (one event per processed slice)
    ▼
RecoveryEvent { index: k, total: Some(N) }
    │
    ▼
RecoveryEvent { index: N, total: Some(N) }            ← terminal; submits reissuance tx
    │
    ├── TxAcceptEvent ──┬── MintSuccessEvent          (reissued outputs landed)
    │                   │
    │                   └── MintFailureEvent          (TBS verify fails on a reissued output)
    │
    └── TxRejectEvent                                 (federation refused reissuance,
                                                       e.g. an invalid recovered input)
```

Progress is reported as a monotonically increasing `index` over an eventually-known `total`. The terminal `RecoveryEvent` (`index == total`) is emitted in the same dbtx that deletes the recovery state and submits the reissuance tx, so observing it guarantees the tx is in flight. From there the operation follows the standard mint flow — `TxAcceptEvent` + `MintSuccessEvent` on success, `MintFailureEvent` only on the rare verification failure of a *reissued output*, and `TxRejectEvent` if the federation refuses the reissuance (which is also how a bad *recovered input* surfaces — the federation rejects the tx rather than client-side verification kicking in).

Re-minting every recovered note keeps the recovery path uniform with the rest of the client: there is no special txid-less success case, and the recovered balance is provably spendable the moment `MintSuccessEvent` lands. An integrator restoring a wallet can wait for `MintSuccessEvent` under the recovery `OperationId` and treat that as full restore-complete.

## Suggested UI mapping

A drop-in `(source, kind) → card` mapping for clients that want a uniform status surface across all operations. Triggers and ongoing-state events use a present-continuous header; terminal and milestone events use a bare past/static label. The operation title (e.g. "Send Lightning · 5 000 sats") is set once from the trigger event and stays put; subsequent cards only update the header/subheader beneath it.

| Source · Kind | Header | Subheader |
|---|---|---|
| `Core` · `tx-accept`                    | Transaction Accepted | — |
| `Core` · `tx-reject`                    | Transaction Rejected | — |
| `Mint` · `receive`                      | Receiving eCash      | `{amount} sats` |
| `Mint` · `send`                         | Sending eCash        | `{amount} sats` |
| `Mint` · `remint`                       | Reminting eCash      | preparing exact denominations |
| `Mint` · `success`                      | Success              | — |
| `Mint` · `failure`                      | Failure              | threshold signature invalid |
| `Mint` · `recovery`                     | Recovering eCash     | `{percent}%` (0% while `total` is `None`) |
| `Wallet` · `receive`                    | Receiving Onchain    | `{value} sats at {address}` |
| `Wallet` · `send`                       | Sending Onchain      | `{value} sats to {address}` |
| `Wallet` · `send-success`               | Success              | `bitcoin tx {txid}` |
| `Wallet` · `send-failure`               | Failure              | missing txid |
| `Ln` · `receive`                        | Receiving Lightning  | `{amount} sats` |
| `Ln` · `send`                           | Sending Lightning    | `{amount} sats · fee {ln_fee + fee}` |
| `Ln` · `send-success`                   | Success              | preimage received |
| `Ln` · `send-refund` (`expired: true`)  | Refunding            | contract expired |
| `Ln` · `send-refund` (`expired: false`) | Refunding            | gateway cancelled |
| `Ln` · `send-failure`                   | Failure              | missing preimage |

Conventions:

- **Kind never repeats source.** The `Source` discriminator already tags the module, so mint terminals are bare `success` / `failure`. Kinds prefix with the operation only when scoped to one (`send-success`, `send-refund`).
- **Color/icon keys off kind**, not the mapping: `tx-reject`, `*-failure` → red; `*-success`, mint `success` → green; `send-refund` → amber; in-progress events → spinner.
- **Multiple terminals per operation are possible** because some flows fan out to parallel state machines (e.g. wallet send emits both `SendSuccessEvent` *and* `MintSuccessEvent` for change). The renderer should pick a single primary terminal per operation — typically the module-specific one — and silently consume the rest.
