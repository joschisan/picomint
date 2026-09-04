# picomint-client

Client library for picomint mints. One `Client` manages any number of added mints — `add_mint(invite)` / `begin_remove_mint(mint)` — and every operation takes the `MintId` it acts on. It owns the per-module client state machines (ecash, onchain, lightning, gateway) and exposes operations as flat `async fn` calls that submit a mint transaction and surface their progress through an append-only **event log**.

## Event log model

Every public operation (`ecash_send`, `onchain_receive`, `lightning_send`, …) returns either a result directly or an `OperationId`. The actual progress of long-running operations — mint acceptance, on-chain confirmation, lightning preimage delivery — is reported by writing typed events to a per-client append-only log.

Integrators consume events via:

- `Client::subscribe_operation_events(op)` — stream of all events for a specific operation
- `Client::get_event_log(pos, limit)` — paged read of the global log
- `Client::event_notify()` — `tokio::sync::Notify` handle that fires whenever new events land

Each event carries its `OperationId` and a `(source, kind)` discriminator. Sources are `Core`, `Ecash`, `Onchain`, `Lightning`, `Gateway`. The flow charts below show, per operation, exactly which event sequences are possible.

## Shared events

These come from the transaction-submission and ecash state machines and appear across every module:

| Event | Source | Meaning |
|---|---|---|
| `TxCreateEvent { txid, remint, fee }` | Core | Tx submitted to the mint. `fee` is the mint fee paid; `remint` is the over-pull beyond the deficit that the mint reissues back as fresh notes once the tx is accepted. |
| `TxAcceptEvent { txid }` | Core | Mint accepted the tx into consensus. |
| `TxRejectEvent { txid, error }` | Core | Mint definitively rejected the tx (double-spend, invalid input, fee too low, …). |
| `EcashSuccessEvent { txid }` | Ecash | Threshold blind-sig shares aggregated and the resulting `SpendableNote`s written to the local note table. |
| `EcashFailureEvent` | Ecash | A blind-sig aggregation produced a note that fails verification — should not happen with honest nodes. |

