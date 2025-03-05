use crate::{flash::Zip, Error, Result, STOCK_META, SUPPORTED_META_VERSION};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs::read_to_string, io::Read, path::PathBuf};

/// Configuration for the flashing process
///
/// This represents the entire flash configuration, including
/// metadata and the sequence of steps to execute.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FlashConfig {
  /// Name of the flash configuration
  pub name: String,
  /// Version of the flash configuration
  pub version: String,
  /// Description of what the flash configuration does
  pub description: String,
  /// Sequence of steps to execute during flashing
  pub steps: Vec<FlashStep>,
  /// Variables to store data between steps
  pub variables: Option<HashMap<String, usize>>,
  /// Version of the metadata format
  pub metadata_version: usize,
}

impl FlashConfig {
  /// Load a flash configuration from a directory
  ///
  /// # Parameters
  /// - `path`: Path to a directory containing a meta.json file
  ///
  /// # Returns
  /// - `Result<Self>`: The loaded configuration or an error
  pub fn from_directory(path: &PathBuf) -> Result<Self> {
    if !path.exists() || !path.is_dir() {
      return Err(Error::NotDir(path.to_owned()));
    }

    let meta = path.join("meta.json");
    if !meta.exists() || !meta.is_file() {
      return Err(Error::NoMeta(meta));
    }

    let json = read_to_string(meta)?;
    let this: FlashConfig = serde_json::from_str(&json)?;
    this.check_config_supported()?;
    Ok(this)
  }

  /// Load a flash configuration from a ZIP archive
  ///
  /// # Parameters
  /// - `zip`: ZIP archive containing a meta.json file
  ///
  /// # Returns
  /// - `Result<Self>`: The loaded configuration or an error
  pub fn from_archive(zip: &mut Zip) -> Result<Self> {
    let mut meta_file = zip.by_name("meta.json")?;

    let mut json = String::new();
    meta_file.read_to_string(&mut json)?;

    let this: FlashConfig = serde_json::from_str(&json)?;
    this.check_config_supported()?;
    Ok(this)
  }

  /// Parse a flash configuration from a JSON string
  ///
  /// # Parameters
  /// - `json`: JSON string in meta.json format
  ///
  /// # Returns
  /// - `Result<Self>`: The parsed configuration or an error
  pub fn from_standalone(json: &str) -> Result<Self> {
    let this: FlashConfig = serde_json::from_str(json)?;
    this.check_config_supported()?;
    Ok(this)
  }

  /// Load the built-in stock flash configuration
  ///
  /// # Returns
  /// - `Result<Self>`: The stock configuration or an error
  pub fn from_stock() -> Result<Self> {
    let this: FlashConfig = serde_json::from_slice(STOCK_META)?;
    this.check_config_supported()?;
    Ok(this)
  }

  fn check_config_supported(&self) -> Result<()> {
    if self.metadata_version != SUPPORTED_META_VERSION {
      return Err(Error::UnsupportedVersion(self.metadata_version));
    }

    for step in &self.steps {
      match step {
        FlashStep::Identify { .. }
        | FlashStep::ReadLargeMemory { .. }
        | FlashStep::ReadSimpleMemory { .. }
        | FlashStep::GetBootAMLC { .. }
        | FlashStep::BulkcmdStat { .. }
        | FlashStep::ValidatePartitionSize { .. } => return Err(Error::UnsupportedFeature(step.to_owned())),
        FlashStep::Wait { value } => match value {
          WaitValue::UserInput { .. } => return Err(Error::UnsupportedFeature(step.to_owned())),
          WaitValue::Time { .. } => continue,
        },
        _ => continue,
      }
    }

    Ok(())
  }
}

/// Reference to a file in the flash package
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MetaFile {
  /// Path to the file
  pub file_path: String,
  /// Optional encoding for text files
  pub encoding: Option<String>,
}

/// Data that can be either inline or from a file
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum DataOrFile {
  /// Inline binary data
  Data(Vec<u8>),
  /// Reference to a file containing the data
  File(MetaFile),
}

/// String that can be either inline or from a file
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum StringOrFile {
  /// Inline string
  String(String),
  /// Reference to a file containing the string
  File(MetaFile),
}

/// A step in the flashing process
///
/// Each step represents a specific operation to perform during flashing.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FlashStep {
  /// Identify the device
  Identify {
    /// Variable to store the result
    variable: Option<String>,
  },
  /// Send a bulk command
  Bulkcmd {
    /// Command to send
    value: String,
  },
  /// Send a bulk command and get the status
  BulkcmdStat {
    /// Command to send
    value: String,
    /// Variable to store the result
    variable: Option<String>,
  },
  /// Run code at an address
  Run {
    /// Run parameters
    value: RunValue,
  },
  /// Write a small amount of data to memory
  WriteSimpleMemory {
    /// Write parameters
    value: WriteSimpleMemoryValue,
  },
  /// Write a large amount of data to memory
  WriteLargeMemory {
    /// Write parameters
    value: WriteLargeMemoryValue,
  },
  /// Read a small amount of data from memory
  ReadSimpleMemory {
    /// Read parameters
    value: ReadMemoryValue,
    /// Variable to store the result
    variable: Option<String>,
  },
  /// Read a large amount of data from memory
  ReadLargeMemory {
    /// Read parameters
    value: ReadMemoryValue,
    /// Variable to store the result
    variable: Option<String>,
  },
  /// Get AMLC boot information
  GetBootAMLC {
    /// Variable to store the result
    variable: Option<String>,
  },
  /// Write AMLC data
  WriteAMLCData {
    /// Write parameters
    value: WriteAMLCDataValue,
  },
  /// Boot using BL2 bootloader
  Bl2Boot {
    /// Boot parameters
    value: BL2BootValue,
  },
  /// Validate the size of a partition
  ValidatePartitionSize {
    /// Validation parameters
    value: ValidatePartitionSizeValue,
    /// Variable to store the result
    variable: Option<String>,
  },
  /// Restore a partition from backup
  RestorePartition {
    /// Restore parameters
    value: RestorePartitionValue,
  },
  /// Write to the U-Boot environment
  WriteEnv {
    /// Environment data
    value: StringOrFile,
  },
  /// Log a message
  Log {
    /// Message to log
    value: String,
  },
  /// Wait for a condition
  Wait {
    /// Wait parameters
    value: WaitValue,
  },
}

