use std::{
  fs::File,
  io::{BufReader, Cursor, Read},
  path::PathBuf,
  thread::sleep,
  time::Duration,
};

use zip::ZipArchive;

use crate::{
  config::{
    BL2BootValue, DataOrFile, FlashConfig, FlashStep, ReadMemoryValue, RestorePartitionValue, RunValue, StringOrFile,
    ValidatePartitionSizeValue, WaitValue, WriteAMLCDataValue, WriteLargeMemoryValue, WriteSimpleMemoryValue,
  },
  partitions::SUPERBIRD_PARTITIONS,
  AmlogicSoC, Callback, Error, Event, Result, ADDR_TMP, TRANSFER_BLOCK_SIZE,
};

pub type Zip = ZipArchive<BufReader<File>>;

#[derive(Debug)]
pub enum FlashMode {
  /// string is a json string in `meta.json` format
  Standalone,
  /// path to a directory with a `meta.json` inside
  Directory(PathBuf),
  /// path to a zip archive with a `meta.json` at the top level
  Archive(ZipArchive<BufReader<File>>),
}

#[derive(Debug, Clone)]
pub struct FlashProgress {
  pub percent: f64,
  pub elapsed: f64,        // in ms
  pub eta: f64,            // in ms
  pub rate: f64,           // in kib/s
  pub avg_chunk_time: f64, // in ms
  pub avg_rate: f64,       // in kib/s
}

pub struct Flasher {
  aml: AmlogicSoC,
  mode: FlashMode,
  config: FlashConfig,

  step: usize,
  callback: Option<Callback>,
}

impl Flasher {
  /// Flash the Car Thing based on steps defined in `meta.json`
  pub fn flash(&mut self) -> Result<()> {
    tracing::info!("beginning flashing process!");

    // i hate clones like this but i need self to be mutable due to the zip
    let steps = self.config.steps.clone();
    for step in &steps {
      tracing::trace!("starting step: {:?}", step);

      self.step += 1;
      if let Some(callback) = &self.callback {
        callback(Event::Step(self.step, step.clone()));
      }

      let outcome = match step {
        FlashStep::Identify { variable } => self.identify(variable)?,
        FlashStep::Bulkcmd { value } => self.bulkcmd(value)?,
        FlashStep::BulkcmdStat { value, variable } => self.bulkcmd_stat(value, variable)?,
        FlashStep::Run { value } => self.run(value)?,
        FlashStep::WriteSimpleMemory { value } => self.write_simple_memory(value)?,
        FlashStep::WriteLargeMemory { value } => self.write_large_memory(value)?,
        FlashStep::ReadSimpleMemory { value, variable } => self.read_simple_memory(value, variable)?,
        FlashStep::ReadLargeMemory { value, variable } => self.read_large_memory(value, variable)?,
        FlashStep::GetBootAMLC { variable } => self.get_boot_amlc(variable)?,
        FlashStep::WriteAMLCData { value } => self.write_amlc_data(value)?,
        FlashStep::Bl2Boot { value } => self.bl2_boot(value)?,
        FlashStep::ValidatePartitionSize { value, variable } => self.validate_partition_size(value, variable)?,
        FlashStep::RestorePartition { value } => self.restore_partition(value)?,
        FlashStep::WriteEnv { value } => self.write_env(value)?,
        FlashStep::Log { value } => self.log(value)?,
        FlashStep::Wait { value } => self.wait(value)?,
      };

      match outcome {
        FlashOutcome::Normal => continue,
        _ => tracing::warn!("handling return values is currently not supported: {:?}", &outcome),
      }
    }

    self.callback = None;
    Ok(())
  }

  fn identify(&self, variable: &Option<String>) -> Result<FlashOutcome> {
    tracing::debug!("running identify with variable {:?}", variable);
    let start_time = std::time::Instant::now();
    let result = self.aml.identify();
    let elapsed = start_time.elapsed();
    tracing::trace!("identify completed in {:?}", elapsed);
    Ok(FlashOutcome::IdentifyResult(result?))
  }

  fn bulkcmd(&self, value: &str) -> Result<FlashOutcome> {
    tracing::debug!("running bulkcmd with value {:?}", value);
    let start_time = std::time::Instant::now();
    let result = self.aml.bulkcmd(value);
    let elapsed = start_time.elapsed();
    tracing::trace!("bulkcmd completed in {:?}", elapsed);
    result?;
    Ok(FlashOutcome::Normal)
  }

