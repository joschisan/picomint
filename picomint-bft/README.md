# picomint-bft

Byzantine-tolerant atomic broadcast over a DAG. Each node publishes
one creator-signed *unit* per round, parents pin exact bodies by
hash, and a deterministic QuickAleph-style virtual-voting rule
(arXiv:1908.05156) extracts a total order over the extended units'
payloads. Equivocation by up to `f` creators per round is tolerated
in the graph itself — no cosigning, no certification round trips, no
coin.

## Scope and threat model

picomint-bft is engineered for one specific operating point:

- **Adversary**: `f` Byzantine nodes out of `n = 3f+1` total. Honest
  nodes follow the protocol; Byzantine nodes may fork, refuse to
  participate, equivocate, or send arbitrary garbage.
- **Network**: assumed honest and roughly random. Messages may drop
  independently and arrive with variable latency, but no adversary
  controls the network — there is no protection against coordinated
  message reordering by a network-level attacker.
- **Goal**: deliver low ordering latency under varying network
  conditions while remaining safe against the Byzantine nodes.

The most concrete consequence: **picomint-bft has no timeouts
anywhere**. Lax insert and demand-pull replace what other DAG
protocols would solve with timeouts. This is the load-bearing
principle behind many of the design choices documented below.

## Performance

A DAG round costs one message delay (units are broadcast once, no
cosign wave), and a well-referenced candidate decides at round
`R+2` — three message delays end to end. Candidates the fast path
misses resolve through the common-vote schedule within an expected
one to two extra rounds under random network behavior, with
geometrically decaying tails and no absorbing undecided state at any
mint size. Dropping the cosign wave cuts messages per round
from Θ(n³) to Θ(n²) and per-node signature verifications per round
from Θ(n²) to Θ(n).

Measured in the mock at n = 22 (f = 7) without drops: agreement over
a 24k-item order, head decisions at mean depth 4.1 rounds under iid
latency jitter (median item delay 6.4 round-periods including the
ancestry sweep), and mean depth 2.25 with 92% three-delay decisions
under a stable per-node latency ladder. In the lossy 4-node mock
(25 ms ± 15 ms, 10% drop), item delay averages ~1 s — set by loss
recovery at the 1 Hz anti-entropy cadence, not by decision depth.

## Glossary

- **Session** — an instance of consensus, identified by a `u32`. All
  signatures bind to the session via the keychain API; a stale message
  from a previous session fails verification.
- **Round** — a row of the DAG. Round 0 is the root row; its units
  carry no parents.
- **UnitHash** — the sha256 consensus-hash of an encoded unit; the
  unit's identity everywhere: storage key of the units table (`"bft-units"`), element of
  the in-memory sets, and how parents pin the exact parent unit. It
  covers the payload transitively through the unit's `data`
  commitment.
- **Position** — the `(round, creator)` pair a unit claims, carried
  inside the unit as an annotation. An honest creator has exactly one
  unit per position; a Byzantine creator may have several — fork
  branches, stored side by side under their own hashes.
- **Unit** — a DAG node: position, parent map, and payload
  commitment (`data: Option<sha256::Hash>`, `None` iff the payload is
  empty) — everything the ordering engine tallies over, and exactly
  the record the in-memory `extended` map holds. Defined in
  [`unit.rs`].
- **UnitEnvelope** — the unit, its payload (`Vec<D>`), and the
  creator's schnorr signature over `(session, unit)`. The one shape
  units travel on the wire and persist in storage.
- **Extended** — a unit's envelope is stored *and* every parent is
  also extended. Equivalent to "this hash is in the in-memory
  `extended` map the extender scans". A unit must be extended before
  it can be used as a parent for a future own unit.

## Wire protocol

Two message types. The sender's `NodeId` is attached by the network
layer; never carried in the payload.

```rust
enum Message<D> {
    Unit(UnitEnvelope<D>),
    Request(UnitHash),
}
```

| Message | Bytes | Emission rule |
|---|---|---|
| `Unit` | `~111 + 33·|parents| + |D|` | Creator's broadcast at unit-creation; creator's anti-entropy push of own highest unit; sole `Request` response. |
| `Request` | `~33` | On-receive demand-pull of a missing ancestor, on receipt of a `Unit`. |