#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RunValue {
  pub address: u32,
  pub keep_power: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WriteSimpleMemoryValue {
  pub address: u32,
  pub data: DataOrFile,
}

#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WriteLargeMemoryValue {
  pub address: u32,
  pub data: DataOrFile,
  pub block_length: usize,
  pub append_zeros: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ReadMemoryValue {
  pub address: u32,
  pub length: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WriteAMLCDataValue {
  pub seq: u8,
  pub amlc_offset: u32,
  pub data: DataOrFile,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BL2BootValue {
  pub bl2: DataOrFile,
  pub bootloader: DataOrFile,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ValidatePartitionSizeValue {
  pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RestorePartitionValue {
  pub name: String,
  pub data: DataOrFile,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WaitValue {
  UserInput { message: String },
  Time { time: u64 },
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_nixos_superbird() {
    let json = r#"
        {
          "$schema": "/dev/null",
          "metadataVersion": 1,
          "name": "nixos-superbird",
          "version": "0.2.0",
          "description": "nixos superbird.",
          "steps": [
            {
              "type": "bulkcmd",
              "value": "amlmmc key"
            },
            {
              "type": "writeLargeMemory",
              "value": {
                "address": 0,
                "data": { "filePath": "./bootfs.bin" },
                "blockLength": 4096
              }
            },
            {
              "type": "writeLargeMemory",
              "value": {
                "address": 319488,
                "data": { "filePath": "./rootfs.img" },
                "blockLength": 4096
              }
            },
            {
              "type": "writeEnv",
              "value": { "filePath": "./env.txt" }
            },
            {
              "type": "bulkcmd",
              "value": "saveenv"
            }
          ]
        }
        "#;
    let config = FlashConfig::from_standalone(json).expect("Failed to parse nixos-superbird config");
    assert_eq!(config.name, "nixos-superbird");
    assert_eq!(config.version, "0.2.0");
    assert_eq!(config.steps.len(), 5);
  }

  #[test]
  #[should_panic]
  fn test_simple_firmware() {
    let json = r#"
        {
          "name": "Simple Firmware",
          "version": "1.0.0",
          "description": "This is an example Superbird flashing configuration file.",
          "steps": [
            {
              "type": "bulkcmd",
              "value": "amlmmc env"
            },
            {
              "type": "identify",
              "variable": "myIdentifyVar"
            },
            {
              "type": "log",
              "value": "My variable is ${myIdentifyVar}"
            }
          ],
          "metadataVersion": 1
        }
        "#;
    let config = FlashConfig::from_standalone(json).expect("Failed to parse Simple Firmware config");
    assert_eq!(config.name, "Simple Firmware");
    assert_eq!(config.version, "1.0.0");
    assert_eq!(config.steps.len(), 3);
  }

  #[test]
  #[should_panic]
  fn test_kitchen_sink() {
    let json = r#"
        {
          "name": "Example Superbird flashing configuration",
          "version": "1.0.0",
          "description": "This is an example Superbird flashing configuration file.",
          "steps": [
            {
              "type": "identify"
            },
            {
              "type": "bulkcmd",
              "value": "echo \"Hello World!\""
            },
            {
              "type": "run",
              "value": {
                "address": 268435456,
                "keepPower": true
              }
            },
            {
              "type": "writeSimpleMemory",
              "value": {
                "address": 268435456,
                "data": { "filePath": "path/to/file.bin" }
              }
            },
            {
              "type": "readSimpleMemory",
              "value": {
                "address": 268435456,
                "length": 1024
              },
              "variable": "readData"
            },
            {
              "type": "readLargeMemory",
              "value": {
                "address": 268435456,
                "length": 1024
              },
              "variable": "readData"
            },
            {
              "type": "getBootAMLC",
              "variable": "bootAMLC"
            },
            {
              "type": "writeAMLCData",
              "value": {
                "seq": 0,
                "amlcOffset": 268435456,
                "data": { "filePath": "path/to/file.bin" }
              }
            },
            {
              "type": "bl2Boot",
              "value": {
                "bl2": { "filePath": "path/to/bl2.bin" },
                "bootloader": { "filePath": "path/to/bootloader.bin" }
              }
            },
            {
              "type": "validatePartitionSize",
              "value": {
                "name": "bootloader"
              }
            },
            {
              "type": "restorePartition",
              "value": {
                "name": "bootloader",
                "data": { "filePath": "path/to/bootloader.bin" }
              }
            }
          ],
          "variables": {
            "readData": 0
          },
          "metadataVersion": 1
        }
        "#;
    let config = FlashConfig::from_standalone(json).expect("Failed to parse Example Superbird config");
    assert_eq!(config.name, "Example Superbird flashing configuration");
    assert_eq!(config.version, "1.0.0");
    assert_eq!(config.steps.len(), 11);
    let vars = config.variables.expect("Missing variables");
    assert_eq!(vars.get("readData"), Some(&0));
  }
}