  fn bulkcmd_stat(&self, value: &str, variable: &Option<String>) -> Result<FlashOutcome> {
    tracing::debug!(
      "running bulkcmd_stat with value {:?} and variable {:?}",
      value,
      variable
    );
    let start_time = std::time::Instant::now();
    let result = self.aml.bulkcmd(value);
    let elapsed = start_time.elapsed();
    tracing::trace!("bulkcmd_stat completed in {:?}", elapsed);
    Ok(FlashOutcome::BulkcmdStatResult(result?))
  }

  fn run(&self, value: &RunValue) -> Result<FlashOutcome> {
    tracing::debug!("running run with value {:?}", value);
    let start_time = std::time::Instant::now();
    let result = self.aml.run(value.address, value.keep_power);
    let elapsed = start_time.elapsed();
    tracing::trace!("run completed in {:?}", elapsed);
    result?;
    Ok(FlashOutcome::Normal)
  }

  fn write_simple_memory(&mut self, value: &WriteSimpleMemoryValue) -> Result<FlashOutcome> {
    tracing::debug!("running write_simple_memory with value {:?}", value);
    let data = self.handle_data_or_file(&value.data)?;

    let start_time = std::time::Instant::now();
    let result = self.aml.write_simple_memory(value.address, &data);
    let elapsed = start_time.elapsed();
    tracing::trace!("write_simple_memory completed in {:?}", elapsed);

    result?;
    Ok(FlashOutcome::Normal)
  }

  fn write_large_memory(&mut self, value: &WriteLargeMemoryValue) -> Result<FlashOutcome> {
    tracing::debug!("running write_large_memory with value {:?}", value);
    let start_time = std::time::Instant::now();

    let (file_size, mut file) = handle_data_or_file_stream(&value.data, &mut self.mode)?;

    let caller_callback = self.callback.clone();
    let progress_callback = |progress: FlashProgress| {
      if let Some(callback) = &caller_callback {
        callback(Event::FlashProgress(progress.clone()));
      };
    };

    self.aml.write_large_memory_to_disk(
      value.address,
      &mut file,
      file_size,
      value.block_length,
      value.append_zeros.unwrap_or(true),
      progress_callback,
    )?;

    let elapsed = start_time.elapsed();
    tracing::trace!("write_large_memory completed in {:?}", elapsed);
    Ok(FlashOutcome::Normal)
  }

  fn read_simple_memory(&self, value: &ReadMemoryValue, variable: &Option<String>) -> Result<FlashOutcome> {
    tracing::debug!(
      "running read_simple_memory with value {:?} and variable {:?}",
      value,
      variable
    );
    let start_time = std::time::Instant::now();
    let result = self.aml.read_simple_memory(value.address, value.length);
    let elapsed = start_time.elapsed();
    tracing::trace!("read_simple_memory completed in {:?}", elapsed);
    result?;
    Ok(FlashOutcome::Normal)
  }

  fn read_large_memory(&self, value: &ReadMemoryValue, variable: &Option<String>) -> Result<FlashOutcome> {
    tracing::debug!(
      "running read_large_memory with value {:?} and variable {:?}",
      value,
      variable
    );
    let start_time = std::time::Instant::now();
    let result = self.aml.read_memory(value.address, value.length);
    let elapsed = start_time.elapsed();
    tracing::trace!("read_large_memory completed in {:?}", elapsed);
    result?;
    Ok(FlashOutcome::Normal)
  }

  fn get_boot_amlc(&self, variable: &Option<String>) -> Result<FlashOutcome> {
    tracing::debug!("running get_boot_amlc with variable {:?}", variable);
    let start_time = std::time::Instant::now();
    let result = self.aml.get_boot_amlc();
    let elapsed = start_time.elapsed();
    tracing::trace!("get_boot_amlc completed in {:?}", elapsed);
    result?;
    Ok(FlashOutcome::Normal)
  }

  fn write_amlc_data(&mut self, value: &WriteAMLCDataValue) -> Result<FlashOutcome> {
    tracing::debug!("running write_amlc_data with value {:?}", value);
    let data = self.handle_data_or_file(&value.data)?;

    let start_time = std::time::Instant::now();
    let result = self.aml.write_amlc_data_packet(value.seq, value.amlc_offset, &data);
    let elapsed = start_time.elapsed();
    tracing::trace!("write_amlc_data completed in {:?}", elapsed);

    result?;
    Ok(FlashOutcome::Normal)
  }

