/// ! NAPI-rs really needs a better way to handle this
use napi_derive::napi;

use crate::monitoring::LogMessage;

// FlashProgress representation for JavaScript
#[napi(object)]
pub struct FlashProgress {
  /// percent complete
  pub percent: f64,
  /// elapsed time in milliseconds
  pub elapsed: f64,
  /// estimated flash time left in milliseconds
  pub eta: f64,
  /// rate in kib/s
  pub rate: f64,
  /// average chunk time in milliseconds
  pub avg_chunk_time: f64,
  /// average rate in kib/s
  pub avg_rate: f64,
}

impl From<flashthing::FlashProgress> for FlashProgress {
  fn from(progress: flashthing::FlashProgress) -> Self {
    Self {
      percent: progress.percent,
      elapsed: progress.elapsed,
      eta: progress.eta,
      rate: progress.rate,
      avg_chunk_time: progress.avg_chunk_time,
      avg_rate: progress.avg_rate,
    }
  }
}

#[napi(string_enum)]
pub enum DeviceMode {
  Normal,
  Usb,
  UsbBurn,
  NotFound,
}

impl From<flashthing::DeviceMode> for DeviceMode {
  fn from(mode: flashthing::DeviceMode) -> Self {
    match mode {
      flashthing::DeviceMode::Normal => Self::Normal,
      flashthing::DeviceMode::Usb => Self::Usb,
      flashthing::DeviceMode::UsbBurn => Self::UsbBurn,
      flashthing::DeviceMode::NotFound => Self::NotFound,
    }
  }
}

#[napi]
pub enum FlashEvent {
  /// log message
  Log { data: LogMessage },
  /// finding device
  FindingDevice,
  /// found device in mode
  DeviceMode { mode: DeviceMode },
  /// connecting to device
  Connecting,
  /// connected to device
  Connected,
  /// bl2 boot
  Bl2Boot,
  /// resetting
  Resetting,
  /// moved to step; this means previous step is over
  StepChanged { step: i32, data: FlashStep },
  /// percent complete with current step (for long-running steps)
  FlashInfo { data: FlashProgress },
}

impl From<flashthing::Event> for FlashEvent {
  fn from(event: flashthing::Event) -> Self {
    match event {
      flashthing::Event::FindingDevice => Self::FindingDevice,
      flashthing::Event::DeviceMode(device_mode) => Self::DeviceMode {
        mode: device_mode.into(),
      },
      flashthing::Event::Connecting => Self::Connecting,
      flashthing::Event::Connected => Self::Connected,
      flashthing::Event::Bl2Boot => Self::Bl2Boot,
      flashthing::Event::Resetting => Self::Resetting,
      flashthing::Event::Step(step_number, step_data) => Self::StepChanged {
        step: step_number as i32,
        data: step_data.into(),
      },
      flashthing::Event::FlashProgress(flash_progress) => Self::FlashInfo {
        data: flash_progress.into(),
      },
    }
  }
}

#[napi(object)]
pub struct MetaFile {
  pub file_path: String,
  pub encoding: Option<String>,
}

impl From<flashthing::config::MetaFile> for MetaFile {
  fn from(meta: flashthing::config::MetaFile) -> Self {
    Self {
      file_path: meta.file_path,
      encoding: meta.encoding,
    }
  }
}

#[napi]
pub enum DataOrFile {
  Data,
  File { file: MetaFile },
}

impl From<flashthing::config::DataOrFile> for DataOrFile {
  fn from(data_or_file: flashthing::config::DataOrFile) -> Self {
    match data_or_file {
      flashthing::config::DataOrFile::Data(_) => Self::Data,
      flashthing::config::DataOrFile::File(file) => Self::File { file: file.into() },
    }
  }
}

#[napi]
pub enum StringOrFile {
  String { string: String },
  File { file: MetaFile },
}

impl From<flashthing::config::StringOrFile> for StringOrFile {
  fn from(string_or_file: flashthing::config::StringOrFile) -> Self {
    match string_or_file {
      flashthing::config::StringOrFile::String(string) => Self::String { string },
      flashthing::config::StringOrFile::File(file) => Self::File { file: file.into() },
    }
  }
}

#[napi]
pub enum FlashStep {
  Identify {
    variable: Option<String>,
  },
  Bulkcmd {
    value: String,
  },
  BulkcmdStat {
    value: String,
    variable: Option<String>,
  },
  Run {
    value: RunValue,
  },
  WriteSimpleMemory {
    value: WriteSimpleMemoryValue,
  },
  WriteLargeMemory {
    value: WriteLargeMemoryValue,
  },
  ReadSimpleMemory {
    value: ReadMemoryValue,
    variable: Option<String>,
  },
  ReadLargeMemory {
    value: ReadMemoryValue,
    variable: Option<String>,
  },
  GetBootAmlc {
    variable: Option<String>,
  },
  WriteAmlcData {
    value: WriteAmlcDataValue,
  },
  Bl2Boot {
    value: Bl2BootValue,
  },
  ValidatePartitionSize {
    value: ValidatePartitionSizeValue,
    variable: Option<String>,
  },
  RestorePartition {
    value: RestorePartitionValue,
  },
  WriteEnv {
    value: StringOrFile,
  },
  Log {
    value: String,
  },
  Wait {
    value: WaitValue,
  },
}

