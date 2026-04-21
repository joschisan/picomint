use std::pin::Pin;

/// Stream that is `Send` unless targeting WASM
pub type BoxStream<'a, T> = Pin<Box<dyn futures::Stream<Item = T> + 'a + Send>>;
