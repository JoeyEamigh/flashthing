use std::{sync::Arc, time::Duration};

use crate::{
  BL2_BIN, Callback, DeviceMode, Error, Event, FIP_BIN, Result,
  aml::{AmlogicSoC, RESET_SETTLE},
  time::sleep,
  usb::{COMMAND_TIMEOUT, UsbTarget, UsbTransport},
};

const PACKET_BYTES: usize = 64;
const TRANSFER_CHUNK_BYTES: usize = 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
const COMMIT_TIMEOUT: Duration = Duration::from_secs(300);

pub const FASTBOOT_BUF_ADDR: u32 = 0x6000000;
pub const MAX_COMMAND_BYTES: usize = 64;
pub const DEFAULT_MAX_DOWNLOAD_BYTES: usize = 0x7000000; // 112 MiB

#[derive(Debug, Clone)]
pub struct FastbootReply {
  pub ok: bool,
  pub info: Vec<String>,
  pub response: String,
}

impl FastbootReply {
  pub fn text(&self) -> String {
    self.info.join("\n")
  }
}

fn fastboot_error(command: &str, reason: impl Into<String>) -> Error {
  Error::Fastboot {
    command: command.to_owned(),
    reason: reason.into(),
  }
}

struct FastbootInner<U: UsbTransport> {
  soc: AmlogicSoC<U>,
  callback: Option<Callback>,
  max_download: std::sync::Mutex<Option<usize>>,
}

pub struct Fastboot<U: UsbTransport> {
  inner: Arc<FastbootInner<U>>,
}

impl<U: UsbTransport> Clone for Fastboot<U> {
  fn clone(&self) -> Self {
    Self {
      inner: self.inner.clone(),
    }
  }
}

impl<U: UsbTransport> Fastboot<U> {
  pub async fn init(transport: U, callback: Option<Callback>) -> Result<Self> {
    Self::init_with(transport, BL2_BIN, FIP_BIN, callback).await
  }

  pub async fn init_with(transport: U, bl2: &[u8], fip: &[u8], callback: Option<Callback>) -> Result<Self> {
    if let Some(callback) = &callback {
      callback(Event::FindingDevice);
    };

    let mode = transport.mode().await;
    if let Some(callback) = &callback {
      callback(Event::DeviceMode(mode));
    };

    match mode {
      DeviceMode::Fastboot => tracing::info!("device found in fastboot!"),
      DeviceMode::Usb => tracing::info!("device booted in usb mode - ram-booting mainline u-boot to reach fastboot"),
      DeviceMode::UsbBurn => {
        tracing::error!(
          "device is in vendor burn mode, which cannot be moved to fastboot. power cycle the car thing while holding \
           buttons 1 & 4 to get back to the mask rom"
        );
        return Err(Error::WrongMode);
      }
      DeviceMode::Normal => {
        tracing::error!(
          "device is booted in normal mode. make sure to power on the car thing while holding buttons 1 & 4"
        );
        return Err(Error::WrongMode);
      }
      DeviceMode::NotFound => {
        tracing::error!("device not found!! make sure to power on the car thing while holding buttons 1 & 4");
        return Err(Error::NotFound);
      }
    };

    let target = if mode == DeviceMode::Usb {
      UsbTarget::Maskrom
    } else {
      UsbTarget::Fastboot
    };
    transport.acquire(target).await?;

    let soc = AmlogicSoC::adopt(transport, callback.clone());

    if mode == DeviceMode::Usb {
      soc.bl2_stream(bl2, fip).await?;

      if let Some(callback) = &callback {
        callback(Event::Resetting);
      };
      tracing::debug!("u-boot is starting, waiting for the fastboot gadget to enumerate");
      sleep(RESET_SETTLE).await;
      soc.transport().acquire(UsbTarget::Fastboot).await?;
    }

    let device = Self {
      inner: Arc::new(FastbootInner {
        soc,
        callback,
        max_download: std::sync::Mutex::new(None),
      }),
    };

    match device.getvar("version-bootloader").await {
      Ok(version) => tracing::info!("connected to fastboot, bootloader {}", version.trim()),
      Err(err) => tracing::warn!("connected to fastboot but version-bootloader failed: {}", err),
    }

    Ok(device)
  }

  fn transport(&self) -> &U {
    self.inner.soc.transport()
  }

  fn emit(&self, event: Event) {
    if let Some(callback) = &self.inner.callback {
      callback(event);
    };
  }

  pub async fn send(&self, command: &str) -> Result<FastbootReply> {
    self.send_with_timeout(command, COMMAND_TIMEOUT).await
  }

