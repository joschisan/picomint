# picomint-client

Client library for picomint mints. One `Client` manages any number of added mints — `add_mint(invite, network)` / `begin_remove_mint(mint)` — and every operation takes the `MintId` it acts on. It owns the per-module client state machines (ecash, onchain, lightning, gateway) and exposes operations as flat `async fn` calls that submit a mint transaction and surface their progress through an append-only **event log**.

## Event log model

Every public operation (`ecash_send`, `onchain_receive`, `lightning_invoice_send`, …) returns either a result directly or an `OperationId`. The actual progress of long-running operations — mint acceptance, on-chain confirmation, lightning preimage delivery — is reported by writing typed events to a per-client append-only log.

Integrators consume events via:

- `Client::subscribe_operation_events(op)` — stream of all events for a specific operation
- `Client::get_event_log(pos, limit)` — paged read of the global log
- `Client::event_notify()` — `tokio::sync::Notify` handle that fires whenever new events land

Each event carries its `OperationId` and a `(source, kind)` discriminator. Sources are `Core`, `Ecash`, `Onchain`, `Lightning`, `Gateway`. The flow charts below show, per operation, exactly which event sequences are possible.

## Shared events

These come from the transaction-submission and ecash state machines and appear across every module:

| Event | Source | Meaning |
|---|---|---|
| `TxCreateEvent { txid, reissue, fee }` | Core | Tx submitted to the mint. `fee` is the mint fee paid; `reissue` is the over-pull beyond the deficit that the mint reissues back as fresh notes once the tx is accepted. |
| `TxAcceptEvent { txid }` | Core | Mint accepted the tx into consensus. |
| `TxRejectEvent { txid, error }` | Core | Mint definitively rejected the tx (double-spend, invalid input, fee too low, …). |
| `EcashSuccessEvent { txid, amount }` | Ecash | Threshold blind-sig shares aggregated and the resulting `SpendableNote`s written to the local note table. |
| `EcashFailureEvent` | Ecash | A blind-sig aggregation produced a note that fails verification — should not happen with honest nodes. |