impl From<flashthing::config::FlashStep> for FlashStep {
  fn from(step: flashthing::config::FlashStep) -> Self {
    match step {
      flashthing::config::FlashStep::Identify { variable } => Self::Identify { variable },
      flashthing::config::FlashStep::Bulkcmd { value } => Self::Bulkcmd { value },
      flashthing::config::FlashStep::BulkcmdStat { value, variable } => Self::BulkcmdStat { value, variable },
      flashthing::config::FlashStep::Run { value } => Self::Run { value: value.into() },
      flashthing::config::FlashStep::WriteSimpleMemory { value } => Self::WriteSimpleMemory { value: value.into() },
      flashthing::config::FlashStep::WriteLargeMemory { value } => Self::WriteLargeMemory { value: value.into() },
      flashthing::config::FlashStep::ReadSimpleMemory { value, variable } => Self::ReadSimpleMemory {
        value: value.into(),
        variable,
      },
      flashthing::config::FlashStep::ReadLargeMemory { value, variable } => Self::ReadLargeMemory {
        value: value.into(),
        variable,
      },
      flashthing::config::FlashStep::GetBootAMLC { variable } => Self::GetBootAmlc { variable },
      flashthing::config::FlashStep::WriteAMLCData { value } => Self::WriteAmlcData { value: value.into() },
      flashthing::config::FlashStep::Bl2Boot { value } => Self::Bl2Boot { value: value.into() },
      flashthing::config::FlashStep::ValidatePartitionSize { value, variable } => Self::ValidatePartitionSize {
        value: value.into(),
        variable,
      },
      flashthing::config::FlashStep::RestorePartition { value } => Self::RestorePartition { value: value.into() },
      flashthing::config::FlashStep::WriteEnv { value } => Self::WriteEnv { value: value.into() },
      flashthing::config::FlashStep::Log { value } => Self::Log { value },
      flashthing::config::FlashStep::Wait { value } => Self::Wait { value: value.into() },
    }
  }
}

#[napi(object)]
pub struct RunValue {
  pub address: u32,
  pub keep_power: Option<bool>,
}

impl From<flashthing::config::RunValue> for RunValue {
  fn from(value: flashthing::config::RunValue) -> Self {
    Self {
      address: value.address,
      keep_power: value.keep_power,
    }
  }
}

#[napi(object)]
pub struct WriteSimpleMemoryValue {
  pub address: u32,
  pub data: DataOrFile,
}

impl From<flashthing::config::WriteSimpleMemoryValue> for WriteSimpleMemoryValue {
  fn from(value: flashthing::config::WriteSimpleMemoryValue) -> Self {
    Self {
      address: value.address,
      data: value.data.into(),
    }
  }
}

#[napi(object)]
pub struct WriteLargeMemoryValue {
  pub address: u32,
  pub data: DataOrFile,
  pub block_length: u32,
  pub append_zeros: Option<bool>,
}

impl From<flashthing::config::WriteLargeMemoryValue> for WriteLargeMemoryValue {
  fn from(value: flashthing::config::WriteLargeMemoryValue) -> Self {
    Self {
      address: value.address,
      data: value.data.into(),
      block_length: value.block_length as u32,
      append_zeros: value.append_zeros,
    }
  }
}

#[napi(object)]
pub struct ReadMemoryValue {
  pub address: u32,
  pub length: u32,
}

impl From<flashthing::config::ReadMemoryValue> for ReadMemoryValue {
  fn from(value: flashthing::config::ReadMemoryValue) -> Self {
    Self {
      address: value.address,
      length: value.length as u32,
    }
  }
}

#[napi(object)]
pub struct WriteAmlcDataValue {
  pub seq: u8,
  pub amlc_offset: u32,
  pub data: DataOrFile,
}

impl From<flashthing::config::WriteAMLCDataValue> for WriteAmlcDataValue {
  fn from(value: flashthing::config::WriteAMLCDataValue) -> Self {
    Self {
      seq: value.seq,
      amlc_offset: value.amlc_offset,
      data: value.data.into(),
    }
  }
}

#[napi(object)]
pub struct Bl2BootValue {
  pub bl2: DataOrFile,
  pub bootloader: DataOrFile,
}

impl From<flashthing::config::BL2BootValue> for Bl2BootValue {
  fn from(value: flashthing::config::BL2BootValue) -> Self {
    Self {
      bl2: value.bl2.into(),
      bootloader: value.bootloader.into(),
    }
  }
}

#[napi(object)]
pub struct ValidatePartitionSizeValue {
  pub name: String,
}

impl From<flashthing::config::ValidatePartitionSizeValue> for ValidatePartitionSizeValue {
  fn from(value: flashthing::config::ValidatePartitionSizeValue) -> Self {
    Self { name: value.name }
  }
}

#[napi(object)]
pub struct RestorePartitionValue {
  pub name: String,
  pub data: DataOrFile,
}

impl From<flashthing::config::RestorePartitionValue> for RestorePartitionValue {
  fn from(value: flashthing::config::RestorePartitionValue) -> Self {
    Self {
      name: value.name,
      data: value.data.into(),
    }
  }
}

#[napi]
pub enum WaitValue {
  UserInput { message: String },
  Time { time: u32 },
}

impl From<flashthing::config::WaitValue> for WaitValue {
  fn from(value: flashthing::config::WaitValue) -> Self {
    match value {
      flashthing::config::WaitValue::UserInput { message } => Self::UserInput { message },
      flashthing::config::WaitValue::Time { time } => Self::Time { time: time as u32 },
    }
  }
}
