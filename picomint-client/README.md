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
| `TxCreateEvent { txid, remint, fee }` | Core | Tx submitted to the federation. `fee` is the federation fee paid; `remint` is the over-pull beyond the deficit that the mint reissues back as fresh notes once the tx is accepted. |
| `TxAcceptEvent { txid }` | Core | Federation accepted the tx into consensus. |
| `TxRejectEvent { txid, error }` | Core | Federation definitively rejected the tx (double-spend, invalid input, fee too low, …). |
| `MintSuccessEvent { txid }` | Mint | Threshold blind-sig shares aggregated and the resulting `SpendableNote`s written to the local note table. |
| `MintFailureEvent` | Mint | A blind-sig aggregation produced a note that fails verification — should not happen with honest peers. |

Any operation that mints notes (every send/receive in this library, since they all flow through the mint module's tx machinery) ends with either a `MintSuccessEvent` or a `MintFailureEvent` for its outputs, in addition to whatever module-specific events it emits.

## Mint

### `mint().receive(ecash)` — claim out-of-band ecash

```
ReceiveEvent ── TxCreateEvent
    │
    ├── TxAcceptEvent ──┬── MintSuccessEvent    (notes landed in NOTE table)
    │                   │
    │                   └── MintFailureEvent  (TBS verify fails)
    │
    └── TxRejectEvent                           (e.g. double-spend)
```

Idempotent: `OperationId` is derived deterministically from the ecash bytes, so replaying the same `receive` call with the same ecash returns the existing op without re-emitting.

### `mint().send(amount)` — produce out-of-band ecash

Returns an `ECash` bundle directly (or `SendECashError` on failure); `ECash`'s serde representation is the `picomint`-prefixed base32 string callers hand off out-of-band, and the same encoding lands in the event log. Internally `send` awaits the operation's terminal `SendSuccessEvent` / `SendFailureEvent`, so observers see the same shape regardless of fast/slow path. `SendEvent` fires immediately so a UI can render an in-flight card right away. On the slow path the immediately-following `RemintEvent` / `TxCreateEvent` carry the reissuance txid.

Two paths. The fast path triggers when the wallet already holds notes whose denominations sum exactly to `amount` — `SendEvent` and `SendSuccessEvent` land atomically in one dbtx, no tx, no SM. Otherwise the slow path reissues notes through the federation first, and a `mint::SendStateMachine` watches the reissuance terminate and emits the terminal `SendSuccessEvent` (assembling the ecash from the freshly minted notes) or `SendFailureEvent`.

```
send(amount)
    │
    ├── SendEvent ── SendSuccessEvent                          (fast path, atomic)
    │
    └── SendEvent ── RemintEvent ── TxCreateEvent
                                          │
                                          ├── TxAcceptEvent ──┬── MintSuccessEvent ──┬── SendSuccessEvent
                                          │                   │                      └── SendFailureEvent  (assembly failed — defensive)
                                          │                   └── MintFailureEvent ── SendFailureEvent
                                          │
                                          └── TxRejectEvent ── SendFailureEvent
```

Every send terminates in exactly one of `SendSuccessEvent` or `SendFailureEvent`. The defensive `SendFailureEvent` after `MintSuccessEvent` only triggers if a concurrent op consumed the freshly minted notes between the mint terminal and the SM's transition — it should never happen under normal use, but the SM declines to retry rather than livelock.

## Wallet

### `wallet().receive()` — peg-in

`receive()` returns a deposit address and emits no events. A background scanner polls the federation for outputs at the wallet's derived addresses; once it sees a deposit it submits a reissuance tx and emits the events:

```
ReceiveEvent ── TxCreateEvent                  ← scanner saw deposit, submitted reissuance tx
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
SendEvent ── TxCreateEvent
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

Both `ln().send` and `ln().receive` take a caller-selected gateway as their first two arguments: a `gateway: String` (the HTTP endpoint) and a `gateway_info: GatewayInfo` (its routing info, including all fees and the outgoing-contract expiry delta). Callers pick a gateway via `ln().select_gateway(invoice_for_direct_swap_match)` — or, for full manual control, `ln().list_gateways()` + `ln().gateway_info(api)` — and inspect `gateway_info` to preview the cost before committing. The library still enforces `PaymentFee::SEND_FEE_LIMIT` / `LN_FEE_LIMIT` / `RECEIVE_FEE_LIMIT` and `EXPIRY_DELTA_LIMIT` on the supplied `gateway_info` as a backstop against an abusive gateway.

### `ln().receive(gateway, gateway_info, amount, expiry, description)` — receive over Lightning

Returns a BOLT11 invoice and emits no events. A background scanner polls `ln_await_incoming_contracts`; when an incoming contract decrypts to the recipient's key it submits the claim tx:

```
ReceiveEvent ── TxCreateEvent                  ← scanner saw paid contract, submitted claim tx
    │
    ├── TxAcceptEvent ──┬── MintSuccessEvent   (notes landed)
    │                   │
    │                   └── MintFailureEvent (TBS verify fails)
    │
    └── TxRejectEvent
```

### `ln().send(gateway, gateway_info, invoice)` — pay a BOLT11 invoice

Submits a funding tx that locks an `OutgoingContract`, then a `SendStateMachine` advances `Funding → Funded`. In `Funded` it races the gateway HTTP payment against the federation's preimage stream; whichever finishes first decides between success and refund. If a refund is taken, a second tx is submitted under the same operation id to claim the contract back.

```
SendEvent ── TxCreateEvent                      ← funding tx submitted
    │
    ├── TxAcceptEvent ──┬── MintSuccessEvent    (change notes — parallel)
    │                   ├── MintFailureEvent
    │                   │
    │                   ├── SendSuccessEvent    (gateway returned preimage
    │                   │                        or fed revealed it)
    │                   │
    │                   └── SendRefundEvent ── TxCreateEvent ──┬── TxAcceptEvent ──┬── MintSuccessEvent
    │                       (refund claim tx)                  │                   └── MintFailureEvent
    │                                                          │
    │                                                          └── TxRejectEvent ──┬── SendSuccessEvent
    │                                                                              └── SendFailureEvent
    │
    └── TxRejectEvent
```

Every send terminates in exactly one of:

- `SendSuccessEvent { preimage }` — gateway paid (either reported back during `Funded`, or the preimage was recovered after a refund-tx rejection).
- `MintSuccessEvent` (clean refund tail) — refund tx was accepted and the recovered notes minted.
- `SendFailureEvent` — refund tx was rejected and the federation still doesn't have a preimage we can verify.

The refund-rejection branch fires because the contract input has already been spent — and the only thing that can spend it is the gateway claiming with a preimage. The state machine re-polls the federation once more after refund rejection: if the preimage is now visible, the original send actually succeeded (`SendSuccessEvent`); if not, the operation is genuinely stuck (`SendFailureEvent`).

## Restore

Restore rebuilds a wallet from its seed alone. It is two steps, and the client is built *between* them, against a database that is already consistent.

```rust
let config  = download(&endpoint, &invite).await?;
let restore = restore(&endpoint, &mnemonic, &config).await?;

let dbtx = db.begin_write();
dbtx.insert(&ClientConfig, &federation_id, &config);
commit_restore(&dbtx, &restore);          // counter marks only
dbtx.commit();

let client = Client::new(endpoint, db, logger, &mnemonic, config);
client.mint().receive(&restore.ecash())?;  // ordinary out-of-band receive
```

`restore` touches no database at all. It walks each denomination's counter space concurrently, asking the federation two membership questions per batch — which nonces it has seen spent, and which blinded messages it ever signed — and stops on the first batch that turns up neither. Both probes answer under threshold consensus, so no single peer can write a counter off. It then fetches the signature shares for the live set in one request.

`commit_restore` persists the counter marks the scan reached, and nothing else. Put it in the same dbtx that marks the federation as added: a wallet resuming from zero would re-derive nonces the federation has already signed, stranding every note behind them.

The restored notes come back as an ordinary `ECash` bundle via `Restore::ecash`, so reissuing them is just `receive`. Restore and an out-of-band receive are the same operation — notes someone else may know traded for notes only this wallet does, the someone else here being the federation, which was asked about every one of these nonces by name during the scan. The operation therefore emits `ReceiveEvent` and follows the standard mint flow; there is no restore-specific event.

Neither step depends on the other having succeeded. The marks are safe to persist without the reissuance, and `receive` dedups on `OperationId::from_encodable`, so an interrupted restore is simply run again. A seed that never held anything scans to an empty bundle, which `receive` rejects with `ReceiveECashError::Empty` — check `Restore::amount` first if that is a case you expect.

## Event kinds

The complete `(source, kind)` set the client emits, for integrators wiring up an event-router or filtering subscriptions. Headers/subheaders are intentionally not prescribed — that's a UI decision per integrator.

| Source · Kind |
|---|
| `Core` · `tx-create` |
| `Core` · `tx-accept` |
| `Core` · `tx-reject` |
| `Mint` · `receive` |
| `Mint` · `send` |
| `Mint` · `send-success` |
| `Mint` · `send-failure` |
| `Mint` · `remint` |
| `Mint` · `success` |
| `Mint` · `failure` |
| `Wallet` · `receive` |
| `Wallet` · `send` |
| `Wallet` · `send-success` |
| `Wallet` · `send-failure` |
| `Ln` · `receive` |
| `Ln` · `send` |
| `Ln` · `send-success` |
| `Ln` · `send-refund` |
| `Ln` · `send-failure` |

Conventions:

- **Kind never repeats source.** The `Source` discriminator already tags the module, so mint terminals are bare `success` / `failure`. Kinds prefix with the operation only when scoped to one (`send-success`, `send-refund`).
- **Multiple terminals per operation are possible** because some flows fan out to parallel state machines (e.g. wallet send emits both `SendSuccessEvent` *and* `MintSuccessEvent` for change, an LN refund tail emits a `SendRefundEvent` followed by its own mint terminal). Rather than try to pick one "primary" terminal and hide the rest, render every event — observing all of them keeps the UI faithful to what actually happened on the wire.
