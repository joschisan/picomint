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
  its claimed creator. Single, dedicated field on `Entry`.
- **Cosig** — a non-creator peer's schnorr signature on the same body.
  Entries hold up to `2f` cosigs.
- **Threshold** — `2f + 1` total sigs (1 creator + 2f cosigs).
- **Confirmed** — a slot's local sig count has reached threshold.
- **Extended** — a slot is confirmed *and* every parent slot is also
  extended. Equivalent to "this slot has been handed to the extender".
  A slot must be extended (not just confirmed) before it can be used
  as a parent for a future own unit.

## Wire protocol

Three message types. The sender's `PeerId` is attached by the network
layer; never carried in the payload.

```rust
enum Message<D> {
    Unit { unit: Unit<D>, sig: Signature },
    Cosig { round, creator, signer, sig: Signature },
    SignedUnit { unit, sig, cosigs: BTreeMap<PeerId, Signature> },
    Request { round, creator },
    NoUnit,
}
```

| Message | Bytes (n=4 / n=10) | Emission rule |
|---|---|---|
| `Unit` | `~70 + |D|` | Creator's broadcast at unit-creation; creator's anti-entropy push of own highest slot; targeted anti-entropy push of the recipient's highest slot. |
| `Cosig` | `~70` | First-time-cosign fan-out by each cosigner. |
| `SignedUnit` | `~70 + |D| + 65·2f` | Sole `Request` response, *only* when the responder holds the slot at threshold. |
| `Request` | `~3` | On-receive demand-pull when a parent isn't yet extended; on receipt of a `Cosig` for a slot whose body we don't hold. |
| `NoUnit` | `~1` | Targeted anti-entropy probe: sent in place of the recipient's own highest unit when the sender holds nothing of the recipient's column. |

Every broadcast (`Recipient::Everyone`) carries content authored by
the sender: their own newly-created unit, their own anti-entropy push
of their column, or their own first cosig. **No peer ever relays
another peer's authored content.** Other peers' bodies flow only on
explicit `Request`.

## Storage

```rust
struct Entry<D> {
    unit: Unit<D>,
    sig: Signature,                       // creator's, always present
    cosigs: BTreeMap<PeerId, Signature>,  // up to 2f
}

struct Graph<D> {
    session: u64,
    n: NumPeers,
    units: BTreeMap<(Round, PeerId), Entry<D>>,
    extended: BTreeSet<(Round, PeerId)>,
    backup: DynBackup<D>,
    extender: Extender<D>,
}
```

Persistence: every `insert_unit`, `insert_signed_unit`, and
`record_cosig` call that mutates an entry persists the post-mutation
entry through `Backup::save`. On restart, `Graph::new` loads entries
in `(round, peer)` lex order and runs `try_extend` on each in turn.
That order is a valid topological order over the parent relation, so
the cascade resolves in a single pass.

## Lifecycle of a slot

The protocol is split into two gates with distinct semantics.

### Graph admission (lax)

`Graph::insert_unit(unit, sig, cosigs, keychain)` accepts any
well-formed signed unit:

- Creator sig is trusted from the caller (engine pre-verifies).
- Each cosig is re-verified against the body; bad cosigs are filtered
  out, valid ones stored (capped at `2f`).
- `check_parents` enforces structural validity: round 0 has empty
  parents, round R>0 has exactly threshold parent creators all drawn
  from the federation.
- Whether parents are *locally present or confirmed* is not checked.
  An out-of-order arrival lands in the graph anyway, so accumulated
  cosigs persist and propagate immediately rather than being dropped
  and refetched.

Duplicate body at an existing slot: drop the new body (first-seen
wins), merge the carried cosigs.

### Promotion (strict, ancestrally complete)

`Graph::try_extend(round, creator)` walks ascending rounds extending
slots that satisfy:

1. Not already in `extended`.
2. Confirmed (`1 + cosigs.len() ≥ threshold`).
3. Round 0, *or* every parent slot is already in `extended`.

Extension feeds the slot's unit into the extender (the total-order
generator) and inserts `(round, creator)` into `extended`. The
cascade sweeps `round + 1`, `round + 2`, … until a sweep produces zero
new extensions, which by induction means no higher round can have new
extensions either.

A slot must be `extended` (not just `confirmed`) to be used as a
parent in own-unit construction (`Graph::parents_for`). This
guarantees every unit we author is itself extendable on receivers
that hold those parents extended.