Every broadcast (`Recipient::Everyone`) carries content authored by
the sender: their own newly-created unit or their own anti-entropy
push of their column. Other nodes' envelopes flow only on explicit
`Request`, answered by whoever holds them.

## Storage

All persisted state lives in one database table. It is *declared by the
daemon* and passed into `Engine::new`; bft only reads and writes it:

```rust
units_table: UnitHash => UnitEnvelope<D>   // the units table (`"bft-units"`)
```

Everything else is in-memory state on `Engine<P, D, T>`, rebuilt on
startup and never persisted:

```rust
rounds:             BTreeMap<Round, BTreeSet<UnitHash>>,  // round index over units_table
extended:           BTreeMap<UnitHash, Unit>,       // stored + all parents extended; the bare units
emitted:            BTreeSet<UnitHash>,             // already sent through ordered_tx
next_decide_round:  Round,                          // extender cursor
decided:            BTreeMap<UnitHash, bool>,       // candidate decisions, kept for the engine's lifetime
votes:              BTreeMap<(UnitHash, UnitHash), bool>, // memoized virtual votes, kept with decided
unordered_own_data: BTreeSet<Round>,                // own rounds with unemitted items (work gate)
own_top:            Option<(Round, UnitHash)>,      // own column top: next-round base + anti-entropy push
request_sent_at:    BTreeMap<UnitHash, Instant>,    // demand-pull throttle
```

The `rounds` index is written at exactly one point in steady state
(`insert_unit`), rebuilt directly by `replay` on startup, and provides
every per-round scan: the extension cascade over stored units, and —
filtered by `extended` membership — the extender's candidate and
decider walks. The `extended` map carries each
extended unit's position and parent map, which is the complete
evidence the decision rule needs — virtual votes are computed from
parent maps alone, so the extender never touches the db except to
read payloads at emission time.

Persistence is just the per-message db commit. Inbound `Unit`
commits are **relaxed** (non-fsync): they are node-originated and
re-fetched via anti-entropy after a crash. The fsync barrier is
own-unit creation, whose durable commit before broadcast both
prevents our own equivocation and flushes the relaxed backlog.

On restart `replay` rebuilds the indexes with one table scan, re-runs
`try_extend` from every round-0 unit, and then `run_extender` once.
Because `try_extend` is a fixpoint over the parent-extended predicate
and the extender is deterministic over the stored unit set, this
reconstructs the exact same `extended` / `emitted` /
`next_decide_round` and re-emits every previously-committed item
through `ordered_tx`; the caller's idempotency check absorbs the
redelivery.

## Lifecycle of a unit

The protocol is split into two gates with distinct semantics.

### Admission (lax)

`insert_unit(dbtx, envelope, hash)` installs a fresh envelope from a
`Unit` message and indexes it in `rounds`. Admission checks:

- Structural validity: the creator is a mint member; round 0 has an
  empty parent map; round R>0 has exactly `threshold` parent entries,
  all keyed by mint members.
- The creator sig verifies against the unit under the session.
- Whether parents are *locally present or extended* is **not**
  checked. An out-of-order arrival lands in `units_table` anyway, so
  it's ready the moment its parents catch up rather than being
  dropped and refetched.

A duplicate unit hits its own key and is rejected — `insert_unit`
errors and the per-message write rolls back.

### Promotion (strict, ancestrally complete)

`try_extend(hash)` walks ascending rounds of the `rounds` index
extending units that satisfy:

1. Not already in `extended`.
2. Envelope stored in `units_table`.
3. Every parent is already in `extended`, was created by the node it
   is keyed under, and sits at exactly `round − 1` (round-0 parent
   maps are empty, so vacuously true).

The parent position check pins each unit's claimed position to its
parents' — inductively down to round 0 — so extended rounds are
gap-free and a forker cannot key its own branches under other
creators to fake the distinct-creator quorums the decision rule
tallies.

Extension records the bare unit in `extended` — the unit set the
extender scans when extracting the total order. The
cascade sweeps `round + 1`, `round + 2`, … until a sweep produces
zero new extensions, which by induction means no higher round can
have new extensions either.

A unit must be `extended` to be used as a parent in own-unit
construction (`parents_for`). This guarantees every unit we author is
itself extendable on receivers that hold those parents extended.

## Anti-entropy and demand-pull

Two propagation mechanisms, each with a narrow role:

**Anti-entropy push (1 Hz)**: each node sends its *own* highest unit
to everyone. Each node is canonical for its own column of the DAG;
pushing only the own unit gives laggards a reentry point. Other nodes'
columns flow only on demand-pull.

**Demand-pull (event-driven)**: on every receive of a `Unit`, the
receiver walks back through the message's not-yet-`extended` ancestors
and unicasts `Request(hash)` to the immediate sender for any ancestor
whose body it does not yet hold. Present-but-not-extended ancestors
are descended through (we already hold their parent maps); missing
bodies are requested and terminate the walk.

Re-issuing on every receive (fresh or duplicate) makes the mechanism
self-healing against dropped requests: the next time the pushing node
ships the same child, we re-ask for the still-not-extended parents. A
per-unit `REQUEST_DEDUP_INTERVAL` throttle keeps those re-asks from
re-firing the whole ancestor walk every second.

## Total ordering

Extended units enter the extender, which runs a deterministic
QuickAleph-style virtual-voting rule per round. Every extended
round-`R` branch is a *candidate*, walked in ascending hash order —
the walk order only needs to be *common* across nodes, and a creator
can grind its body hash for early position under any public order,
so re-hashing with the round would buy nothing. Each candidate
resolves to a binary include/exclude decision:

- A round-`R+1` unit **votes** 1 for branch `B` iff its parent entry
  for `B`'s creator is exactly `B`'s hash.
- A unit above `R+1` adopts its parents' votes if they are
  **unanimous**, otherwise it votes the round's **common vote** —
  fixed 1 at `R+2`, fixed 0 at `R+3`, seeded pseudo-random bits
  above. The bits are plain hashes of `(round, candidate)`: every
  node computes the same value, and commonness — not
  unpredictability — is all safety needs under a benign network.
- A unit at round `R+2` or above **decides** the common-vote value
  `v` of its round iff at least `2f+1` of its parents vote `v`. One
  deciding unit anywhere suffices — the decision propagates
  structurally (see Safety).
- The round **head** is the first candidate in hash order decided
  1, once every earlier candidate is decided 0; an undecided
  candidate blocks the round until later rounds decide it. Head
  election waits for an extended `R+3` unit — that is what makes
  walking a deterministic rather than secret order over the local
  candidate set safe (the coverage lemma below). A full walk with
  every candidate decided 0 also waits — the paper's trailing
  `output ⊥`, which its Lemma C.13 makes dead code: some candidate
  is always referenced by `f+1` honest next-round units, so every
  unit two rounds up votes 1 on it and it can never be decided 0.

The fixed 1 at `R+2` gives well-referenced candidates a
three-message-delay include; the fixed 0 at `R+3` excludes invisible
candidates fast, keeping the walk moving past crashed nodes; the
seeded bits break middle-band ties within an expected extra round.
Decisions are stable once reached, so they are cached in `decided`
for the engine's lifetime, and startup replay reproduces the exact
live emission sequence.

On commit, the head's not-yet-emitted causal ancestors are extracted
BFS-style and emitted as the round's batch (oldest-first): rounds
ascend, so every node's own units emit in submission order; within a
round the BFS discovery order is a deterministic function of the head
and the emitted set, hence identical on every node — the paper's hash
tie-break within rounds is not load-bearing. An
equivocator's sibling branches may both be swept as ancestry — the
guarantee is one identical order on every node, not single-branch
emission; item processing downstream validates each item on its own
terms.

## Safety

Fork tolerance rests on four facts:

1. **Evidence is per-unit and hash-pinned.** A vote is a pure
   function of one specific unit's fixed ancestry — a forker can
   create units that vote differently, but never ambiguity about
   what a given unit voted.
2. **Honest creators are single-voiced per round** (self-parent
   chain plus the fsync-before-broadcast barrier), so any `f+1`
   distinct creators include one with exactly one unit that round.
3. **Quorum intersection lands on a concrete unit**: two parent sets
   of `2f+1` distinct creators share `f+1` creators, hence an honest
   one, hence one common actual unit — not merely a common creator
   whose testimony might be forked.
4. **Unanimity-else-default aggregation.** Two same-round units can
   only vote differently if *both* had unanimous parent sets, which
   by (3) would force their common honest parent unit to have voted
   both ways — impossible. Mixed views don't get to choose: they are
   forced onto the common vote, identical everywhere. (Strict
   *majority* aggregation lacks this property — the common unit can
   sit in one side's minority, which is the classic fork
   counterexample and the reason this rule aggregates by unanimity.)