  fn bl2_boot(&mut self, value: &BL2BootValue) -> Result<FlashOutcome> {
    tracing::debug!("running bl2_boot with value {:?}", value);
    let bl2 = self.handle_data_or_file(&value.bl2)?;
    let bootloader = self.handle_data_or_file(&value.bootloader)?;

    let start_time = std::time::Instant::now();
    let result = self.aml.bl2_boot(Some(&bl2), Some(&bootloader));
    let elapsed = start_time.elapsed();
    tracing::trace!("bl2_boot completed in {:?}", elapsed);

    result?;
    Ok(FlashOutcome::Normal)
  }

  fn validate_partition_size(
    &self,
    value: &ValidatePartitionSizeValue,
    variable: &Option<String>,
  ) -> Result<FlashOutcome> {
    tracing::debug!(
      "running validate_partition_size with value {:?} and variable {:?}",
      value,
      variable
    );

    let part_name = &value.name;
    let part_info = match SUPERBIRD_PARTITIONS.get(part_name.as_str()) {
      Some(info) => info,
      None => {
        tracing::error!("Error: Invalid partition name: {}", part_name);
        return Ok(FlashOutcome::ValidatePartitionResult(None, None));
      }
    };

    match self.aml.validate_partition_size(part_name, part_info) {
      Ok(part_size) => {
        let part_offset = part_info.offset;
        Ok(FlashOutcome::ValidatePartitionResult(
          Some(part_size),
          Some(part_offset),
        ))
      }
      Err(_) => Ok(FlashOutcome::ValidatePartitionResult(None, None)),
    }
  }

  fn restore_partition(&mut self, value: &RestorePartitionValue) -> Result<FlashOutcome> {
    tracing::debug!("running restore_partition with value {:?}", value);

    let part_name = &value.name;
    let validate_result = match self.validate_partition_size(
      &ValidatePartitionSizeValue {
        name: part_name.clone(),
      },
      &None,
    )? {
      FlashOutcome::ValidatePartitionResult(size, offset) => (size, offset),
      _ => (None, None),
    };

    let (part_size, _) = match validate_result {
      (Some(size), Some(offset)) => (size, offset),
      _ => return Err(Error::InvalidOperation("Failed to validate partition size!".into())),
    };

    let (file_size, file_reader) = handle_data_or_file_stream(&value.data, &mut self.mode)?;

    let caller_callback = self.callback.clone();
    let progress_callback = |progress: FlashProgress| {
      if let Some(callback) = &caller_callback {
        callback(Event::FlashProgress(progress.clone()));
      };
    };

    self
      .aml
      .restore_partition(part_name, part_size, file_reader, file_size, progress_callback)?;

    Ok(FlashOutcome::Normal)
  }

  fn write_env(&mut self, value: &StringOrFile) -> Result<FlashOutcome> {
    tracing::debug!("running write_env with value {:?}", value);

    let env_data = self.handle_string_or_file(value)?;

    if !env_data.is_ascii() {
      return Err(Error::InvalidOperation("env data must be ascii".into()));
    }

    let env_data_bytes = env_data.as_bytes();
    let env_size = env_data_bytes.len();
    let start_time = std::time::Instant::now();

    tracing::debug!("initializing env subsystem");
    self.aml.bulkcmd("amlmmc env")?;

    tracing::debug!("sending env ({} bytes)", env_size);
    self
      .aml
      .write_large_memory(ADDR_TMP, env_data_bytes, TRANSFER_BLOCK_SIZE, true)?;

    self
      .aml
      .bulkcmd(&format!("env import -t {:#X} {:#X}", ADDR_TMP, env_size))?;

    let elapsed = start_time.elapsed();
    tracing::trace!("write_env completed in {:?}", elapsed);

    Ok(FlashOutcome::Normal)
  }

  fn log(&self, value: &str) -> Result<FlashOutcome> {
    tracing::debug!("running log with value {:?}", value);
    tracing::info!(">> {:?}", value);
    Ok(FlashOutcome::Normal)
  }

