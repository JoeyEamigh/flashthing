use std::{future::Future, pin::Pin};

use crate::Result;

/// A forward-only source of payload bytes.
pub trait PayloadSource {
  fn read_exact<'a>(&'a mut self, buf: &'a mut [u8]) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;
}

/// Resolves the file references in a flash config to bytes.
pub trait PayloadStore {
  /// Read a whole file into memory. Only used for payloads small enough to hold at once.
  fn read_all<'a>(&'a mut self, path: &'a str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + 'a>>;

  /// Open a file for streaming, yielding its total length and a source positioned at its start.
  #[allow(clippy::type_complexity)]
  fn open<'a>(
    &'a mut self,
    path: &'a str,
  ) -> Pin<Box<dyn Future<Output = Result<(usize, Box<dyn PayloadSource + 'a>)>> + 'a>>;
}

/// Adapts any blocking reader into a [`PayloadSource`].
pub struct BlockingSource<R>(pub R);

impl<R: std::io::Read> PayloadSource for BlockingSource<R> {
  fn read_exact<'a>(&'a mut self, buf: &'a mut [u8]) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
    Box::pin(async move { self.0.read_exact(buf).map_err(crate::Error::from) })
  }
}

/// Wraps bytes inlined into a flash config so they stream like any other payload.
pub fn inline_source(data: &[u8]) -> Box<dyn PayloadSource + '_> {
  Box::new(BlockingSource(std::io::Cursor::new(data)))
}
