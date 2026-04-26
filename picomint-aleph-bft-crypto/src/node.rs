use derive_more::{Add, AddAssign, From, Into, Sub, SubAssign, Sum};
use picomint_encoding::{Decodable, Encodable};
use std::{
    collections::HashMap,
    fmt,
    hash::Hash,
    io::{self, Read, Write},
    ops::{Div, Index as StdIndex, Mul},
    vec,
};

/// The index of a node
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default, From, Into)]
pub struct NodeIndex(pub usize);

impl Encodable for NodeIndex {
    fn consensus_encode<W: Write>(&self, w: &mut W) -> io::Result<()> {
        (self.0 as u64).consensus_encode(w)
    }
}

impl Decodable for NodeIndex {
    fn consensus_decode_partial<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(NodeIndex(u64::consensus_decode_partial(r)? as usize))
    }
}

/// Indicates that an implementor has been assigned some index.
pub trait Index {
    fn index(&self) -> NodeIndex;
}

/// Node count. Right now it doubles as node weight in many places in the code, in the future we
/// might need a new type for that.
#[derive(
    Copy,
    Clone,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    Debug,
    Default,
    Add,
    AddAssign,
    From,
    Into,
    Sub,
    SubAssign,
    Sum,
)]
pub struct NodeCount(pub usize);

// deriving Mul and Div is somehow cumbersome
impl Mul<usize> for NodeCount {
    type Output = Self;
    fn mul(self, rhs: usize) -> Self::Output {
        NodeCount(self.0 * rhs)
    }
}

impl Div<usize> for NodeCount {
    type Output = Self;
    fn div(self, rhs: usize) -> Self::Output {
        NodeCount(self.0 / rhs)
    }
}

impl NodeCount {
    pub fn into_range(self) -> core::ops::Range<NodeIndex> {
        core::ops::Range {
            start: 0.into(),
            end: self.0.into(),
        }
    }

    pub fn into_iterator(self) -> impl Iterator<Item = NodeIndex> {
        (0..self.0).map(NodeIndex)
    }

    /// If this is the total node count, what number of nodes is required for secure consensus.
    pub fn consensus_threshold(&self) -> NodeCount {
        (*self * 2) / 3 + NodeCount(1)
    }
}

/// A container keeping items indexed by NodeIndex.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Default, Decodable, Encodable, From)]
pub struct NodeMap<T: Encodable + Decodable + 'static>(Vec<Option<T>>);

impl<T: Encodable + Decodable + 'static> NodeMap<T> {
    /// Constructs a new node map with a given length.
    pub fn with_size(len: NodeCount) -> Self
    where
        T: Clone,
    {
        let v = vec![None; len.into()];
        NodeMap(v)
    }

    pub fn from_hashmap(len: NodeCount, hashmap: HashMap<NodeIndex, T>) -> Self
    where
        T: Clone,
    {
        let v = vec![None; len.into()];
        let mut nm = NodeMap(v);
        for (id, item) in hashmap.into_iter() {
            nm.insert(id, item);
        }
        nm
    }

    pub fn size(&self) -> NodeCount {
        self.0.len().into()
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeIndex, &T)> {
        self.0
            .iter()
            .enumerate()
            .filter_map(|(idx, maybe_value)| Some((NodeIndex(idx), maybe_value.as_ref()?)))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (NodeIndex, &mut T)> {
        self.0
            .iter_mut()
            .enumerate()
            .filter_map(|(idx, maybe_value)| Some((NodeIndex(idx), maybe_value.as_mut()?)))
    }

    fn into_iter(self) -> impl Iterator<Item = (NodeIndex, T)>
    where
        T: 'static,
    {
        self.0
            .into_iter()
            .enumerate()
            .filter_map(|(idx, maybe_value)| Some((NodeIndex(idx), maybe_value?)))
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.iter().map(|(_, value)| value)
    }

    pub fn into_values(self) -> impl Iterator<Item = T>
    where
        T: 'static,
    {
        self.into_iter().map(|(_, value)| value)
    }

    pub fn get(&self, node_id: NodeIndex) -> Option<&T> {
        self.0[node_id.0].as_ref()
    }

    pub fn get_mut(&mut self, node_id: NodeIndex) -> Option<&mut T> {
        self.0[node_id.0].as_mut()
    }

    pub fn insert(&mut self, node_id: NodeIndex, value: T) {
        self.0[node_id.0] = Some(value)
    }

    pub fn delete(&mut self, node_id: NodeIndex) {
        self.0[node_id.0] = None
    }

    pub fn to_subset(&self) -> NodeSubset {
        NodeSubset(self.0.iter().map(Option::is_some).collect())
    }

    pub fn item_count(&self) -> usize {
        self.iter().count()
    }
}