  async fn send_with_timeout(&self, command: &str, timeout: Duration) -> Result<FastbootReply> {
    let bytes = command.as_bytes();
    if bytes.len() > MAX_COMMAND_BYTES {
      return Err(fastboot_error(
        command,
        format!(
          "command is {} bytes, over the {}-byte fastboot limit",
          bytes.len(),
          MAX_COMMAND_BYTES
        ),
      ));
    }

    tracing::debug!("fastboot >> {}", command);
    self.transport().bulk_out(bytes, COMMAND_TIMEOUT).await?;
    self.read_reply(command, timeout).await
  }

  pub async fn expect(&self, command: &str) -> Result<FastbootReply> {
    let reply = self.send(command).await?;
    if !reply.ok {
      return Err(fastboot_error(command, reply.response));
    }
    Ok(reply)
  }

  async fn read_reply(&self, command: &str, timeout: Duration) -> Result<FastbootReply> {
    let mut info: Vec<String> = Vec::new();
    let mut buf = [0u8; PACKET_BYTES];

    loop {
      let read = self.transport().bulk_in(&mut buf, timeout).await?;
      if read < 4 {
        return Err(fastboot_error(command, "device sent a truncated reply"));
      }

      let text = String::from_utf8_lossy(&buf[..read]);
      let (tag, body) = text.split_at(4);
      tracing::trace!("fastboot << {}{}", tag, body);

      match tag {
        "INFO" => info.push(body.to_owned()),
        "TEXT" => match info.last_mut() {
          Some(last) => last.push_str(body),
          None => info.push(body.to_owned()),
        },
        "OKAY" => {
          return Ok(FastbootReply {
            ok: true,
            info,
            response: body.to_owned(),
          });
        }
        "FAIL" => {
          return Ok(FastbootReply {
            ok: false,
            info,
            response: body.to_owned(),
          });
        }
        other => return Err(fastboot_error(command, format!("unexpected reply tag {:?}", other))),
      }
    }
  }

  pub async fn getvar(&self, name: &str) -> Result<String> {
    let command = format!("getvar:{}", name);
    Ok(self.expect(&command).await?.response)
  }

  pub async fn max_download_size(&self) -> usize {
    if let Some(cached) = *self.inner.max_download.lock().expect("max download mutex poisoned") {
      return cached;
    }

    let size = match self.getvar("max-download-size").await {
      Ok(raw) => parse_size(raw.trim()).unwrap_or(DEFAULT_MAX_DOWNLOAD_BYTES),
      Err(err) => {
        tracing::debug!("device did not report max-download-size ({}), assuming default", err);
        DEFAULT_MAX_DOWNLOAD_BYTES
      }
    };

    *self.inner.max_download.lock().expect("max download mutex poisoned") = Some(size);
    size
  }

  pub async fn download<F: Fn(usize, usize)>(&self, data: &[u8], on_progress: F) -> Result<()> {
    let size = data.len();
    if size == 0 {
      return Err(Error::InvalidOperation("refusing to download an empty image".into()));
    }

    let command = format!("download:{:08x}", size);
    tracing::debug!("fastboot >> {}", command);
    self.transport().bulk_out(command.as_bytes(), COMMAND_TIMEOUT).await?;

    let mut buf = [0u8; PACKET_BYTES];
    let read = self.transport().bulk_in(&mut buf, COMMAND_TIMEOUT).await?;
    if read < 4 {
      return Err(fastboot_error(&command, "device sent a truncated reply"));
    }
    let ack = String::from_utf8_lossy(&buf[..read]).into_owned();
    if !ack.starts_with("DATA") {
      return Err(fastboot_error(&command, ack[4.min(ack.len())..].to_owned()));
    }

    let accepted = usize::from_str_radix(ack[4..12.min(ack.len())].trim(), 16)
      .map_err(|_| fastboot_error(&command, format!("could not parse the DATA reply {:?}", ack)))?;
    if accepted != size {
      return Err(fastboot_error(
        &command,
        format!("device accepted {} bytes but we offered {}", accepted, size),
      ));
    }

    let mut sent = 0;
    while sent < size {
      let end = std::cmp::min(sent + TRANSFER_CHUNK_BYTES, size);
      self.transport().bulk_out(&data[sent..end], DOWNLOAD_TIMEOUT).await?;
      sent = end;
      on_progress(sent, size);
    }

    let reply = self.read_reply(&command, COMMAND_TIMEOUT).await?;
    if !reply.ok {
      return Err(fastboot_error(&command, reply.response));
    }
    Ok(())
  }

