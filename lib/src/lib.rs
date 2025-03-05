mod aml;
mod flash;
mod partitions;
mod setup;

pub mod config;

use std::sync::Arc;

pub use aml::*;
pub use flash::{FlashProgress, Flasher};

use config::FlashStep;

type Callback = Arc<dyn Fn(Event) + Send + Sync>;
#[derive(Debug)]
pub enum Event {
  /// finding device
  FindingDevice,
  /// found device in mode
  DeviceMode(DeviceMode),
  /// connecting to device
  Connecting,
  /// connected to device
  Connected,
  /// bl2 boot
  Bl2Boot,
  /// resetting
  Resetting,
  /// moved to step; this means previous step is over
  Step(usize, FlashStep),
  /// percent complete with current step (for long-running steps)
  FlashProgress(FlashProgress),
}

pub type Result<T> = std::result::Result<T, Error>;
#[derive(thiserror::Error, Debug)]
pub enum Error {
  #[error("USB error: {0}")]
  UsbError(#[from] rusb::Error),
  #[error("IO error: {0}")]
  IoError(#[from] std::io::Error),
  #[error("slice conversion error: {0}")]
  Bytes(#[from] std::array::TryFromSliceError),
  #[error("Invalid operation: {0}")]
  InvalidOperation(String),
  #[error("UTF8 conversion error: {0}")]
  Utf8Error(#[from] std::string::FromUtf8Error),
  #[error("device not found!")]
  NotFound,
  #[error("device in wrong mode!")]
  WrongMode,
  #[error("bulkcmd failed: {0}")]
  BulkCmdFailed(String),
  #[error("unsupported `meta.json` version: {0}")]
  UnsupportedVersion(usize),
  #[error("unsupported `meta.json` feature: {:?}", 0)]
  UnsupportedFeature(config::FlashStep),
  #[error("failed to deserialize json: {0}")]
  Json(#[from] serde_json::Error),
  #[error("{0} is not a directory")]
  NotDir(std::path::PathBuf),
  #[error("could not find required `meta.json` at {0}")]
  NoMeta(std::path::PathBuf),
  #[error("required file does not exist at {0}")]
  FileMissing(std::path::PathBuf),
  #[error("zip error: {0}")]
  Zip(#[from] zip::result::ZipError),
}

const SUPPORTED_META_VERSION: usize = 1;

const BL2_BIN: &[u8] = include_bytes!("../resources/superbird.bl2.encrypted.bin");
const BOOTLOADER_BIN: &[u8] = include_bytes!("../resources/superbird.bootloader.img");
const UNBRICK_BIN_ZIP: &[u8] = include_bytes!("../resources/unbrick.bin.zip");
const STOCK_META: &[u8] = include_bytes!("../resources/stock-meta.json");

const VENDOR_ID: u16 = 0x1b8e;
const PRODUCT_ID: u16 = 0xc003;

#[allow(dead_code)]
const VENDOR_ID_BOOTED: u16 = 0x1d6b;
#[allow(dead_code)]
const PRODUCT_ID_BOOTED: u16 = 0x1014;

const ADDR_BL2: u32 = 0xfffa0000;
const TRANSFER_SIZE_THRESHOLD: usize = 8 * 1024 * 1024;
const ADDR_TMP: u32 = 0x1080000;

// all requests
const REQ_WRITE_MEM: u8 = 0x01;
const REQ_READ_MEM: u8 = 0x02;
#[allow(dead_code)]
const REQ_FILL_MEM: u8 = 0x03;
#[allow(dead_code)]
const REQ_MODIFY_MEM: u8 = 0x04;
const REQ_RUN_IN_ADDR: u8 = 0x05;
#[allow(dead_code)]
const REQ_WRITE_AUX: u8 = 0x06;
#[allow(dead_code)]
const REQ_READ_AUX: u8 = 0x07;

const REQ_WR_LARGE_MEM: u8 = 0x11;
#[allow(dead_code)]
const REQ_RD_LARGE_MEM: u8 = 0x12;
const REQ_IDENTIFY_HOST: u8 = 0x20;

#[allow(dead_code)]
const REQ_TPL_CMD: u8 = 0x30;
#[allow(dead_code)]
const REQ_TPL_STAT: u8 = 0x31;

#[allow(dead_code)]
const REQ_WRITE_MEDIA: u8 = 0x32;
#[allow(dead_code)]
const REQ_READ_MEDIA: u8 = 0x33;

const REQ_BULKCMD: u8 = 0x34;

#[allow(dead_code)]
const REQ_PASSWORD: u8 = 0x35;
#[allow(dead_code)]
const REQ_NOP: u8 = 0x36;

const REQ_GET_AMLC: u8 = 0x50;
const REQ_WRITE_AMLC: u8 = 0x60;

const FLAG_KEEP_POWER_ON: u32 = 0x10;

const AMLC_AMLS_BLOCK_LENGTH: usize = 0x200;
const AMLC_MAX_BLOCK_LENGTH: usize = 0x4000;
const AMLC_MAX_TRANSFER_LENGTH: usize = 65536;

#[allow(dead_code)]
const WRITE_MEDIA_CHEKSUM_ALG_NONE: u16 = 0x00ee;
#[allow(dead_code)]
const WRITE_MEDIA_CHEKSUM_ALG_ADDSUM: u16 = 0x00ef;
#[allow(dead_code)]
const WRITE_MEDIA_CHEKSUM_ALG_CRC32: u16 = 0x00f0;

// Constants for partition operations
const PART_SECTOR_SIZE: usize = 512; // bytes, size of sectors used in partition table
const TRANSFER_BLOCK_SIZE: usize = 8 * PART_SECTOR_SIZE; // 4KB data transferred into memory one block at a time
