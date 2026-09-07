//! BLS12-381 values. Every value the threshold schemes hand around is held
//! as the bytes it travels and is stored in — decoding it is a copy — and
//! decoded at most once, the first time it is used: a key at config load, a
//! note when it is verified. Decompressing a point with its subgroup check
//! costs ~70 µs on G1 and ~170 µs on G2, most of what decoding a note costs
//! and far more than the secp256k1 half of it. Equality and hashing are on
//! the bytes, which is exact because the compressed encoding is canonical.
//!
//! The macros name `bls12_381`, `serde` and `serdect` at their expansion
//! site, so this crate depends on none of them.

/// A wrapper over `[u8; $len]` decoded lazily into `$inner` by `$decode`,
/// built from an `$inner` by `$encode`, and reached through `$get`. The
/// using crate needs `serde` and `serdect`.
#[macro_export]
macro_rules! lazy_bytes {
    ($type:ident, $inner:ty, $len:literal, $get:ident, $decode:expr, $encode:expr) => {
        #[derive(Clone, Debug)]
        pub struct $type {
            bytes: [u8; $len],
            decoded: ::std::sync::OnceLock<Option<$inner>>,
        }

        impl $type {
            /// The decoded value, `None` if the bytes don't decode; decoded
            /// on the first call and kept.
            pub fn $get(&self) -> Option<&$inner> {
                let decode: fn(&[u8; $len]) -> Option<$inner> = $decode;

                self.decoded.get_or_init(|| decode(&self.bytes)).as_ref()
            }

            /// The bytes as they travel and are stored.
            pub fn as_bytes(&self) -> &[u8; $len] {
                &self.bytes
            }
        }

        impl From<$inner> for $type {
            fn from(value: $inner) -> Self {
                let encode: fn(&$inner) -> [u8; $len] = $encode;

                Self {
                    bytes: encode(&value),
                    decoded: ::std::sync::OnceLock::from(Some(value)),
                }
            }
        }

        impl PartialEq for $type {
            fn eq(&self, other: &Self) -> bool {
                self.bytes == other.bytes
            }
        }

        impl Eq for $type {}

        impl ::std::hash::Hash for $type {
            fn hash<H: ::std::hash::Hasher>(&self, state: &mut H) {
                self.bytes.hash(state);
            }
        }

        impl $crate::Encodable for $type {
            fn consensus_encode<W: ::std::io::Write>(&self, w: &mut W) -> ::std::io::Result<()> {
                self.bytes.consensus_encode(w)
            }
        }

        impl $crate::Decodable for $type {
            fn consensus_decode_partial<R: ::std::io::Read>(r: &mut R) -> ::std::io::Result<Self> {
                Ok(Self {
                    bytes: <[u8; $len]>::consensus_decode_partial(r)?,
                    decoded: ::std::sync::OnceLock::new(),
                })
            }
        }

        impl ::serde::Serialize for $type {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                ::serdect::array::serialize_hex_lower_or_bin(&self.bytes, s)
            }
        }

        impl<'d> ::serde::Deserialize<'d> for $type {
            fn deserialize<D: ::serde::Deserializer<'d>>(d: D) -> Result<Self, D::Error> {
                let mut bytes = [0; $len];

                ::serdect::array::deserialize_hex_or_bin(&mut bytes, d)?;

                Ok(Self {
                    bytes,
                    decoded: ::std::sync::OnceLock::new(),
                })
            }
        }
    };
}

/// A G1 point, 48 compressed bytes, decompressed with the subgroup check.
#[macro_export]
macro_rules! bls_g1 {
    ($type:ident) => {
        $crate::lazy_bytes!(
            $type,
            ::bls12_381::G1Affine,
            48,
            point,
            |bytes| ::bls12_381::G1Affine::from_compressed(bytes).into(),
            |point| point.to_compressed()
        );
    };
}

/// A G2 point, 96 compressed bytes, decompressed with the subgroup check.
#[macro_export]
macro_rules! bls_g2 {
    ($type:ident) => {
        $crate::lazy_bytes!(
            $type,
            ::bls12_381::G2Affine,
            96,
            point,
            |bytes| ::bls12_381::G2Affine::from_compressed(bytes).into(),
            |point| point.to_compressed()
        );
    };
}

/// A scalar, 32 bytes.
#[macro_export]
macro_rules! bls_scalar {
    ($type:ident) => {
        $crate::lazy_bytes!(
            $type,
            ::bls12_381::Scalar,
            32,
            scalar,
            |bytes| ::bls12_381::Scalar::from_bytes(bytes).into(),
            |scalar| scalar.to_bytes()
        );
    };
}