impl<T: Encodable + Decodable + 'static> IntoIterator for NodeMap<T> {
    type Item = (NodeIndex, T);
    type IntoIter = Box<dyn Iterator<Item = (NodeIndex, T)>>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.into_iter())
    }
}

impl<'a, T: Encodable + Decodable + 'static> IntoIterator for &'a NodeMap<T> {
    type Item = (NodeIndex, &'a T);
    type IntoIter = Box<dyn Iterator<Item = (NodeIndex, &'a T)> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl<'a, T: Encodable + Decodable> IntoIterator for &'a mut NodeMap<T> {
    type Item = (NodeIndex, &'a mut T);
    type IntoIter = Box<dyn Iterator<Item = (NodeIndex, &'a mut T)> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter_mut())
    }
}

impl<T: Encodable + Decodable + fmt::Display> fmt::Display for NodeMap<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "[")?;
        let mut it = self.iter().peekable();
        while let Some((id, item)) = it.next() {
            write!(f, "({}, {})", id.0, item)?;
            if it.peek().is_some() {
                write!(f, ", ")?;
            }
        }
        write!(f, "]")?;
        Ok(())
    }
}

#[derive(Clone, Eq, PartialEq, Hash, Debug, Default)]
pub struct NodeSubset(bit_vec::BitVec<u32>);

impl NodeSubset {
    pub fn with_size(capacity: NodeCount) -> Self {
        NodeSubset(bit_vec::BitVec::from_elem(capacity.0, false))
    }

    pub fn insert(&mut self, i: NodeIndex) {
        self.0.set(i.0, true);
    }

    pub fn size(&self) -> usize {
        self.0.len()
    }

    pub fn elements(&self) -> impl Iterator<Item = NodeIndex> + '_ {
        self.0
            .iter()
            .enumerate()
            .filter_map(|(i, b)| if b { Some(i.into()) } else { None })
    }

    pub fn complement(&self) -> NodeSubset {
        let mut result = self.0.clone();
        result.negate();
        NodeSubset(result)
    }

    pub fn len(&self) -> usize {
        self.elements().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Encodable for NodeSubset {
    fn consensus_encode<W: Write>(&self, w: &mut W) -> io::Result<()> {
        (self.0.len() as u32).consensus_encode(w)?;
        self.0.to_bytes().consensus_encode(w)?;
        Ok(())
    }
}

impl Decodable for NodeSubset {
    fn consensus_decode_partial<R: Read>(r: &mut R) -> io::Result<Self> {
        let capacity = u32::consensus_decode_partial(r)? as usize;
        let bytes = Vec::<u8>::consensus_decode_partial(r)?;
        let mut bv = bit_vec::BitVec::from_bytes(&bytes);
        if bv.len() != 8 * capacity.div_ceil(8) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Length of bitvector inconsistent with encoded capacity.",
            ));
        }
        while bv.len() > capacity {
            if bv.pop() != Some(false) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Non-canonical encoding. Trailing bits should be all 0.",
                ));
            }
        }
        bv.truncate(capacity);
        Ok(NodeSubset(bv))
    }
}

impl StdIndex<NodeIndex> for NodeSubset {
    type Output = bool;

    fn index(&self, vidx: NodeIndex) -> &bool {
        &self.0[vidx.0]
    }
}

impl fmt::Display for NodeSubset {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut v: Vec<usize> = self.elements().map(|n| n.into()).collect();
        v.sort();
        write!(f, "{:?}", v)
    }
}

#[cfg(test)]
mod tests {

    use crate::node::{NodeIndex, NodeSubset};
    use picomint_encoding::{Decodable, Encodable};

    #[test]
    fn decoding_node_index_works() {
        for i in 0..1000 {
            let node_index = NodeIndex(i);
            let encoded = node_index.consensus_encode_to_vec();
            let decoded = NodeIndex::consensus_decode(&encoded).unwrap();
            assert_eq!(node_index, decoded);
        }
    }

    #[test]
    fn bool_node_map_decoding_works() {
        for len in 0..12 {
            for mask in 0..(1 << len) {
                let mut bnm = NodeSubset::with_size(len.into());
                for i in 0..len {
                    if (1 << i) & mask != 0 {
                        bnm.insert(i.into());
                    }
                }
                let encoded = bnm.consensus_encode_to_vec();
                let decoded =
                    NodeSubset::consensus_decode(&encoded).expect("decode should work");
                assert_eq!(decoded, bnm);
            }
        }
    }

    #[test]
    fn decoding_bool_node_map_works() {
        let bool_node_map = NodeSubset([true, false, true, true, true].iter().cloned().collect());
        let encoded = bool_node_map.consensus_encode_to_vec();
        let decoded = NodeSubset::consensus_decode(&encoded).expect("decode should work");
        assert_eq!(decoded, bool_node_map);
    }
}
