use std::{future::Future, time::Duration};

use crate::{DeviceMode, Result};

pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbTarget {
  Maskrom,
  Fastboot,
}

impl UsbTarget {
  pub fn ids(self) -> (u16, u16) {
    match self {
      Self::Maskrom => (crate::VENDOR_ID, crate::PRODUCT_ID),
      Self::Fastboot => (crate::VENDOR_ID_FASTBOOT, crate::PRODUCT_ID_FASTBOOT),
    }
  }
}

pub trait UsbTransport {
  fn control_out(
    &self,
    request: u8,
    value: u16,
    index: u16,
    data: &[u8],
    timeout: Duration,
  ) -> impl Future<Output = Result<usize>>;

  fn control_in(
    &self,
    request: u8,
    value: u16,
    index: u16,
    buf: &mut [u8],
    timeout: Duration,
  ) -> impl Future<Output = Result<usize>>;

  fn bulk_out(&self, data: &[u8], timeout: Duration) -> impl Future<Output = Result<usize>>;

  fn bulk_in(&self, buf: &mut [u8], timeout: Duration) -> impl Future<Output = Result<usize>>;

  fn mode(&self) -> impl Future<Output = DeviceMode>;

  fn acquire(&self, target: UsbTarget) -> impl Future<Output = Result<()>>;
}
