use std::io;
use std::marker::PhantomData;

use crate::{Decodable, Encodable};

/// A `T` carried as the `N` bytes it is encoded as, decoded only where it
/// is used. For a value whose decode does real work — a BLS point pays a
/// decompression and subgroup check, ~70 µs on G1 — this lets a stored
/// note be loaded, compared and hashed without verifying its signature
/// again. The bytes are bare, so this encodes exactly as the value it
/// stands in for. Equality and hashing are on the bytes, which is exact
/// because the consensus encoding is canonical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Undecoded<T, const N: usize> {
    bytes: [u8; N],
    decoded: PhantomData<T>,
}

impl<T: Decodable, const N: usize> Undecoded<T, N> {
    /// The value, with whatever checks its decode runs.
    pub fn decode(&self) -> io::Result<T> {
        T::consensus_decode(&self.bytes)
    }
}

impl<T: Encodable, const N: usize> From<T> for Undecoded<T, N> {
    fn from(value: T) -> Self {
        Self {
            bytes: value
                .consensus_encode_to_vec()
                .try_into()
                .expect("the value encodes to exactly N bytes"),
            decoded: PhantomData,
        }
    }
}

impl<T, const N: usize> Encodable for Undecoded<T, N> {
    fn consensus_encode<W: io::Write>(&self, w: &mut W) -> io::Result<()> {
        self.bytes.consensus_encode(w)
    }
}

impl<T, const N: usize> Decodable for Undecoded<T, N> {
    fn consensus_decode_partial<R: io::Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            bytes: <[u8; N]>::consensus_decode_partial(r)?,
            decoded: PhantomData,
        })
    }
}
