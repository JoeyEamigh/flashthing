use std::{
  fs::File,
  future::Future,
  io::{BufReader, Read},
  path::PathBuf,
  pin::Pin,
  sync::Mutex,
  time::Duration,
};

use rusb::{Context, DeviceHandle, Direction, UsbContext};
use zip::ZipArchive;

use crate::{
  Callback, DeviceMode, Error, Event, PRODUCT_ID, PRODUCT_ID_NORMAL, Result, VENDOR_ID, VENDOR_ID_NORMAL,
  payload::{BlockingSource, PayloadSource, PayloadStore},
  time,
  usb::{COMMAND_TIMEOUT, UsbTransport},
};

const INTERFACE_NUMBER: u8 = 0;

pub type Zip = ZipArchive<BufReader<File>>;

struct Claimed {
  handle: DeviceHandle<Context>,
  endpoint_in: u8,
  endpoint_out: u8,
}

/// [`UsbTransport`] over libusb.
pub struct NativeUsb {
  claimed: Mutex<Option<Claimed>>,
  callback: Option<Callback>,
}

impl NativeUsb {
  pub fn new(callback: Option<Callback>) -> Self {
    Self {
      claimed: Mutex::new(None),
      callback,
    }
  }

  fn with_claimed<T>(&self, op: impl FnOnce(&Claimed) -> Result<T>) -> Result<T> {
    let guard = self.claimed.lock().expect("usb handle mutex poisoned");
    let claimed = guard
      .as_ref()
      .ok_or_else(|| Error::InvalidOperation("device is not claimed".into()))?;
    op(claimed)
  }

  fn open_once(&self) -> Result<Claimed> {
    tracing::debug!("connecting to Amlogic device");
    if let Some(callback) = &self.callback {
      callback(Event::Connecting);
    };

    let context = Context::new()?;
    let handle = {
      let device = context
        .devices()?
        .iter()
        .find(|device| {
          if let Ok(desc) = device.device_descriptor() {
            desc.vendor_id() == VENDOR_ID && desc.product_id() == PRODUCT_ID
          } else {
            false
          }
        })
        .ok_or_else(|| Error::InvalidOperation("Device not found".into()))?;
      device.open()?
    };

    handle.set_active_configuration(1)?;
    handle.claim_interface(INTERFACE_NUMBER)?;

    let device = handle.device();
    let config_desc = device.active_config_descriptor()?;
    let interface = config_desc
      .interfaces()
      .find(|i| i.number() == INTERFACE_NUMBER)
      .ok_or_else(|| Error::InvalidOperation("Interface not found".into()))?;
    let descriptor = interface
      .descriptors()
      .next()
      .ok_or_else(|| Error::InvalidOperation("No alt setting".into()))?;
    let mut endpoint_in = None;
    let mut endpoint_out = None;
    for ep in descriptor.endpoint_descriptors() {
      match ep.direction() {
        Direction::In => endpoint_in = Some(ep.address()),
        Direction::Out => endpoint_out = Some(ep.address()),
      }
    }
    let endpoint_in = endpoint_in.ok_or_else(|| Error::InvalidOperation("IN endpoint not found".into()))?;
    let endpoint_out = endpoint_out.ok_or_else(|| Error::InvalidOperation("OUT endpoint not found".into()))?;
    tracing::info!("device connected, claiming interface {}", INTERFACE_NUMBER);
    if let Some(callback) = &self.callback {
      callback(Event::Connected);
    };

    Ok(Claimed {
      handle,
      endpoint_in,
      endpoint_out,
    })
  }

  fn release(&self) {
    let claimed = self.claimed.lock().expect("usb handle mutex poisoned").take();
    let Some(claimed) = claimed else { return };

    match claimed.handle.release_interface(INTERFACE_NUMBER) {
      Ok(()) => tracing::trace!("successfully dropped usb interface"),
      Err(err) => tracing::warn!("failed to release usb interface: {:?}", err),
    }
  }
}

impl UsbTransport for NativeUsb {
  async fn control_out(&self, request: u8, value: u16, index: u16, data: &[u8], timeout: Duration) -> Result<usize> {
    self.with_claimed(|claimed| {
      Ok(
        claimed
          .handle
          .write_control(0x40, request, value, index, data, timeout)?,
      )
    })
  }

