# picomint-bft

Byzantine-tolerant atomic broadcast over a DAG. Each peer publishes
one creator-signed *unit* per round, and a deterministic leader-vote
rule extracts a total order over the extended units' payloads.

> **Interim state**: the consistent-broadcast (cosignature) layer has
> been removed in preparation for a fork-tolerant (Mysticeti-style)
> commit rule. Until that rule lands, peers store the first valid body
> they see per slot and the ordering rule assumes slot uniqueness —
> i.e. equivocation by a Byzantine peer is not yet tolerated.

## Scope and threat model

picomint-bft is engineered for one specific operating point:

- **Adversary**: `f` Byzantine peers out of `n = 3f+1` total. Honest
  peers follow the protocol; Byzantine peers may fork, refuse to
  participate, equivocate, or send arbitrary garbage.
- **Network**: assumed honest and roughly random. Messages may drop
  independently and arrive with variable latency, but no adversary
  controls the network — there is no protection against coordinated
  message reordering by a network-level attacker.
- **Goal**: deliver low ordering latency under varying network
  conditions while remaining safe against the Byzantine peers.

The most concrete consequence: **picomint-bft has no timeouts
anywhere**. Lax insert and demand-pull replace what other DAG
protocols would solve with timeouts. This is the load-bearing
principle behind many of the design choices documented below.

## Performance

A DAG round now costs one message delay (units are broadcast once, no
cosign wave). Latency figures for the new structure are pending
re-measurement.

## Glossary

- **Session** — an instance of consensus, identified by a `u64`. All
  signatures bind to the session via the keychain API; a stale message
  from a previous session fails verification.
- **Round** — a row of the DAG. Round 0 is the root row; its units
  carry no parents.
- **Slot** — a `(round, creator)` coordinate. Peers store the first
  valid body they see per slot (see the interim note above).
- **Unit** — the body at a slot: the creator's payload (`Vec<D>`),
  parent set, and identifying metadata. Defined in [`unit.rs`].
- **SignedUnit** — a unit plus its creator's schnorr signature over
  `(session, unit)`. The one shape units travel on the wire and
  persist in storage.
- **Extended** — a slot's body is stored *and* every parent slot is
  also extended. Equivalent to "this slot is in the in-memory
  `extended` set the extender scans". A slot must be extended before
  it can be used as a parent for a future own unit.

## Wire protocol

Two message types. The sender's `PeerId` is attached by the network
layer; never carried in the payload.

```rust
enum Message<D> {
    Unit(SignedUnit<D>),
    Request { round, creator },
}
```

| Message | Bytes | Emission rule |
|---|---|---|
| `Unit` | `~70 + |D|` | Creator's broadcast at unit-creation; creator's anti-entropy push of own highest slot; sole `Request` response. |
| `Request` | `~3` | On-receive demand-pull of a missing ancestor, on receipt of a `Unit`. |

Every broadcast (`Recipient::Everyone`) carries content authored by
the sender: their own newly-created unit or their own anti-entropy
push of their column. Other peers' bodies flow only on explicit
`Request`, answered by whoever holds them.

## Storage

All persisted state lives in one redb table. It is *declared by the
daemon* and passed into `Engine::new`; bft only reads and writes it:

```rust
units_table: (Round, PeerId) => SignedUnit<D>   // BFT_UNITS
```

Everything else is in-memory state on `Engine<P, D>`, rebuilt on
startup and never persisted:

```rust
extended:          BTreeSet<(Round, PeerId)>,          // stored + all parents extended
emitted:           BTreeSet<(Round, PeerId)>,          // already sent through ordered_tx
next_decide_round: Round,                              // extender cursor
request_sent_at:   BTreeMap<(Round, PeerId), Instant>, // demand-pull throttle
```

Persistence is just the per-message redb commit. Inbound `Unit`
commits are **relaxed** (non-fsync): they are peer-originated and
re-fetched via anti-entropy after a crash. The fsync barrier is
own-unit creation, whose durable commit before broadcast both
prevents our own equivocation and flushes the relaxed backlog.

On restart `replay` re-runs `try_extend` from every round-0 creator
and then `run_extender` once. Because `try_extend` is a fixpoint over
the parent-extended predicate and the extender is deterministic over
the stored unit set, this reconstructs the exact same `extended` /
`emitted` / `next_decide_round` and re-emits every previously-committed
item through `ordered_tx`; the caller's idempotency check absorbs the
redelivery.

## Lifecycle of a slot

The protocol is split into two gates with distinct semantics.

### Admission (lax)

`insert_unit(dbtx, signed)` installs a fresh slot from a `Unit`
message. Admission checks:

- The encoded body is within `BFT_UNIT_BYTE_LIMIT` (50 KB).
- Structural validity: round 0 has empty parents; round R>0 has
  exactly `threshold` parent creators, all drawn from the federation.
- The creator sig verifies against the body under the session.
- Whether parents are *locally present or extended* is **not**
  checked. An out-of-order arrival lands in `units_table` anyway, so
  it's ready the moment its parents catch up rather than being
  dropped and refetched.

A duplicate body at an occupied slot is rejected — `insert_unit`
errors and the per-message write rolls back; first body seen wins.

### Promotion (strict, ancestrally complete)

`try_extend(round, creator)` walks ascending rounds extending slots
that satisfy:

