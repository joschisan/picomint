// After concretization, Hasher64 is a no-op marker; the actual hash function
// is the free `aleph_bft_types::hash` (sha256). Kept as a type so upstream tests
// that reference `Hasher64` as a marker still compile.
#[derive(Copy, Clone, Eq, PartialEq, std::hash::Hash, Debug, Default)]
pub struct Hasher64;