  async fn control_in(&self, request: u8, value: u16, index: u16, buf: &mut [u8], timeout: Duration) -> Result<usize> {
    self.with_claimed(|claimed| Ok(claimed.handle.read_control(0xC0, request, value, index, buf, timeout)?))
  }

  async fn bulk_out(&self, data: &[u8], timeout: Duration) -> Result<usize> {
    self.with_claimed(|claimed| Ok(claimed.handle.write_bulk(claimed.endpoint_out, data, timeout)?))
  }

  async fn bulk_in(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
    self.with_claimed(|claimed| Ok(claimed.handle.read_bulk(claimed.endpoint_in, buf, timeout)?))
  }

  async fn mode(&self) -> DeviceMode {
    find_device()
  }

  async fn acquire(&self) -> Result<()> {
    self.release();

    let mut attempts = 0;
    while attempts < 3 {
      match self.open_once() {
        Ok(claimed) => {
          *self.claimed.lock().expect("usb handle mutex poisoned") = Some(claimed);
          return Ok(());
        }
        Err(e) => {
          tracing::debug!("failed to connect to device: {}. Attempt {}/3", e, attempts + 1);
          attempts += 1;
          time::sleep(Duration::from_secs(1)).await;
        }
      }
    }

    let claimed = self.open_once()?;
    *self.claimed.lock().expect("usb handle mutex poisoned") = Some(claimed);
    Ok(())
  }
}

impl Drop for NativeUsb {
  fn drop(&mut self) {
    self.release();
  }
}

#[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
fn find_device() -> DeviceMode {
  let context = match Context::new() {
    Ok(c) => c,
    Err(_) => return DeviceMode::NotFound,
  };
  let devices = match context.devices() {
    Ok(d) => d,
    Err(_) => return DeviceMode::NotFound,
  };
  for device in devices.iter() {
    let desc = match device.device_descriptor() {
      Ok(d) => d,
      Err(_) => continue,
    };
    if desc.vendor_id() == VENDOR_ID_NORMAL && desc.product_id() == PRODUCT_ID_NORMAL {
      tracing::debug!("Found device booted normally, with USB Gadget (adb/usbnet) enabled");
      return DeviceMode::Normal;
    }
    if desc.vendor_id() == VENDOR_ID && desc.product_id() == PRODUCT_ID {
      match device.open() {
        Ok(handle) => {
          let lang = handle.read_languages(COMMAND_TIMEOUT).unwrap_or_default();
          let Some(lang) = lang.first() else {
            tracing::debug!("Found device in USB Burn Mode (unable to read product string)");
            return DeviceMode::UsbBurn;
          };

          let prod = handle
            .read_product_string(*lang, &desc, Duration::from_millis(100))
            .ok();
          if prod.as_deref() == Some("GX-CHIP") {
            tracing::debug!("Found device booted in USB Mode (buttons 1 & 4 held at boot)");
            return DeviceMode::Usb;
          } else {
            tracing::debug!("Found device booted in USB Burn Mode (ready for commands)");
            return DeviceMode::UsbBurn;
          }
        }
        Err(_) => {
          tracing::debug!("Found device in USB Burn Mode (unable to read product string)");
          return DeviceMode::UsbBurn;
        }
      }
    }
  }

  tracing::debug!("No device found!");
  DeviceMode::NotFound
}

/// Where the flasher reads payload files from
#[derive(Debug)]
pub enum FlashMode {
  /// Using a standalone JSON string as configuration
  Standalone,
  /// Using files from a directory
  Directory(PathBuf),
  /// Using files from a ZIP archive
  Archive(Zip),
}

fn archive_name(path: &str) -> &str {
  path.strip_prefix("./").unwrap_or(path)
}