  pub async fn flash(&self, partition: &str) -> Result<()> {
    let command = format!("flash:{}", partition);
    let reply = self.send_with_timeout(&command, COMMIT_TIMEOUT).await?;
    if !reply.ok {
      return Err(fastboot_error(&command, reply.response));
    }
    Ok(())
  }

  pub async fn erase(&self, partition: &str) -> Result<()> {
    let command = format!("erase:{}", partition);
    let reply = self.send_with_timeout(&command, COMMIT_TIMEOUT).await?;
    if !reply.ok {
      return Err(fastboot_error(&command, reply.response));
    }
    Ok(())
  }

  pub async fn console(&self, command: &str) -> Result<String> {
    let full = format!("oem console {}", command);
    let reply = self.send_with_timeout(&full, COMMIT_TIMEOUT).await?;
    if !reply.ok {
      return Err(fastboot_error(&full, reply.response));
    }
    Ok(reply.text())
  }

  pub async fn drain_console(&self) -> Result<String> {
    Ok(self.send("oem console").await?.text())
  }

  pub async fn set_raw_target(&self, alias: &str, start_lba: u64, sector_count: u64) -> Result<()> {
    self
      .console(&format!(
        "setenv fastboot_raw_partition_{} {} {}",
        alias, start_lba, sector_count
      ))
      .await?;
    Ok(())
  }

  pub async fn clear_raw_target(&self, alias: &str) -> Result<()> {
    self
      .console(&format!("setenv fastboot_raw_partition_{}", alias))
      .await?;
    Ok(())
  }

  pub async fn select_hwpart(&self, hwpart: u8) -> Result<()> {
    if hwpart > 2 {
      return Err(Error::InvalidOperation(format!(
        "eMMC hwpart must be 0, 1 or 2, got {hwpart}"
      )));
    }
    self.console(&format!("mmc dev 0 {}", hwpart)).await?;
    Ok(())
  }

  pub async fn reboot(&self) -> Result<()> {
    self.expect("reboot").await?;
    self.emit(Event::Resetting);
    Ok(())
  }

  pub async fn reboot_bootloader(&self) -> Result<()> {
    self.expect("reboot-bootloader").await?;
    self.emit(Event::Resetting);
    Ok(())
  }

  pub async fn reboot_maskrom(&self) -> Result<()> {
    self.expect("oem maskrom").await?;
    self.emit(Event::Resetting);
    Ok(())
  }

  pub async fn continue_boot(&self) -> Result<()> {
    self.expect("continue").await?;
    Ok(())
  }

  pub async fn set_active_slot(&self, slot: &str) -> Result<()> {
    self.expect(&format!("set_active:{}", slot)).await?;
    Ok(())
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl Fastboot<crate::native::NativeUsb> {
  pub async fn connect(callback: Option<Callback>) -> Result<Self> {
    let transport = crate::native::NativeUsb::new(callback.clone());
    Self::init(transport, callback).await
  }

  pub async fn connect_with(bl2: &[u8], fip: &[u8], callback: Option<Callback>) -> Result<Self> {
    let transport = crate::native::NativeUsb::new(callback.clone());
    Self::init_with(transport, bl2, fip, callback).await
  }
}

#[cfg(target_arch = "wasm32")]
impl Fastboot<crate::web::WebUsb> {
  pub async fn connect(await_gesture: js_sys::Function, callback: Option<Callback>) -> Result<Self> {
    let transport = crate::web::WebUsb::new(await_gesture, callback.clone());
    Self::init(transport, callback).await
  }

  pub async fn connect_with(
    await_gesture: js_sys::Function,
    bl2: &[u8],
    fip: &[u8],
    callback: Option<Callback>,
  ) -> Result<Self> {
    let transport = crate::web::WebUsb::new(await_gesture, callback.clone());
    Self::init_with(transport, bl2, fip, callback).await
  }
}

fn parse_size(raw: &str) -> Option<usize> {
  let value = match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
    Some(hex) => usize::from_str_radix(hex, 16).ok()?,
    None => raw.parse().ok()?,
  };
  (value > 0).then_some(value)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_both_max_download_size_spellings() {
    assert_eq!(parse_size("0x7000000"), Some(0x7000000));
    assert_eq!(parse_size("0X7000000"), Some(0x7000000));
    assert_eq!(parse_size("117440512"), Some(117440512));
    assert_eq!(parse_size("0"), None);
    assert_eq!(parse_size("nonsense"), None);
  }
}