  fn wait(&self, value: &WaitValue) -> Result<FlashOutcome> {
    tracing::debug!("running wait with value {:?}", value);
    match value {
      WaitValue::UserInput { .. } => panic!("wait for user input is not supported!"),
      WaitValue::Time { time } => sleep(Duration::from_millis(*time)),
    }
    Ok(FlashOutcome::Normal)
  }

  fn handle_data_or_file(&mut self, data_or_file: &DataOrFile) -> Result<Vec<u8>> {
    tracing::debug!("handling data or file {:?}", data_or_file);
    match data_or_file {
      DataOrFile::Data(data) => Ok(data.to_owned()),
      DataOrFile::File(file) => match &mut self.mode {
        FlashMode::Standalone => {
          tracing::warn!("trying to read a file in standalone mode!!");
          let mut file = File::open(PathBuf::from(&file.file_path))?;
          let mut data = vec![];
          file.read_to_end(&mut data)?;
          Ok(data)
        }
        FlashMode::Directory(path) => {
          let path = path.join(&file.file_path);
          let mut file = File::open(path)?;
          let mut data = vec![];
          file.read_to_end(&mut data)?;
          Ok(data)
        }
        FlashMode::Archive(zip) => {
          tracing::warn!("reading whole file into memory! is this what you want??");
          let file_name = if file.file_path.starts_with("./") {
            file.file_path.replacen("./", "", 1)
          } else {
            file.file_path.clone()
          };
          let mut found = zip.by_name(&file_name)?;
          let mut data = vec![];
          found.read_to_end(&mut data)?;
          Ok(data)
        }
      },
    }
  }

  fn handle_string_or_file(&mut self, string_or_file: &StringOrFile) -> Result<String> {
    tracing::debug!("handling string or file {:?}", string_or_file);
    match string_or_file {
      StringOrFile::String(data) => Ok(data.clone()),
      StringOrFile::File(file) => match &mut self.mode {
        FlashMode::Standalone => {
          tracing::warn!("trying to read a string file in standalone mode");
          let path = PathBuf::from(&file.file_path);
          std::fs::read_to_string(path).map_err(Error::from)
        }
        FlashMode::Directory(base_path) => {
          let path = base_path.join(&file.file_path);
          std::fs::read_to_string(path).map_err(Error::from)
        }
        FlashMode::Archive(zip) => {
          let file_name = if file.file_path.starts_with("./") {
            file.file_path.replacen("./", "", 1)
          } else {
            file.file_path.clone()
          };
          let mut zip_file = zip.by_name(&file_name)?;
          let mut data = String::new();
          zip_file.read_to_string(&mut data)?;
          Ok(data)
        }
      },
    }
  }

  /// get the total number of steps in the flash config
  pub fn num_steps(&self) -> usize {
    self.config.steps.len()
  }

  /// get current step in the flashing process
  pub fn current_step(&self) -> usize {
    self.step + 1
  }

  /// Create a new Flasher where the flash files are relative to the `cwd`.
  /// `path` MUST be the path to a directory.
  ///
  /// NOTE: Car Thing is expected to be plugged in at time of creation.
  ///
  /// # Parameters
  /// - `path`: [PathBuf] path to a directory
  pub fn from_directory(path: PathBuf, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new flasher from directory at {:?}", &path);

    Ok(Self {
      config: FlashConfig::from_directory(&path)?,
      mode: FlashMode::Directory(path),
      aml: AmlogicSoC::init(callback.clone())?,
      step: 0,
      callback,
    })
  }

  /// Create a new Flasher where the zip archive is relative to the `cwd`.
  /// `path` MUST be the path to a zip archive.
  ///
  /// NOTE: Car Thing is expected to be plugged in at time of creation.
  ///
  /// # Parameters
  /// - `path`: [PathBuf] path to the zip archive
  pub fn from_archive(path: PathBuf, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new flasher from archive at {:?}", &path);

    if !path.exists() || !path.is_file() {
      return Err(Error::NotFound);
    }

    let reader = BufReader::new(File::open(&path)?);
    let mut zip = ZipArchive::new(reader)?;

    Ok(Self {
      config: FlashConfig::from_archive(&mut zip)?,
      mode: FlashMode::Archive(zip),
      aml: AmlogicSoC::init(callback.clone())?,
      step: 0,
      callback,
    })
  }

