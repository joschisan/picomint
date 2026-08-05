# picomint-bft

Byzantine-tolerant atomic broadcast over a DAG. Each peer publishes
one creator-signed *unit* per round, parents pin exact bodies by
hash, and a deterministic QuickAleph-style virtual-voting rule
(arXiv:1908.05156) extracts a total order over the extended units'
payloads. Equivocation by up to `f` creators per round is tolerated
in the graph itself — no cosigning, no certification round trips, no
coin.

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

A DAG round costs one message delay (units are broadcast once, no
cosign wave), and a well-referenced candidate decides at round
`R+2` — three message delays end to end. Candidates the fast path
misses resolve through the common-vote schedule within an expected
one to two extra rounds under random network behavior, with
geometrically decaying tails and no absorbing undecided state at any
federation size. Dropping the cosign wave cuts messages per round
from Θ(n³) to Θ(n²) and per-peer signature verifications per round
from Θ(n²) to Θ(n).

Measured in the mock at n = 22 (f = 7) without drops: agreement over
a 24k-item order, head decisions at mean depth 4.1 rounds under iid
latency jitter (median item delay 6.4 round-periods including the
ancestry sweep), and mean depth 2.25 with 92% three-delay decisions
under a stable per-peer latency ladder. In the lossy 4-peer mock
(25 ms ± 15 ms, 10% drop), item delay averages ~1 s — set by loss
recovery at the 1 Hz anti-entropy cadence, not by decision depth.

## Glossary

- **Session** — an instance of consensus, identified by a `u64`. All
  signatures bind to the session via the keychain API; a stale message
  from a previous session fails verification.
- **Round** — a row of the DAG. Round 0 is the root row; its units
  carry no parents.
- **UnitHash** — the sha256 consensus-hash of a unit body. Parent
  references pin the exact parent body via this hash.
- **Slot** — a `(round, creator, hash)` coordinate: the storage key of
  a unit body. The `(round, creator)` prefix names the position the
  body claims; the hash disambiguates fork branches within it. An
  honest creator has exactly one branch per position; a Byzantine
  creator may have several, stored side by side.