1. Not already in `extended`.
2. Body stored in `units_table`.
3. Round 0, *or* every parent slot is already in `extended`.

Extension inserts `(round, creator)` into `extended` — the slot set
the extender scans when extracting the total order. The cascade sweeps
`round + 1`, `round + 2`, … until a sweep produces zero new
extensions, which by induction means no higher round can have new
extensions either.

A slot must be `extended` to be used as a parent in own-unit
construction (`parents_for`). This guarantees every unit we author is
itself extendable on receivers that hold those parents extended.

## Anti-entropy and demand-pull

Two propagation mechanisms, each with a narrow role:

**Anti-entropy push (1 Hz)**: each peer sends its *own* highest unit
to everyone. Each peer is canonical for its own column of the DAG;
pushing only the own slot gives laggards a reentry point. Other peers'
columns flow only on demand-pull.

**Demand-pull (event-driven)**: on every receive of a `Unit`, the
receiver walks back through the message's not-yet-`extended` ancestors
and unicasts `Request { round, creator }` to the immediate sender for
any slot whose body it does not yet hold. Present-but-not-extended
ancestors are descended through (we already hold their parent sets);
missing bodies are requested and terminate the walk.

Re-issuing on every receive (fresh or duplicate) makes the mechanism
self-healing against dropped requests: the next time the pushing peer
ships the same child, we re-ask for the still-not-extended parents. A
per-slot `REQUEST_DEDUP_INTERVAL` throttle keeps those re-asks from
re-firing the whole ancestor walk every second.

## Total ordering

Extended units enter the extender, which runs a leader-vote rule per
round to extract a total order:

- For each round `R`, candidates are walked in a deterministic random
  permutation seeded by the round number.
- A round-`R+1` unit votes **yes** for candidate `c` iff `c` appears
  in its parent set, otherwise **no**.
- A round-`K` unit (`K > R+1`) votes **yes** iff a strict majority of
  its `2f+1` parents voted yes.
- If some round above `R` has `≥ 2f+1` yes-voters, `c` is **elected**
  the round head. If `≥ 2f+1` no-voters, `c` is **eliminated** and we
  move to the next candidate. Otherwise, **undecided** — wait for
  more units.
- If every candidate eliminates, the round is **skipped**.

On commit, the head's not-yet-emitted causal ancestors are extracted
BFS-style and emitted as the round's batch (oldest-first).

## Safety

In the interim state, agreement rests on slot uniqueness: as long as
no creator equivocates, all honest peers eventually hold the same
unit at every slot (anti-entropy + demand-pull), the per-unit votes
are deterministic over those units' parent sets, and the `2f+1`
verdict thresholds guarantee every honest peer reaches the same
elect/eliminate verdict per candidate.

A forking creator can currently split honest peers onto different
bodies for its slot and break that argument — this is the gap the
upcoming fork-tolerant commit rule (hash parents, vote/certificate
patterns, anchor-based indirect decisions) closes.

Session binding via the sig prefix `(session, &unit)` ensures stale
messages from prior sessions can never be confused with the current
session's slot — verification fails on the receiver side without any
explicit session check on the body.

## Network complexity

Per-peer bandwidth at sustained max throughput (50 KB unit bodies,
20 rounds/sec, no drops). Figures are aggregates at a single peer
summed across its `n−1` links; egress and ingress are equal by
symmetry — each peer broadcasts its own unit to `n−1` peers and
receives `n−1` peers' units in return:

| n | t | egress | ingress |
|---|---|---|---|
| 4 | 3 | ~3.15 MB/s | ~3.15 MB/s |
| 7 | 5 | ~6.35 MB/s | ~6.35 MB/s |
| 10 | 7 | ~9.6 MB/s | ~9.6 MB/s |

Per individual link it's a `1/(n−1)` slice of each column — e.g. at
n=4, ~1.05 MB/s egress and ~1.05 MB/s ingress on each of the 3
connections.

Unit body fan-out is the only sustained traffic; anti-entropy is two
orders of magnitude smaller. Catch-up under loss is O(n × R) Request
/ Unit pairs for a peer R rounds behind — paid one-shot.

## Layout

- [`lib.rs`] — crate root; re-exports the public surface (`Engine`,
  `Keychain`, `Message`, `Unit`, `SignedUnit`, `DataProvider`, …).
- [`unit.rs`] — `Unit<D>`, `SignedUnit<D>`, `UnitData`, `Round` type
  alias.
- [`engine.rs`] — `Engine<P, D>`: the `run` loop (anti-entropy push,
  inbound message handling, unit creation), lax insert, the extension
  cascade, and all graph state over the units table.
- [`extender.rs`] — leader-vote ordering and BFS batch extraction (an
  `impl Engine` block).
- [`network.rs`] — `Message<D>`, `Recipient`, `INetwork` trait.
- [`keychain.rs`] — schnorr `sign(session, value)` / `verify(session,
  value, sig, peer)` with session-binding hash prefix.
- [`data.rs`] — `DataProvider<D>` trait for unit payload sourcing.

[`lib.rs`]: src/lib.rs
[`unit.rs`]: src/unit.rs
[`engine.rs`]: src/engine.rs
[`extender.rs`]: src/extender.rs
[`network.rs`]: src/network.rs
[`keychain.rs`]: src/keychain.rs
[`data.rs`]: src/data.rs