impl PayloadStore for FlashMode {
  fn read_all<'a>(&'a mut self, path: &'a str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + 'a>> {
    Box::pin(async move {
      match self {
        FlashMode::Standalone => {
          tracing::warn!("trying to read a file in standalone mode!!");
          let mut file = File::open(PathBuf::from(path))?;
          let mut data = vec![];
          file.read_to_end(&mut data)?;
          Ok(data)
        }
        FlashMode::Directory(base) => {
          let mut file = File::open(base.join(path))?;
          let mut data = vec![];
          file.read_to_end(&mut data)?;
          Ok(data)
        }
        FlashMode::Archive(zip) => {
          let mut found = zip.by_name(archive_name(path))?;
          let mut data = vec![];
          found.read_to_end(&mut data)?;
          Ok(data)
        }
      }
    })
  }

  fn open<'a>(
    &'a mut self,
    path: &'a str,
  ) -> Pin<Box<dyn Future<Output = Result<(usize, Box<dyn PayloadSource + 'a>)>> + 'a>> {
    Box::pin(async move {
      match self {
        FlashMode::Standalone => {
          tracing::warn!("trying to read a file in standalone mode!!");
          let file = File::open(PathBuf::from(path))?;
          let size = file.metadata()?.len() as usize;
          let source: Box<dyn PayloadSource + 'a> = Box::new(BlockingSource(BufReader::new(file)));
          Ok((size, source))
        }
        FlashMode::Directory(base) => {
          let file = File::open(base.join(path))?;
          let size = file.metadata()?.len() as usize;
          let source: Box<dyn PayloadSource + 'a> = Box::new(BlockingSource(BufReader::new(file)));
          Ok((size, source))
        }
        FlashMode::Archive(zip) => {
          let file = zip.by_name(archive_name(path))?;
          let size = file.size() as usize;
          let source: Box<dyn PayloadSource + 'a> = Box::new(BlockingSource(file));
          Ok((size, source))
        }
      }
    })
  }
}

#[cfg(test)]
mod tests {
  use std::{
    fs,
    io::{Cursor, Write},
  };

  use zip::{ZipWriter, write::SimpleFileOptions};

  use super::*;
  use crate::config::FlashConfig;

  const META: &str = r#"{
    "name": "archive fixture",
    "version": "0.0.1",
    "description": "exercises the archive payload store",
    "metadataVersion": 2,
    "steps": [{ "type": "writeUserArea", "value": { "lba": 0, "data": { "filePath": "./payload.bin" } } }]
  }"#;

  fn build_archive(payload: &[u8]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    writer.start_file("meta.json", options).unwrap();
    writer.write_all(META.as_bytes()).unwrap();
    writer.start_file("payload.bin", options).unwrap();
    writer.write_all(payload).unwrap();
    writer.finish().unwrap().into_inner()
  }

  fn write_temp(bytes: &[u8], name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(name);
    fs::write(&path, bytes).unwrap();
    path
  }

  #[test]
  fn archive_store_reads_meta_and_streams_payload() {
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let path = write_temp(&build_archive(&payload), "flashthing-archive-store.zip");

    let mut zip = ZipArchive::new(BufReader::new(File::open(&path).unwrap())).unwrap();
    let config = FlashConfig::from_archive(&mut zip).unwrap();
    assert_eq!(config.steps.len(), 1);

    let mut mode = FlashMode::Archive(zip);

    assert_eq!(pollster::block_on(mode.read_all("./payload.bin")).unwrap(), payload);
    assert_eq!(pollster::block_on(mode.read_all("payload.bin")).unwrap(), payload);

    let (size, mut source) = pollster::block_on(mode.open("./payload.bin")).unwrap();
    assert_eq!(size, payload.len());
    let mut got = vec![0u8; payload.len()];
    for chunk in got.chunks_mut(512) {
      pollster::block_on(source.read_exact(chunk)).unwrap();
    }
    drop(source);
    assert_eq!(got, payload);

    fs::remove_file(&path).unwrap();
  }

  #[test]
  fn archive_store_errors_on_missing_entry() {
    let path = write_temp(&build_archive(b"x"), "flashthing-archive-missing.zip");
    let zip = ZipArchive::new(BufReader::new(File::open(&path).unwrap())).unwrap();
    let mut mode = FlashMode::Archive(zip);

    assert!(pollster::block_on(mode.read_all("nope.bin")).is_err());

    fs::remove_file(&path).unwrap();
  }
}
