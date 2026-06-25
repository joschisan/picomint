# picomint-bft

Byzantine-tolerant atomic broadcast over a DAG. Each peer publishes
one *unit* per round, accumulates threshold cosignatures, and a
deterministic leader-vote rule extracts a total order over the
confirmed units' payloads.

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
anywhere**. Lax insert, demand-pull, and `SignedUnit` overwrite
together replace what other DAG protocols would solve with timeouts.
This is the load-bearing principle behind many of the design choices
documented below.

## Performance

Items reach the total order in roughly `3 × avg_RTT` between peers.
For a 50 ms inter-peer RTT, that's ~150 ms commit latency.

## Glossary

- **Session** — an instance of consensus, identified by a `u64`. All
  signatures bind to the session via the keychain API; a stale message
  from a previous session fails verification.
- **Round** — a row of the DAG, `u16`. Round 0 is the root row; its
  units carry no parents.
- **Slot** — a `(round, creator)` coordinate. At most one *body* per
  slot can ever reach threshold (see *consistent broadcast* below).
- **Unit** — the body at a slot: the creator's payload (`Vec<D>`),
  parent set, and identifying metadata. Defined in [`unit.rs`].
- **Creator sig** (`sig`) — the schnorr signature of the unit body by
  its claimed creator. Carried as the `sig` field of a `Unit` message
  and stored at `(round, creator, creator)` in `cosigs_table`.
- **Cosig** — a non-creator peer's schnorr signature on the same body.
  Up to `2f` accumulate per slot in `cosigs_table`; with the creator's
  own sig that's `2f+1` = threshold.
- **Threshold** — `2f + 1` total sigs (1 creator + 2f cosigs).
- **Confirmed** — a slot's local sig count has reached threshold.
- **Extended** — a slot is confirmed *and* every parent slot is also
  extended. Equivalent to "this slot is in the in-memory `extended`
  set the extender scans". A slot must be extended (not just
  confirmed) before it can be used as a parent for a future own unit.

## Wire protocol

Three message types. The sender's `PeerId` is attached by the network
layer; never carried in the payload.

```rust
enum Message<D> {
    Unit { unit: Unit<D>, sig: Signature },
    Cosig { round, creator, signer, cosig: Signature },
    SignedUnit { unit, cosigs: BTreeMap<PeerId, Signature> },
    Request { round, creator },
}
```

`SignedUnit` carries no separate creator sig — the creator's signature
is one entry of the `cosigs` map (keyed by the creator), which holds
the full `threshold` set.

| Message | Bytes (n=4 / n=10) | Emission rule |
|---|---|---|
| `Unit` | `~70 + |D|` | Creator's broadcast at unit-creation; creator's anti-entropy push of own highest slot. |
| `Cosig` | `~70` | First-time-cosign fan-out by each cosigner; rebroadcast of our own existing cosig when we re-receive the body. |
| `SignedUnit` | `~70 + |D| + 64·(2f+1)` | Sole `Request` response, *only* when the responder holds the slot at threshold. |
| `Request` | `~3` | On-receive demand-pull of a not-yet-extended parent, on receipt of a `Unit` or `SignedUnit`. |

Every broadcast (`Recipient::Everyone`) carries content authored by
the sender: their own newly-created unit, their own anti-entropy push
of their column, or their own first cosig. **No peer ever relays
another peer's authored content.** Other peers' bodies flow only on
explicit `Request`.

## Storage

All persisted state lives in two redb tables. They are *declared by
the daemon* and passed into `Engine::new`; bft only reads and writes
them:

```rust
units_table:  (Round, PeerId)         => Unit<D>   // BFT_UNITS
cosigs_table: (Round, PeerId, PeerId) => Cosig     // BFT_COSIGS
```

A unit's body sits at `(round, creator)` in `units_table`. Every
signature over that body sits in `cosigs_table` keyed by signer,
including the creator's own sig at `(round, creator, creator)`. A slot
is *confirmed* once its cosig-row count reaches `threshold`.

Everything else is in-memory state on `Engine<P, D>`, rebuilt on
startup and never persisted:

```rust
extended:          BTreeSet<(Round, PeerId)>,          // confirmed + all parents extended
emitted:           BTreeSet<(Round, PeerId)>,          // already sent through ordered_tx
next_decide_round: Round,                              // extender cursor
request_sent_at:   BTreeMap<(Round, PeerId), Instant>, // demand-pull throttle
```

Persistence is just the per-message redb commit. Inbound `Unit` /
`Cosig` / `SignedUnit` commits are **relaxed** (non-fsync): they are
peer-originated and re-fetched via anti-entropy after a crash. The
fsync barrier is own-unit creation, whose durable commit before
broadcast both prevents our own equivocation and flushes the relaxed
backlog.

On restart `replay` re-runs `try_extend` from every round-0 creator
and then `run_extender` once. Because `try_extend` is a fixpoint over
the parent-extended predicate and the extender is deterministic over
the stored unit/cosig set, this reconstructs the exact same
`extended` / `emitted` / `next_decide_round` and re-emits every
previously-committed item through `ordered_tx`; the caller's
idempotency check absorbs the redelivery.

## Lifecycle of a slot

The protocol is split into two gates with distinct semantics.

### Admission (lax)

`insert_unit(dbtx, unit, sig)` installs a fresh slot from a `Unit`
message. A `Unit` carries only the body and the creator's own sig —
cosigs never ride along; they arrive separately as `Cosig` messages
and accumulate in `cosigs_table` via `record_cosig`. Admission checks:

