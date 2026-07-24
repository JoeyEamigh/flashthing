use std::{future::Future, time::Duration};

use crate::{DeviceMode, Result};

/// Timeout for every control transfer and for the descriptor reads done while probing a device.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// The USB surface the Amlogic protocol needs, independent of how the host reaches the device.
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

  /// Which mode the device is currently in, without claiming it.
  fn mode(&self) -> impl Future<Output = DeviceMode>;

  /// Drop any existing handle and claim the device again.
  fn acquire(&self) -> impl Future<Output = Result<()>>;
}