Any operation that mints notes (every send/receive in this library, since they all flow through the ecash module's tx machinery) and whose tx is accepted ends with either an `EcashSuccessEvent` or an `EcashFailureEvent` for its outputs, in addition to whatever module-specific events it emits; a rejected tx ends at `TxRejectEvent` alone.

## Ecash

### `ecash_receive(mint, account, ecash)` — claim out-of-band ecash

```
ReceiveEvent ── TxCreateEvent
    │
    ├── TxAcceptEvent ──┬── EcashSuccessEvent    (notes landed in NOTE table)
    │                   │
    │                   └── EcashFailureEvent  (TBS verify fails)
    │
    └── TxRejectEvent                           (e.g. double-spend)
```

`OperationId` is derived deterministically from the ecash bytes, so replaying the same `receive` call with the same ecash is rejected with `ReceiveEcashError::AlreadyAttempted` rather than attempting a double-spend.

### `ecash_send(mint, account, amount)` — produce out-of-band ecash

Returns an `Ecash` bundle directly (or `SendEcashError` on failure); `Ecash`'s serde representation is the `picomint`-prefixed base32 string callers hand off out-of-band, and the same encoding lands in the event log. Internally `send` awaits the operation's terminal `SendSuccessEvent` / `SendFailureEvent`, so observers see the same shape regardless of fast/slow path. `SendEvent` fires immediately so a UI can render an in-flight card right away. On the slow path the immediately-following `ReissueEvent` / `TxCreateEvent` carry the reissuance txid.

Two paths. The fast path triggers when the wallet already holds notes whose denominations sum exactly to `amount` — `SendEvent` and `SendSuccessEvent` land atomically in one dbtx, no tx, no SM. Otherwise the slow path reissues notes through the mint first, and an `ecash::SendStateMachine` watches the reissuance terminate and emits the terminal `SendSuccessEvent` (assembling the ecash from the freshly minted notes) or `SendFailureEvent`.

```
ecash_send(mint, account, amount)
    │
    ├── SendEvent ── SendSuccessEvent                          (fast path, atomic)
    │
    └── SendEvent ── ReissueEvent ── TxCreateEvent
                                          │
                                          ├── TxAcceptEvent ──┬── EcashSuccessEvent ──┬── SendSuccessEvent
                                          │                   │                      └── SendFailureEvent  (assembly failed — defensive)
                                          │                   └── EcashFailureEvent ── SendFailureEvent
                                          │
                                          └── TxRejectEvent ── SendFailureEvent
```

Every send terminates in exactly one of `SendSuccessEvent` or `SendFailureEvent`. The defensive `SendFailureEvent` after `EcashSuccessEvent` only triggers if a concurrent op consumed the freshly minted notes between the mint terminal and the SM's transition — it should never happen under normal use, but the SM declines to retry rather than livelock.

## Onchain

### `onchain_receive(mint, account)` — peg-in

`onchain_receive` returns a deposit address and emits no events. A background scanner polls the mint for outputs at the client's derived addresses; once it sees a deposit it submits a reissuance tx and emits the events:

```
ReceiveEvent ── TxCreateEvent                  ← scanner saw deposit, submitted reissuance tx
    │
    ├── TxAcceptEvent ──┬── EcashSuccessEvent   (notes landed)
    │                   │
    │                   └── EcashFailureEvent (TBS verify fails)
    │
    └── TxRejectEvent
```

### `onchain_send(mint, account, address, amount, fee)` — peg-out

Submits a tx with a `OnchainOutput`, then an onchain-specific `SendStateMachine` tracks the bitcoin-side outcome while the ecash state machine handles any change notes in parallel.

```
SendEvent ── TxCreateEvent
    │
    ├── TxAcceptEvent ──┬── SendSuccessEvent     (pegout txid observed on bitcoin)
    │                   ├── SendFailureEvent     (mint could not produce a bitcoin tx)
    │                   ├── EcashSuccessEvent     (change notes — parallel)
    │                   └── EcashFailureEvent   (TBS verify fails for change)
    │
    └── TxRejectEvent                            (e.g. zero-fee aborts)
```

`SendSuccessEvent` and `SendFailureEvent` are alternatives produced by the onchain `SendStateMachine`. `EcashSuccessEvent` and `EcashFailureEvent` are alternatives produced by the ecash state machine for change. The two state machines run concurrently after `TxAcceptEvent` — the events can interleave in either order.

## Lightning

Every gateway operation takes the `gateway_pk: GatewayPk` of a gateway from `lightning_gateways(mint)`, which maps every probed gateway in the mint's announced set to its latest `GatewayInfo` (its fees). Callers pick one, preview the cost from its info, and pass the pk; the info only changes on `lightning_refresh_gateways(mint)`, which re-probes the announced set. Gateways are reached over pooled iroh connections, discovered from the mint's announced pk set — there are no gateway URLs on the client side. The library still enforces `PaymentFee::SEND_FEE_LIMIT` on sends and `PaymentFee::RECEIVE_FEE_LIMIT` on receives against the gateway's info as a backstop against an abusive gateway.

The methods come in two families. `lightning_invoice_*` take or produce a BOLT11 invoice. `lightning_lnurl_*` take or produce an lnurl, and split again by how the payment travels: `lightning_lnurl_send`, `lightning_lnurl_send_max` and `lightning_lnurl_send_max_amount` resolve the lnurl to an invoice and go through a gateway, while `lightning_lnurl_send_direct`, `lightning_lnurl_send_direct_max` and `lightning_lnurl_send_direct_max_amount` pay an lnurl of the sender's own mint with no gateway at all. `lightning_lnurl_mint(lnurl)` tells which added mint an lnurl belongs to, so a caller picks the family before it prices anything.

### `lightning_invoice_receive(mint, account, gateway_pk, amount)` — receive over Lightning

Returns a BOLT11 invoice and emits no events. The gateway authors the incoming contract it will fund from the account's receive key, preimage included; the recipient sees nothing of it until a background scanner, polling the mint's incoming-contract stream, finds a contract locked to its key and submits the claim tx:

```
ReceiveEvent ── TxCreateEvent                  ← scanner saw paid contract, submitted claim tx
    │
    ├── TxAcceptEvent ──┬── EcashSuccessEvent   (notes landed)
    │                   │
    │                   └── EcashFailureEvent (TBS verify fails)
    │
    └── TxRejectEvent
```

`lightning_lnurl_receive(mint, account, lnurl_daemon)` hands out a reusable lnurl for the account, served by the hosted lnurl daemon; a payment to it lands exactly as above.

### `lightning_invoice_send(mint, account, gateway_pk, invoice)` — pay a BOLT11 invoice

Submits a funding tx that locks an `OutgoingContract`, then a `SendStateMachine` races the gateway's payment response (over its pooled iroh connection) against the mint's preimage table; whichever finishes first decides between success and refund. The contract settles only through the gateway, by preimage or by forfeit signature, so the state machine waits for as long as that takes. If a refund is taken, a second tx is submitted under the same operation id to claim the contract back.

```
SendEvent ── TxCreateEvent                      ← funding tx submitted
    │
    ├── TxAcceptEvent ──┬── EcashSuccessEvent    (change notes — parallel)
    │                   ├── EcashFailureEvent
    │                   │
    │                   ├── SendSuccessEvent    (gateway returned preimage
    │                   │                        or the mint revealed it)
    │                   │
    │                   └── SendRefundEvent ── TxCreateEvent ──┬── TxAcceptEvent ──┬── EcashSuccessEvent
    │                       (refund claim tx)                  │                   └── EcashFailureEvent
    │                                                          │
    │                                                          └── TxRejectEvent ── SendSuccessEvent
    │
    └── TxRejectEvent
```

Every send whose funding tx is accepted terminates in exactly one of (a rejected funding tx ends at `TxRejectEvent` alone):

- `SendSuccessEvent { preimage }` — gateway paid (either reported back, or the preimage was recovered after a refund-tx rejection).
- `EcashSuccessEvent` (clean refund tail) — refund tx was accepted and the recovered notes minted (`EcashFailureEvent` if TBS verification of the refund notes fails).

The refund-rejection branch fires because the contract input has already been spent — and the only thing that can spend it is the gateway claiming with a preimage, so the state machine waits for the mint's preimage table to show it.

`lightning_lnurl_send(mint, account, gateway_pk, lnurl, amount)` resolves the lnurl to an invoice for the amount, refuses one for any other amount, and pays it exactly as above; `lightning_lnurl_send_max` does the same for the amount that empties the account, which `lightning_lnurl_send_max_amount` previews.

### `lightning_lnurl_send_direct(mint, account, lnurl, amount)` — pay an lnurl of this mint

An lnurl of this mint, as `lightning_lnurl_receive` hands out, names the recipient's receive key; the sender authors the recipient's incoming contract itself, funds it straight from the account at no fee, and is done — there is no gateway and nothing to settle, so the funding tx's acceptance is the payment and the recipient's scanner claims the contract as it claims any other. Any other lnurl is refused. `lightning_lnurl_send_direct_max` empties the account this way, and `lightning_lnurl_send_direct_max_amount` previews what that pays, priced with no gateway fee.

```
SendEvent ── TxCreateEvent                      ← incoming contract funded
    │
    ├── TxAcceptEvent ──┬── EcashSuccessEvent    (change notes)
    │                   └── EcashFailureEvent
    │
    └── TxRejectEvent
```

## Restore

Restore is built into `add_mint`. Adding a mint from an invite downloads and verifies its config, then scans the seed's counter spaces before anything is written — config, counter marks, and recovered notes all land in one dbtx, so a crash leaves either a fully added mint or nothing.

The scan touches no database. It walks each account's counter space in batches, asking the mint two membership questions per batch — which nonces it has seen spent, and which blinded messages it ever signed — and stops on the first batch that turns up neither. Both probes answer under threshold consensus, so no single node can write a counter off. It then fetches the signature shares for the live set in one request and verifies each note against the aggregate key.

Recovered notes are credited directly to the note table rather than reissued, so the balance is simply there when the client opens — no restore-specific events are emitted. The trade-off is linkability: the mint was asked about each of these nonces by name, so a restored wallet is linkable to its scan until the notes churn out through the change of ordinary transactions.

The scan runs on every add, not just conscious restores: a seed that has been added before holds notes behind counters a fresh client would re-derive from zero, stranding them. A seed that never held anything scans to nothing, which costs a round trip and is otherwise indistinguishable. `begin_remove_mint` stages the deletion of every mint-scoped row in a dbtx the caller commits (so an embedder can drop its own mint-scoped rows in the same transaction), and re-adding later scans against clean state.

## Event kinds

The complete `(source, kind)` set the client emits, for integrators wiring up an event-router or filtering subscriptions. Headers/subheaders are intentionally not prescribed — that's a UI decision per integrator.

| Source · Kind |
|---|
| `Core` · `tx-create` |
| `Core` · `tx-accept` |
| `Core` · `tx-reject` |
| `Ecash` · `receive` |
| `Ecash` · `send` |
| `Ecash` · `send-success` |
| `Ecash` · `send-failure` |
| `Ecash` · `reissue` |
| `Ecash` · `success` |
| `Ecash` · `failure` |
| `Onchain` · `receive` |
| `Onchain` · `send` |
| `Onchain` · `send-success` |
| `Onchain` · `send-failure` |
| `Lightning` · `receive` |
| `Lightning` · `send` |
| `Lightning` · `send-success` |
| `Lightning` · `send-refund` |
| `Gateway` · `send` |
| `Gateway` · `send-success` |
| `Gateway` · `send-cancel` |
| `Gateway` · `receive` |
| `Gateway` · `receive-success` |
| `Gateway` · `receive-failure` |

Conventions:

- **Kind never repeats source.** The `Source` discriminator already tags the module, so mint terminals are bare `success` / `failure`. Kinds prefix with the operation only when scoped to one (`send-success`, `send-refund`).
- **Multiple terminals per operation are possible** because some flows fan out to parallel state machines (e.g. an onchain send emits both `SendSuccessEvent` *and* `EcashSuccessEvent` for change, a lightning refund tail emits a `SendRefundEvent` followed by its own mint terminal). Rather than try to pick one "primary" terminal and hide the rest, render every event — observing all of them keeps the UI faithful to what actually happened on the wire.
