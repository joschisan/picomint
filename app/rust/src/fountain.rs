use flutter_rust_bridge::frb;
use picomint_client::mint::ECash;
use picomint_fountain::{FountainDecoder, FountainEncoder, Fragment};

use crate::ECashWrapper;

#[frb(opaque)]
pub struct ECashEncoder(FountainEncoder);

impl ECashEncoder {
    #[frb(sync)]
    pub fn new(ecash: &ECashWrapper) -> Self {
        Self(FountainEncoder::new(&ecash.0, 512))
    }

    #[frb]
    pub fn next_fragment(&mut self) -> String {
        picomint_base32::encode(&self.0.next_fragment())
    }
}

#[frb(opaque)]
pub struct ECashDecoder {
    inner: FountainDecoder<ECash>,
}

impl ECashDecoder {
    #[frb(sync)]
    pub fn new() -> Self {
        Self {
            inner: FountainDecoder::default(),
        }
    }

    #[frb(sync)]
    pub fn add_fragment(&mut self, fragment: &str) -> Option<ECashWrapper> {
        let decoded = picomint_base32::decode::<Fragment>(fragment).ok()?;

        self.inner.add_fragment(&decoded).map(ECashWrapper)
    }
}
