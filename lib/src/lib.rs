//! # flashthing
//!
//! A Rust library for flashing custom firmware to the Spotify Car Thing device.
//!
//! This library provides a comprehensive toolset for interacting with the Amlogic
//! SoC on the Spotify Car Thing, allowing users to flash custom firmware, read/write
//! partitions, and execute other low-level operations on the device.
//!
//! ## Main Features
//!
//! - Device detection and mode switching
//! - Memory reading and writing
//! - Partition management and restoration
//! - Custom firmware flashing via JSON configuration
//! - Progress reporting and event callbacks
//! - Error handling and recovery (including unbricking)
//!
//! ## Usage Example
//!
//! ```no_run
//! use flashthing::{AmlogicSoC, Flasher, Event};
//! use std::{path::PathBuf, sync::Arc};
//!
//! // Set up USB access for the device (on Linux, but no-op for other OSes so fine to call)
//! AmlogicSoC::host_setup().unwrap();
//!
//! // Create a callback to handle events
//! let callback = Arc::new(|event: Event| {
//!     match event {
//!         Event::FlashProgress(progress) => {
//!             println!("Progress: {:.1}%, ETA: {:.1}s", progress.percent, progress.eta / 1000.0);
//!         },
//!         Event::Step(step_index, step) => {
//!             println!("Step {}: {:?}", step_index, step);
//!         },
//!         Event::DeviceMode(mode) => {
//!             println!("Device mode: {:?}", mode);
//!         },
//!         _ => {}
//!     }
//! });
//!
//! // Flash firmware from a directory
//! let mut flasher = Flasher::from_directory(
//!     PathBuf::from("/path/to/firmware"),
//!     Some(callback.clone())
//! ).unwrap();
//!
//! // Start the flashing process
//! flasher.flash().unwrap();
//! ```
//!
//! ## Device Connection
//!
//! To use this library, the Spotify Car Thing must be connected via USB and placed
//! in USB Mode by holding buttons 1 & 4 during power-on.
//!
//! ## Configuration Format
//!
//! The flashing process is guided by a `meta.json` file that specifies a sequence
//! of operations to perform. See the schema documentation for details on the format.

mod aml;
mod flash;
mod partitions;
mod setup;

/// Configuration types for the flashing process
pub mod config;

use std::sync::Arc;

pub use aml::*;
pub use flash::{FlashProgress, Flasher};

use config::FlashStep;

/// Callback type for receiving flash events
///
/// This is used to handle events during the flashing process, such as
/// progress updates, device connection status, and step transitions.
pub type Callback = Arc<dyn Fn(Event) + Send + Sync>;

/// Events emitted during the flashing process
///
/// These events are sent to the callback function to notify about
/// the progress and status of the flashing procedure.
#[derive(Debug)]
pub enum Event {
  /// Indicates the tool is searching for a connected device
  FindingDevice,
  /// Indicates the device was found and reports its current mode
  DeviceMode(DeviceMode),
  /// Indicates the tool is attempting to connect to the device
  Connecting,
  /// Indicates a successful connection to the device
  Connected,
  /// Indicates the BL2 boot process has started
  Bl2Boot,
  /// Indicates the device is being reset
  Resetting,
  /// Indicates movement to a new flashing step
  ///
  /// Parameters: (step_index, step_details)
  Step(usize, FlashStep),
  /// Provides progress information for the current flashing step
  FlashProgress(FlashProgress),
}

/// Result type used throughout the crate
pub type Result<T> = std::result::Result<T, Error>;

/// Error types that can occur during the flashing process
#[derive(thiserror::Error, Debug)]
pub enum Error {
  /// Error from the USB subsystem
  #[error("USB error: {0}")]
  UsbError(#[from] rusb::Error),

  /// I/O related error
  #[error("IO error: {0}")]
  IoError(#[from] std::io::Error),

  /// Error converting slices
  #[error("slice conversion error: {0}")]
  Bytes(#[from] std::array::TryFromSliceError),

  /// Error when an operation is invalid in the current context
  #[error("Invalid operation: {0}")]
  InvalidOperation(String),

  /// UTF-8 conversion error
  #[error("UTF8 conversion error: {0}")]
  Utf8Error(#[from] std::string::FromUtf8Error),

  /// Error when the device is not found
  #[error("device not found!")]
  NotFound,

  /// Error when the device is in an incompatible mode
  #[error("device in wrong mode!")]
  WrongMode,

  /// Error when a bulk command fails
  #[error("bulkcmd failed: {0}")]
  BulkCmdFailed(String),

  /// Error when the meta.json version is not supported
  #[error("unsupported `meta.json` version: {0}")]
  UnsupportedVersion(usize),

  /// Error when a feature in meta.json is not supported
  #[error("unsupported `meta.json` feature: {:?}", 0)]
  UnsupportedFeature(config::FlashStep),

  /// JSON deserialization error
  #[error("failed to deserialize json: {0}")]
  Json(#[from] serde_json::Error),

  /// Error when a path expected to be a directory is not
  #[error("{0} is not a directory")]
  NotDir(std::path::PathBuf),

  /// Error when the required meta.json file is not found
  #[error("could not find required `meta.json` at {0}")]
  NoMeta(std::path::PathBuf),

  /// Error when a required file is missing
  #[error("required file does not exist at {0}")]
  FileMissing(std::path::PathBuf),

  /// Zip archive error
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