From these: if any unit decides `v`, every unit of its round votes
`v` — unanimous ones by (3)+(4), mixed ones because the deciding
round's default *is* `v` — so every round above inherits `v`
unanimously and no contrary decision can ever form. Decisions are
exclusive and stable, which also makes the engine-lifetime `decided`
and `votes` caches and startup replay sound.

The **coverage lemma** closes head choice: a round-`R+3` vote-1 for a
branch requires a unanimous parent set, and every vote-1 implies
causal containment of the branch, so an `R+3` unit voting 1 shares a
common above-the-branch unit with every other `R+3` unit — including
one we hold. Contrapositive: a branch outside our held `R+3`
ancestry is never voted 1 at `R+3` anywhere, every higher round
inherits 0, and no node can ever decide it 1. A branch we have never
even heard of is outside that ancestry (extended ancestry is
complete), so once an `R+3` unit is extended, no unknown candidate
can outrank a head chosen from the local candidate set — which is
why head election waits for one.

Votes are tallied over *extended* units only, which makes the
evidence self-contained: an extended unit's parents are extended, so
every referenced branch is one we hold.

## Liveness under quiescence

Unit creation is work-gated: a node only builds a unit that carries
items or keeps the DAG growing while an earlier unit of its own awaits
ordering. A head at round `R` decides once rounds `R+1` through `R+3` (or
a few more, on the common-vote path) exist, and those evidence rounds
are built while at least `2f+1` nodes
still await ordering of their own units — guaranteed for client work
because submissions fan out to every node, so all nodes propose
(and keep building until they order) their own copy. A tail unit that
no committed head happened to sweep before the mint went
quiescent waits for the next burst of work: its items were also
proposed and ordered through the other nodes' units, and the
self-parent chain sweeps the whole backlog on the next commit.

Session binding via the sig prefix `(session, &unit)` ensures stale
messages from prior sessions can never be confused with the current
session's units — verification fails on the receiver side without any
explicit session check on the body.

## Network complexity

Per-node bandwidth at sustained max throughput (50 KB unit bodies,
20 rounds/sec, no drops). Figures are aggregates at a single node
summed across its `n−1` links; egress and ingress are equal by
symmetry — each node broadcasts its own unit to `n−1` nodes and
receives `n−1` nodes' units in return:

| n | t | egress | ingress |
|---|---|---|---|
| 4 | 3 | ~3.15 MB/s | ~3.15 MB/s |
| 7 | 5 | ~6.35 MB/s | ~6.35 MB/s |
| 10 | 7 | ~9.6 MB/s | ~9.6 MB/s |

Per individual link it's a `1/(n−1)` slice of each column — e.g. at
n=4, ~1.05 MB/s egress and ~1.05 MB/s ingress on each of the 3
connections.

Unit envelope fan-out is the only sustained traffic; anti-entropy is two
orders of magnitude smaller. Catch-up under loss is O(n × R) Request
/ Unit pairs for a node R rounds behind — paid one-shot.

## Layout

- [`lib.rs`] — crate root; re-exports the public surface (`Engine`,
  `Keychain`, `Message`, `Unit`, `UnitEnvelope`, `DataProvider`, …).
- [`unit.rs`] — `Unit`, `UnitEnvelope<D>`, `UnitHash`, `UnitData`,
  `Round` type alias.
- [`engine.rs`] — `Engine<P, D, T>`: the `run` loop (anti-entropy push,
  inbound message handling, unit creation), lax insert, the extension
  cascade, and all graph state over the units table.
- [`extender.rs`] — the virtual-voting decision rule and BFS batch
  extraction (an `impl Engine` block).
- [`network.rs`] — `Message<D>`, `Recipient`, `INetwork` trait.
- [`keychain.rs`] — schnorr `sign(session, value)` / `verify(session,
  value, sig, node)` with session-binding hash prefix.
- [`data.rs`] — `DataProvider<D>` trait for unit payload sourcing.

[`lib.rs`]: src/lib.rs
[`unit.rs`]: src/unit.rs
[`engine.rs`]: src/engine.rs
[`extender.rs`]: src/extender.rs
[`network.rs`]: src/network.rs
[`keychain.rs`]: src/keychain.rs
[`data.rs`]: src/data.rs