- **Unit** — the body at a slot: the creator's payload (`Vec<D>`),
  parent map, and identifying metadata. Defined in [`unit.rs`].
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
    Request(Slot),
}
```

| Message | Bytes | Emission rule |
|---|---|---|
| `Unit` | `~70 + 32·|parents| + |D|` | Creator's broadcast at unit-creation; creator's anti-entropy push of own highest slot; sole `Request` response. |
| `Request` | `~37` | On-receive demand-pull of a missing ancestor, on receipt of a `Unit`. |

Every broadcast (`Recipient::Everyone`) carries content authored by
the sender: their own newly-created unit or their own anti-entropy
push of their column. Other peers' bodies flow only on explicit
`Request`, answered by whoever holds them.

## Storage

All persisted state lives in one redb table. It is *declared by the
daemon* and passed into `Engine::new`; bft only reads and writes it:

```rust
units_table: Slot => SignedUnit<D>   // BFT_UNITS, Slot = (Round, PeerId, UnitHash)
```

Everything else is in-memory state on `Engine<P, D>`, rebuilt on
startup and never persisted:

```rust
extended:          BTreeMap<Slot, Parents>,     // stored + all parents extended, mapped to parent maps
emitted:           BTreeSet<Slot>,              // already sent through ordered_tx
next_decide_round: Round,                       // extender cursor
decided:           BTreeMap<Slot, bool>,        // candidate decisions, pruned below the cursor
votes:             BTreeMap<(Slot, Slot), bool>, // memoized virtual votes, pruned with decided
request_sent_at:   BTreeMap<Slot, Instant>,     // demand-pull throttle
```

The `extended` map carries each extended unit's parent map, which is
the complete evidence the decision rule needs — virtual votes are
computed from parent maps alone, so the extender never touches the
db except to read payloads at emission time.

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
- Structural validity: round 0 has an empty parent map; round R>0 has
  exactly `threshold` parent entries, all keyed by federation members.
- The creator sig verifies against the body under the session.
- Whether parents are *locally present or extended* is **not**
  checked. An out-of-order arrival lands in `units_table` anyway, so
  it's ready the moment its parents catch up rather than being
  dropped and refetched.

A duplicate body hits its own key and is rejected — `insert_unit`
errors and the per-message write rolls back.

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
and unicasts `Request(slot)` to the immediate sender for any slot
whose body it does not yet hold. Present-but-not-extended ancestors
are descended through (we already hold their parent maps); missing
bodies are requested and terminate the walk.

Re-issuing on every receive (fresh or duplicate) makes the mechanism
self-healing against dropped requests: the next time the pushing peer
ships the same child, we re-ask for the still-not-extended parents. A
per-slot `REQUEST_DEDUP_INTERVAL` throttle keeps those re-asks from
re-firing the whole ancestor walk every second.

## Total ordering

Extended units enter the extender, which runs a deterministic
QuickAleph-style virtual-voting rule per round. Every extended
round-`R` branch is a *candidate*, walked in a priority order seeded
by the round; each candidate resolves to a binary include/exclude
decision:

- A round-`R+1` unit **votes** 1 for branch `B` iff its parent entry
  for `B`'s creator is exactly `B`'s hash.
- A unit above `R+1` adopts its parents' votes if they are
  **unanimous**, otherwise it votes the round's **common vote** —
  fixed 1 at `R+2`, fixed 0 at `R+3`, seeded pseudo-random bits
  above. The bits are plain hashes of `(round, candidate)`: every
  peer computes the same value, and commonness — not
  unpredictability — is all safety needs under a benign network.
- A unit at round `R+2` or above **decides** the common-vote value
  `v` of its round iff at least `2f+1` of its parents vote `v`. One
  deciding unit anywhere suffices — the decision propagates
  structurally (see Safety).
- The round **head** is the first candidate in priority order decided
  1, once every earlier candidate is decided 0. Candidates outside
  the ancestry of a held `R+3` unit are skipped without waiting:
  they are globally doomed to decide 0 (the coverage lemma below),
  which is what makes walking a deterministic rather than secret
  permutation safe. If every candidate excludes, the round is
  **skipped**.

The fixed 1 at `R+2` gives well-referenced candidates a
three-message-delay include; the fixed 0 at `R+3` excludes invisible
candidates fast, keeping the walk moving past crashed peers; the
seeded bits break middle-band ties within an expected extra round.
Decisions are stable once reached, so they are cached in `decided`
for the engine's lifetime, and startup replay reproduces the exact
live emission sequence.

On commit, the head's not-yet-emitted causal ancestors are extracted
BFS-style and emitted as the round's batch (oldest-first). An
equivocator's sibling branches may both be swept as ancestry — the
guarantee is one identical order on every peer, not single-branch
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
inherits 0, and no peer can ever decide it 1 — skipping it without
waiting is safe.

Votes are tallied over *extended* units only, which makes the
evidence self-contained: an extended unit's parents are extended, so
every referenced branch is one we hold.

## Liveness under quiescence

Unit creation is work-gated: a peer only builds a unit that carries
items or keeps the DAG growing while an earlier unit of its own awaits
ordering. A head at round `R` decides once rounds `R+1` and `R+2` (or
a few more, on the common-vote path) exist, and those evidence rounds
are built while at least `2f+1` peers
still await ordering of their own units — guaranteed for client work
because submissions fan out to every guardian, so all peers propose
(and keep building until they order) their own copy. A tail unit that
no committed head happened to sweep before the federation went
quiescent waits for the next burst of work: its items were also
proposed and ordered through the other guardians' units, and the
self-parent chain sweeps the whole backlog on the next commit.

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
- [`extender.rs`] — the virtual-voting decision rule and BFS batch
  extraction (an `impl Engine` block).
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
