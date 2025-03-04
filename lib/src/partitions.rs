//! Partitions for Superbird, extracted from output of: bulkcmd 'amlmmc part 1'

use lazy_static::lazy_static;
use std::collections::HashMap;

/// Information about a partition
#[derive(Debug, Clone)]
pub struct PartitionInfo {
  /// Offset in bytes
  pub offset: usize,
  /// Size in 512-byte sectors
  pub size: usize,
  /// Alternative size in 512-byte sectors (for data partition)
  pub size_alt: Option<usize>,
}

lazy_static! {
    /// Partition table for Superbird
    pub static ref SUPERBIRD_PARTITIONS: HashMap<&'static str, PartitionInfo> = {
        let mut m = HashMap::new();
        m.insert("bootloader", PartitionInfo {
            offset: 0,
            size: 4096,
            size_alt: None,
        });
        m.insert("reserved", PartitionInfo {
            offset: 73728,
            size: 131072,
            size_alt: None,
        });
        m.insert("cache", PartitionInfo {
            offset: 221184,
            size: 0,
            size_alt: None,
        });
        m.insert("env", PartitionInfo {
            offset: 237568,
            size: 16384,
            size_alt: None,
        });
        m.insert("fip_a", PartitionInfo {
            offset: 270336,
            size: 8192,
            size_alt: None,
        });
        m.insert("fip_b", PartitionInfo {
            offset: 294912,
            size: 8192,
            size_alt: None,
        });
        m.insert("logo", PartitionInfo {
            offset: 319488,
            size: 16384,
            size_alt: None,
        });
        m.insert("dtbo_a", PartitionInfo {
            offset: 352256,
            size: 8192,
            size_alt: None,
        });
        m.insert("dtbo_b", PartitionInfo {
            offset: 376832,
            size: 8192,
            size_alt: None,
        });
        m.insert("vbmeta_a", PartitionInfo {
            offset: 401408,
            size: 2048,
            size_alt: None,
        });
        m.insert("vbmeta_b", PartitionInfo {
            offset: 419840,
            size: 2048,
            size_alt: None,
        });
        m.insert("boot_a", PartitionInfo {
            offset: 438272,
            size: 32768,
            size_alt: None,
        });
        m.insert("boot_b", PartitionInfo {
            offset: 487424,
            size: 32768,
            size_alt: None,
        });
        m.insert("system_a", PartitionInfo {
            offset: 536576,
            size: 1056856,
            size_alt: None,
        });
        m.insert("system_b", PartitionInfo {
            offset: 1609816,
            size: 1056856,
            size_alt: None,
        });
        m.insert("misc", PartitionInfo {
            offset: 2683056,
            size: 16384,
            size_alt: None,
        });
        m.insert("settings", PartitionInfo {
            offset: 2715824,
            size: 524288,
            size_alt: None,
        });
        m.insert("data", PartitionInfo {
            offset: 3256496,
            size: 4476752,
            size_alt: Some(4378448),  // some devices have a smaller data partition
        });
        m
    };
}
