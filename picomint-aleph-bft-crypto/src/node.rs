use derive_more::From;
use picomint_core::{NumPeers, PeerId};
use picomint_encoding::{Decodable, Encodable};
use std::{collections::HashMap, fmt};

/// Indicates that an implementor has been assigned some index.
pub trait Index {
    fn index(&self) -> PeerId;
}

/// A container keeping items indexed by [`PeerId`].
#[derive(Clone, Eq, PartialEq, Hash, Debug, Default, Decodable, Encodable, From)]
pub struct NodeMap<T: Encodable + Decodable + 'static>(Vec<Option<T>>);

impl<T: Encodable + Decodable + 'static> NodeMap<T> {
    /// Constructs a new node map with a given length.
    pub fn with_size(len: NumPeers) -> Self
    where
        T: Clone,
    {
        let v = vec![None; len.total()];
        NodeMap(v)
    }

    pub fn from_hashmap(len: NumPeers, hashmap: HashMap<PeerId, T>) -> Self
    where
        T: Clone,
    {
        let v = vec![None; len.total()];
        let mut nm = NodeMap(v);
        for (id, item) in hashmap.into_iter() {
            nm.insert(id, item);
        }
        nm
    }

    pub fn size(&self) -> NumPeers {
        self.0.len().into()
    }

    pub fn iter(&self) -> impl Iterator<Item = (PeerId, &T)> {
        self.0
            .iter()
            .enumerate()
            .filter_map(|(idx, maybe_value)| Some((PeerId::from(idx as u8), maybe_value.as_ref()?)))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (PeerId, &mut T)> {
        self.0
            .iter_mut()
            .enumerate()
            .filter_map(|(idx, maybe_value)| Some((PeerId::from(idx as u8), maybe_value.as_mut()?)))
    }

    fn into_iter(self) -> impl Iterator<Item = (PeerId, T)>
    where
        T: 'static,
    {
        self.0
            .into_iter()
            .enumerate()
            .filter_map(|(idx, maybe_value)| Some((PeerId::from(idx as u8), maybe_value?)))
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

    pub fn get(&self, node_id: PeerId) -> Option<&T> {
        self.0[node_id.to_usize()].as_ref()
    }

    pub fn get_mut(&mut self, node_id: PeerId) -> Option<&mut T> {
        self.0[node_id.to_usize()].as_mut()
    }

    pub fn insert(&mut self, node_id: PeerId, value: T) {
        self.0[node_id.to_usize()] = Some(value)
    }

    pub fn delete(&mut self, node_id: PeerId) {
        self.0[node_id.to_usize()] = None
    }

    pub fn item_count(&self) -> usize {
        self.iter().count()
    }
}

impl<T: Encodable + Decodable + 'static> IntoIterator for NodeMap<T> {
    type Item = (PeerId, T);
    type IntoIter = Box<dyn Iterator<Item = (PeerId, T)>>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.into_iter())
    }
}

impl<'a, T: Encodable + Decodable + 'static> IntoIterator for &'a NodeMap<T> {
    type Item = (PeerId, &'a T);
    type IntoIter = Box<dyn Iterator<Item = (PeerId, &'a T)> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl<'a, T: Encodable + Decodable + 'static> IntoIterator for &'a mut NodeMap<T> {
    type Item = (PeerId, &'a mut T);
    type IntoIter = Box<dyn Iterator<Item = (PeerId, &'a mut T)> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter_mut())
    }
}

impl<T: Encodable + Decodable + fmt::Display> fmt::Display for NodeMap<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "[")?;
        let mut it = self.iter().peekable();
        while let Some((id, item)) = it.next() {
            write!(f, "({}, {})", id, item)?;
            if it.peek().is_some() {
                write!(f, ", ")?;
            }
        }
        write!(f, "]")?;
        Ok(())
    }
}
