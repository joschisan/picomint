use std::io;
use std::marker::PhantomData;

use crate::{Decodable, Encodable};

/// A `T` carried as the bytes it is encoded as, decoded only where it is
/// used. For a value whose decode does real work — a BLS point pays a
/// decompression and subgroup check, ~70 µs on G1 — this lets a
/// transaction be parsed, hashed, compared and stored without touching
/// the values inside, and a stored note be loaded without verifying its
/// signature again. `B` is the byte container and sets the framing: a
/// `Vec<u8>` is length-prefixed, a `[u8; N]` is bare and so encodes
/// exactly as the value it stands in for. Equality and hashing are on the
/// bytes, which is exact because the consensus encoding is canonical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Undecoded<T, B = Vec<u8>> {
    bytes: B,
    decoded: PhantomData<T>,
}

impl<T: Decodable, B: AsRef<[u8]>> Undecoded<T, B> {
    /// The value, with whatever checks its decode runs.
    pub fn decode(&self) -> io::Result<T> {
        T::consensus_decode(self.bytes.as_ref())
    }
}

impl<T: Encodable, B: TryFrom<Vec<u8>>> From<T> for Undecoded<T, B> {
    fn from(value: T) -> Self {
        Self {
            bytes: value
                .consensus_encode_to_vec()
                .try_into()
                .ok()
                .expect("the value's encoding fits its container"),
            decoded: PhantomData,
        }
    }
}

impl<T, B: Encodable> Encodable for Undecoded<T, B> {
    fn consensus_encode<W: io::Write>(&self, w: &mut W) -> io::Result<()> {
        self.bytes.consensus_encode(w)
    }
}

impl<T, B: Decodable> Decodable for Undecoded<T, B> {
    fn consensus_decode_partial<R: io::Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            bytes: B::consensus_decode_partial(r)?,
            decoded: PhantomData,
        })
    }
}