- The encoded body is within `BFT_UNIT_BYTE_LIMIT` (50 KB).
- Structural validity: round 0 has empty parents; round R>0 has
  exactly `threshold` parent creators, all drawn from the federation.
- The creator sig verifies against the body under the session.
- Whether parents are *locally present, confirmed, or extended* is
  **not** checked. An out-of-order arrival lands in `units_table`
  anyway, so it's ready the moment its parents catch up rather than
  being dropped and refetched.

A duplicate body at an occupied slot is rejected — `insert_unit`
errors and the per-message write rolls back; first body seen wins. The
only path that overwrites a stored body is `insert_signed_unit` (see
*Forker recovery*), authorized by a full threshold proof.

### Promotion (strict, ancestrally complete)

`try_extend(round, creator)` walks ascending rounds extending slots
that satisfy:

1. Not already in `extended`.
2. Confirmed (cosig-row count `≥ threshold`).
3. Round 0, *or* every parent slot is already in `extended`.

Extension inserts `(round, creator)` into `extended` — the slot set
the extender scans when extracting the total order. The cascade sweeps
`round + 1`, `round + 2`, … until a sweep produces zero new
extensions, which by induction means no higher round can have new
extensions either.

A slot must be `extended` (not just `confirmed`) to be used as a
parent in own-unit construction (`parents_for`). This guarantees every
unit we author is itself extendable on receivers that hold those
parents extended.

## Anti-entropy and demand-pull

Two propagation mechanisms, each with a narrow role:

**Anti-entropy push (1 Hz)**: each peer sends its *own* highest unit
to everyone. Each peer is canonical for its own column of the DAG;
pushing only the own slot gives laggards a reentry point. Other peers'
columns flow only on demand-pull.

**Demand-pull (event-driven)**: on every receive of a `Unit` or
`SignedUnit`, the receiver walks back through the message's
not-yet-`extended` ancestors and unicasts `Request { round, creator }`
to the immediate sender for any slot it does not yet hold confirmed.
This single walk triggers three recovery cases:

- **Body missing** — the response delivers it.
- **Below threshold** — the responder's `SignedUnit` reply carries the
  canonical body plus the full `threshold` cosig set, fixing any sig
  deficit.
- **Confirmed but ancestrally not extended** — receiving the body
  re-fires the deeper walk-back via the same parent-pull loop.

A `Cosig` for a slot whose body we don't hold triggers no request — it
simply fails to record and is dropped; the missing body arrives on the
creator's next anti-entropy push. Re-issuing on every receive (fresh
or duplicate) makes the mechanism self-healing against dropped
requests: the next time the pushing peer ships the same child, we
re-ask for the still-not-extended parents. A per-slot
`REQUEST_DEDUP_INTERVAL` throttle keeps those re-asks from re-firing
the whole ancestor walk every second.

## Forker recovery via `SignedUnit`

A Byzantine creator can fork its own slot — sending body `B₁` to one
half of the federation and body `B₂` to the other half. Each subset
can collect cosigs only on the body it saw, so neither side reaches
threshold on its own.

Quorum math saves us:

> At most one body per slot can ever assemble `1 + 2f` valid sigs,
> since honest peers cosign exactly one body. So among the two
> halves, at most one body reaches threshold.

The peer holding the canonical (`B₂`) confirmed body responds to a
`Request` with `SignedUnit { unit, cosigs }` — a bundle carrying the
full `threshold` (`2f+1`) sig set over the canonical body, the
creator's sig included. The receiver verifies the bundle and
*atomically replaces* its prior `B₁` entry with `B₂`, clearing any
stale cosigs over `B₁` as a side effect. That's the only place in the
protocol where overwrite happens, and it's authorized exclusively by
the threshold proof.

Sub-threshold slots get no `Request` response — the responder only
emits `SignedUnit` when their entry is locally confirmed.

## Total ordering

Confirmed-and-extended units enter the extender, which runs a leader-vote
rule per round to extract a total order:

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

The full proof is straightforward by induction: once any honest peer
has decided a candidate verdict at round `R`, all honest peers will
eventually reach the same verdict for that candidate.

Sketch:
1. *Fork safety*: at most one body per slot reaches threshold, by
   the quorum argument above. So all honest peers that hold a
   confirmed slot at `(R, c)` hold the same body.
2. *Vote determinism*: voting at round `K` depends only on which
   confirmed units exist at rounds `R..K` and on their parent sets.
   Both are immutable once a unit is confirmed.
3. *Eventual delivery*: anti-entropy + demand-pull + `SignedUnit`
   overwrite together guarantee every honest peer eventually sees the
   same set of confirmed units.

Combine and the verdict at round `K` for candidate `c` is a function
of state that all honest peers eventually share, so all reach the
same conclusion.

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

Unit body fan-out dominates (~99%). Cosig and anti-entropy are two
orders of magnitude smaller. Catch-up under loss is O(n × R) Request
/ SignedUnit pairs for a peer R rounds behind — paid one-shot.

## Layout

- [`lib.rs`] — crate root; re-exports the public surface (`Engine`,
  `Keychain`, `Message`, `Unit`, `DataProvider`, …).
- [`unit.rs`] — `Unit<D>`, `UnitData`, `Cosig`, `Round` type alias.
- [`engine.rs`] — `Engine<P, D>`: the `run` loop (anti-entropy push,
  inbound message handling, unit creation), lax insert, the extension
  cascade, `record_cosig`, `insert_signed_unit`, and all graph state
  over the two redb tables.
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