## Anti-entropy and demand-pull

Two propagation mechanisms, each with a narrow role:

**Anti-entropy push (1 Hz)**: each peer sends its *own* highest
entry to everyone. Each peer is canonical for its own column of the
DAG; pushing only the own slot gives laggards a reentry point. Each
peer also unicasts every *other* peer's highest column entry back to
that peer — or a `NoUnit` probe if it holds nothing of that column —
so a peer that wiped its data learns its own canonical highest from
its neighbours. Other peers' columns flow only on demand-pull.

## Bootstrap fork-safety gate

A peer that lost its data would otherwise restart with an empty column
and create a fresh round-0 unit, forking the round-0 slot against an
already-confirmed predecessor. To prevent this, each peer refuses to
author own units until it has observed `threshold` distinct peers'
views of its column this session — counting:

- *self* as one responder iff own units survived on disk through
  replay (the on-disk evidence is what lets us safely resume from
  `highest + 1` instead of forking at round 0), and
- each remote peer the first time they echo back our column via
  `Unit { creator = us, sig }` with a verifying creator sig, or send
  a `NoUnit` probe.

The byzantine adversary can lie freely with `NoUnit`, but cannot
forge a `Unit` of our column (no access to our key) and cannot stop
the `f + 1` honest cosigners of any confirmed unit of ours from
honestly echoing it back. So any `threshold` responder set must
contain at least one honest cosigner of every confirmed unit we
ever signed — the gate clearing implies we know our true highest,
so the next slot we author is genuinely unused.

**Demand-pull (event-driven)**: on every receive of `Unit`,
`SignedUnit`, or `Cosig`, the receiver checks the message's parent
slots (or, for `Cosig`, the slot itself). For each slot that isn't
yet `extended`, the receiver unicasts `Request { round, creator }`
to the immediate sender. This single `!is_extended` predicate
triggers three recovery cases:

- **Body missing** — the response delivers it.
- **Below threshold** — the responder's `SignedUnit` reply carries
  the canonical body plus `2f` cosigs, fixing any sig deficit.
- **Confirmed but ancestrally not extended** — receiving the body
  re-fires the deeper walk-back via the same parent-pull loop.

Re-issuing on every receive (fresh or duplicate) is what makes the
mechanism self-healing against dropped requests: the next time the
pushing peer ships the same child, we re-ask for the still-not-extended
parents.

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
`Request` with `SignedUnit { unit, sig, cosigs }` — a bundle carrying
`2f` valid cosigs over the canonical body. The receiver verifies the
bundle and *atomically replaces* its prior `B₁` entry with `B₂`.
That's the only place in the protocol where overwrite happens, and it's
authorized exclusively by the threshold proof.

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
20 rounds/sec, no drops):

| n | t | per-peer per-direction | bidirectional |
|---|---|---|---|
| 4 | 3 | ~3.15 MB/s | ~6.3 MB/s |
| 7 | 5 | ~6.35 MB/s | ~12.7 MB/s |
| 10 | 7 | ~9.6 MB/s | ~19.2 MB/s |

Unit body fan-out dominates (~99%). Cosig and anti-entropy are two
orders of magnitude smaller. Catch-up under loss is O(n × R) Request
/ SignedUnit pairs for a peer R rounds behind — paid one-shot.

## Layout

- [`unit.rs`] — `Unit<D>`, `UnitData`, `Round` type alias.
- [`graph.rs`] — `Entry<D>`, `Graph<D>`, lax insert, extension
  cascade, `record_cosig`, `insert_signed_unit`.
- [`extender.rs`] — leader-vote ordering and BFS batch extraction.
- [`engine.rs`] — `pub async fn run` driving anti-entropy push,
  inbound message handling, and unit creation.
- [`network.rs`] — `Message<D>`, `INetwork` trait.
- [`backup.rs`] — `Backup` trait + `NoopBackup`.
- [`keychain.rs`] — schnorr `sign(session, value)` / `verify(session,
  value, sig, peer)` with session-binding hash prefix.
- [`data.rs`] — `DataProvider<D>` trait for unit payload sourcing.

[`unit.rs`]: src/unit.rs
[`graph.rs`]: src/graph.rs
[`extender.rs`]: src/extender.rs
[`engine.rs`]: src/engine.rs
[`network.rs`]: src/network.rs
[`backup.rs`]: src/backup.rs
[`keychain.rs`]: src/keychain.rs
[`data.rs`]: src/data.rs
