//! Ancestry views and the reference rule.
//!
//! Every extended unit gets a *view*: for each creator, the tip of the
//! single self-parent chain of that creator's units its ancestry
//! contains — or [`View::Forked`], once the ancestry holds two units
//! of one creator that do not lie on one chain. That pair is
//! self-authenticating proof of misbehavior (an honest creator's
//! units always chain), it is absorbing, and it travels with the DAG
//! itself — no alerts, no extra messages.
//!
//! The *reference rule* judges each parent entry of a unit against
//! the combined views of the unit's **other** parents — exactly the
//! evidence its creator provably held when signing: a unit pinning a
//! creator its other parents already prove forked is invalid and
//! never extends, so nothing can reference it, vote on it, or build
//! on it. Excluding the judged pin itself keeps the merge point
//! innocent: the first unit whose parents span both fork branches is
//! valid — it is what carries the evidence upward — while every unit
//! above it is barred from the forked creator, the forker's own
//! column included (its self-pin fails the same judgment). Own-unit
//! creation mirrors the judgment, so an honest node drops a forked
//! creator from its parent row the moment the evidence reaches its
//! ancestry and never signs a unit receivers would refuse.
//!
//! The judgment is a pure function of the unit's fixed ancestry, so
//! every node reaches the same verdict — which is what lets the rule
//! stay local while still quarantining forkers and the colluders that
//! keep pinning them within the few rounds it takes the evidence to
//! spread.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use picomint_core::NodeId;

use crate::unit::{Round, Unit, UnitHash};

/// What a unit's ancestry establishes about one creator's column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum View {
    /// The ancestry contains two of the creator's units that do not
    /// lie on one self-parent chain — proof of a fork. Absorbing.
    Forked,
    /// The tip of the single chain of the creator's units the
    /// ancestry contains.
    Tip(Round, UnitHash),
}

/// A unit's full view: one entry per creator its ancestry contains
/// units of.
pub(crate) type UnitView = BTreeMap<NodeId, View>;

/// True iff `low` lies on the self-parent chain descending from
/// `high`, walked through `creator`-keyed parent entries — one round
/// per step, since parents sit at exactly `round - 1`. A missing
/// self-parent entry breaks the chain, which makes the two units
/// incomparable.
fn on_chain(
    extended: &BTreeMap<UnitHash, Unit>,
    creator: NodeId,
    high: (Round, UnitHash),
    low: (Round, UnitHash),
) -> bool {
    let mut current = high;

    while current.0 > low.0 {
        let unit = extended
            .get(&current.1)
            .expect("view tips are ancestry of extended units");

        match unit.parents.get(&creator) {
            Some(parent) => current = (current.0 - 1, *parent),
            None => return false,
        }
    }

    current.1 == low.1
}

/// Merge two views of one creator's column: the newer tip when both
/// lie on one chain, [`View::Forked`] otherwise.
pub(crate) fn merge_element(
    extended: &BTreeMap<UnitHash, Unit>,
    creator: NodeId,
    a: View,
    b: View,
) -> View {
    let (View::Tip(ra, ha), View::Tip(rb, hb)) = (a, b) else {
        return View::Forked;
    };

    let (high, low) = if ra >= rb {
        ((ra, ha), (rb, hb))
    } else {
        ((rb, hb), (ra, ha))
    };

    if on_chain(extended, creator, high, low) {
        View::Tip(high.0, high.1)
    } else {
        View::Forked
    }
}

/// Merge `from` into `into`, creator by creator.
pub(crate) fn merge_views(
    extended: &BTreeMap<UnitHash, Unit>,
    into: &mut UnitView,
    from: &UnitView,
) {
    for (creator, view) in from {
        match into.entry(*creator) {
            Entry::Vacant(entry) => {
                entry.insert(*view);
            }
            Entry::Occupied(mut entry) => {
                let merged = merge_element(extended, *creator, *entry.get(), *view);

                entry.insert(merged);
            }
        }
    }
}