Any operation that mints notes (every send/receive in this library, since they all flow through the ecash module's tx machinery) ends with either an `EcashSuccessEvent` or an `EcashFailureEvent` for its outputs, in addition to whatever module-specific events it emits.

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

Idempotent: `OperationId` is derived deterministically from the ecash bytes, so replaying the same `receive` call with the same ecash returns the existing op without re-emitting.

### `ecash_send(mint, account, amount)` — produce out-of-band ecash

Returns an `Ecash` bundle directly (or `SendEcashError` on failure); `Ecash`'s serde representation is the `picomint`-prefixed base32 string callers hand off out-of-band, and the same encoding lands in the event log. Internally `send` awaits the operation's terminal `SendSuccessEvent` / `SendFailureEvent`, so observers see the same shape regardless of fast/slow path. `SendEvent` fires immediately so a UI can render an in-flight card right away. On the slow path the immediately-following `RemintEvent` / `TxCreateEvent` carry the reissuance txid.

Two paths. The fast path triggers when the wallet already holds notes whose denominations sum exactly to `amount` — `SendEvent` and `SendSuccessEvent` land atomically in one dbtx, no tx, no SM. Otherwise the slow path reissues notes through the mint first, and an `ecash::SendStateMachine` watches the reissuance terminate and emits the terminal `SendSuccessEvent` (assembling the ecash from the freshly minted notes) or `SendFailureEvent`.

```
ecash_send(mint, account, amount)
    │
    ├── SendEvent ── SendSuccessEvent                          (fast path, atomic)
    │
    └── SendEvent ── RemintEvent ── TxCreateEvent
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

Both `lightning_send` and `lightning_receive` take a caller-selected gateway: a `gateway_pk: GatewayPk` (its identity in the mint's announced gateway set) and a `gateway_info: GatewayInfo` (its latest probed routing info, including all fees and the outgoing-contract expiry delta). Callers pick one via `lightning_select_gateway(mint)` and inspect the returned `gateway_info` to preview the cost before committing; `lightning_refresh_gateways(mint)` re-probes the announced set. Gateways are reached over pooled iroh connections, discovered from the mint's announced pk set — there are no gateway URLs on the client side. The library still enforces `PaymentFee::SEND_FEE_LIMIT` / `LN_FEE_LIMIT` / `RECEIVE_FEE_LIMIT` and `EXPIRY_DELTA_LIMIT` on the supplied `gateway_info` as a backstop against an abusive gateway.

### `lightning_receive(mint, account, gateway_pk, gateway_info, amount)` — receive over Lightning

Returns a BOLT11 invoice and emits no events. A background scanner polls the mint for incoming contracts; when an incoming contract decrypts to the recipient's key it submits the claim tx:

```
ReceiveEvent ── TxCreateEvent                  ← scanner saw paid contract, submitted claim tx
    │
    ├── TxAcceptEvent ──┬── EcashSuccessEvent   (notes landed)
    │                   │
    │                   └── EcashFailureEvent (TBS verify fails)
    │
    └── TxRejectEvent
```

### `lightning_send(mint, account, gateway_pk, gateway_info, invoice)` — pay a BOLT11 invoice

Submits a funding tx that locks an `OutgoingContract`, then a `SendStateMachine` advances `Funding → Funded`. In `Funded` it races the gateway payment response (over its pooled iroh connection) against the mint's preimage stream; whichever finishes first decides between success and refund. If a refund is taken, a second tx is submitted under the same operation id to claim the contract back.

```
SendEvent ── TxCreateEvent                      ← funding tx submitted
    │
    ├── TxAcceptEvent ──┬── EcashSuccessEvent    (change notes — parallel)
    │                   ├── EcashFailureEvent
    │                   │
    │                   ├── SendSuccessEvent    (gateway returned preimage
    │                   │                        or fed revealed it)
    │                   │
    │                   └── SendRefundEvent ── TxCreateEvent ──┬── TxAcceptEvent ──┬── EcashSuccessEvent
    │                       (refund claim tx)                  │                   └── EcashFailureEvent
    │                                                          │
    │                                                          └── TxRejectEvent ──┬── SendSuccessEvent
    │                                                                              └── SendFailureEvent
    │
    └── TxRejectEvent
```

Every send terminates in exactly one of:

- `SendSuccessEvent { preimage }` — gateway paid (either reported back during `Funded`, or the preimage was recovered after a refund-tx rejection).
- `EcashSuccessEvent` (clean refund tail) — refund tx was accepted and the recovered notes minted.
- `SendFailureEvent` — refund tx was rejected and the mint still doesn't have a preimage we can verify.

The refund-rejection branch fires because the contract input has already been spent — and the only thing that can spend it is the gateway claiming with a preimage. The state machine re-polls the mint once more after refund rejection: if the preimage is now visible, the original send actually succeeded (`SendSuccessEvent`); if not, the operation is genuinely stuck (`SendFailureEvent`).

## Restore

Restore is built into `add_mint`. Adding a mint from an invite downloads and verifies its config, then scans the seed's counter spaces before anything is written — config, counter marks, and recovered notes all land in one dbtx, so a crash leaves either a fully added mint or nothing.

The scan touches no database. It walks each account's counter space in batches, asking the mint two membership questions per batch — which nonces it has seen spent, and which blinded messages it ever signed — and stops on the first batch that turns up neither. Both probes answer under threshold consensus, so no single node can write a counter off. It then fetches the signature shares for the live set in one request and verifies each note against the aggregate key.

Recovered notes are credited directly to the note table rather than reissued, so the balance is simply there when the client opens — no restore-specific events are emitted. The trade-off is linkability: the mint was asked about each of these nonces by name, so a restored wallet is linkable to its scan until the notes churn out through the change of ordinary transactions.

The scan runs on every add, not just conscious restores: a seed that has been added before holds notes behind counters a fresh client would re-derive from zero, stranding them. A seed that never held anything scans to nothing, which costs a round trip and is otherwise indistinguishable. `begin_remove_mint` deletes every mint-scoped row, so re-adding later scans against clean state.

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
| `Ecash` · `remint` |
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
| `Lightning` · `send-failure` |
| `Gateway` · `send` |
| `Gateway` · `send-success` |
| `Gateway` · `send-cancel` |
| `Gateway` · `receive` |
| `Gateway` · `receive-success` |
| `Gateway` · `receive-failure` |
| `Gateway` · `receive-refund` |

Conventions:

- **Kind never repeats source.** The `Source` discriminator already tags the module, so mint terminals are bare `success` / `failure`. Kinds prefix with the operation only when scoped to one (`send-success`, `send-refund`).
- **Multiple terminals per operation are possible** because some flows fan out to parallel state machines (e.g. an onchain send emits both `SendSuccessEvent` *and* `EcashSuccessEvent` for change, a lightning refund tail emits a `SendRefundEvent` followed by its own mint terminal). Rather than try to pick one "primary" terminal and hide the rest, render every event — observing all of them keeps the UI faithful to what actually happened on the wire.