  /// Create a new Flasher from a standalone `meta.json`.
  /// This type of flasher will attempt to access files relative to cwd.
  ///
  /// NOTE: Car Thing is expected to be plugged in at time of creation.
  ///
  /// # Parameters
  /// - `meta`: [String] stringified json
  pub fn from_json(meta: String, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new flasher from json string {:?}", &meta);

    Ok(Self {
      mode: FlashMode::Standalone,
      config: FlashConfig::from_standalone(&meta)?,
      aml: AmlogicSoC::init(callback.clone())?,
      step: 0,
      callback,
    })
  }

  /// Create a new Flasher where the flash files are relative to the `cwd`.
  /// `path` MUST be the path to a directory. This can only be used for stock flashing.
  ///
  /// NOTE: Car Thing is expected to be plugged in at time of creation.
  ///
  /// # Parameters
  /// - `path`: [PathBuf] path to a directory
  pub fn from_stock_directory(path: PathBuf, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new flasher from directory at {:?}", &path);

    Ok(Self {
      config: FlashConfig::from_stock()?,
      mode: FlashMode::Directory(path),
      aml: AmlogicSoC::init(callback.clone())?,
      step: 0,
      callback,
    })
  }

  /// Create a new Flasher where the zip archive is relative to the `cwd`.
  /// `path` MUST be the path to a zip archive. This can only be used for stock flashing.
  ///
  /// NOTE: Car Thing is expected to be plugged in at time of creation.
  ///
  /// # Parameters
  /// - `path`: [PathBuf] path to the zip archive
  pub fn from_stock_archive(path: PathBuf, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new flasher from archive at {:?}", &path);

    if !path.exists() || !path.is_file() {
      return Err(Error::NotFound);
    }

    let reader = BufReader::new(File::open(&path)?);
    let zip = ZipArchive::new(reader)?;

    Ok(Self {
      config: FlashConfig::from_stock()?,
      mode: FlashMode::Archive(zip),
      aml: AmlogicSoC::init(callback.clone())?,
      step: 0,
      callback,
    })
  }
}

fn handle_data_or_file_stream<'a>(
  data_or_file: &'a DataOrFile,
  mode: &'a mut FlashMode,
) -> Result<(usize, Box<dyn Read + 'a>)> {
  tracing::debug!("handling data or file {:?}", data_or_file);
  match data_or_file {
    DataOrFile::Data(data) => Ok((data.len(), Box::new(Cursor::new(data)))),
    DataOrFile::File(file) => match mode {
      FlashMode::Standalone => {
        tracing::warn!("trying to read a file in standalone mode!!");
        let file_path = PathBuf::from(&file.file_path);
        let file = File::open(file_path)?;
        Ok((file.metadata()?.len() as usize, Box::new(BufReader::new(file))))
      }
      FlashMode::Directory(path) => {
        let file_path = path.join(&file.file_path);
        let file = File::open(file_path)?;
        Ok((file.metadata()?.len() as usize, Box::new(BufReader::new(file))))
      }
      FlashMode::Archive(zip) => {
        let file_name = if file.file_path.starts_with("./") {
          &file.file_path.replacen("./", "", 1)
        } else {
          &file.file_path
        };

        let file = zip.by_name(file_name)?;
        Ok((file.size() as usize, Box::new(file)))
      }
    },
  }
}

#[derive(Debug)]
#[allow(dead_code)] // this is for if i decide to support handing control back or variables
pub enum FlashOutcome {
  /// flash step completed normally, continue flash
  ///
  /// this outcome does not hand control flow back, so no need to handle it
  Normal,
  /// flash completed, all steps finished
  ///
  /// calling flasher.flash() now will do nothing
  Complete,
  /// wait for user input
  ///
  /// you should display message string until user input, then call flasher.flash() again to continue.
  AwaitUserInput(String),
  /// result of a bulkcmdStat
  ///
  /// you should handle this result, then call flasher.flash() again to continue.
  BulkcmdStatResult(String),
  /// result of a bytes read
  ///
  /// you should handle this result, then call flasher.flash() again to continue.
  ReadResult(Vec<u8>),
  /// result of an identify step
  ///
  /// you should handle this result, then call flasher.flash() again to continue.
  IdentifyResult(String),
  /// result of a get boot amlc step
  ///
  /// you should handle this result, then call flasher.flash() again to continue.
  GetBootAMLCResult(u32, u32),
  /// result of a validate partition size step
  ///
  /// you can ignore this since it is handled internally
  ValidatePartitionResult(Option<usize>, Option<usize>),
}
