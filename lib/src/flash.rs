use std::time::Duration;

use crate::{
  ADDR_TMP, AmlogicSoC, Callback, Error, Event, Result, TRANSFER_BLOCK_SIZE,
  config::{
    BL2BootValue, DataOrFile, FlashConfig, FlashStep, ReadMemoryValue, RestorePartitionValue, RunValue, StringOrFile,
    ValidatePartitionSizeValue, WaitValue, WriteAMLCDataValue, WriteBootPartitionValue, WriteLargeMemoryValue,
    WriteSimpleMemoryValue, WriteUserAreaValue,
  },
  partitions::SUPERBIRD_PARTITIONS,
  payload::{PayloadSource, PayloadStore, inline_source},
  time::{Instant, sleep},
  usb::UsbTransport,
};

/// Progress information for flashing operations
///
/// This provides detailed metrics about an ongoing flash operation.
#[derive(Debug, Clone)]
pub struct FlashProgress {
  /// Percent complete (0-100)
  pub percent: f64,
  /// Time elapsed in milliseconds
  pub elapsed: f64,
  /// Estimated time remaining in milliseconds
  pub eta: f64,
  /// Current transfer rate in KiB/s
  pub rate: f64,
  /// Average time per chunk in milliseconds
  pub avg_chunk_time: f64,
  /// Average transfer rate in KiB/s
  pub avg_rate: f64,
}

/// The main interface for flashing firmware to a Superbird device
///
/// This provides high-level operations for loading and flashing firmware
/// based on a configuration file.
pub struct Flasher<U: UsbTransport, S: PayloadStore> {
  aml: AmlogicSoC<U>,
  store: S,
  config: FlashConfig,

  step: usize,
  callback: Option<Callback>,
}

impl<U: UsbTransport, S: PayloadStore> Flasher<U, S> {
  /// Create a Flasher over an already connected device and a payload store
  ///
  /// # Parameters
  /// - `aml`: connected device
  /// - `store`: resolves the file references in `config`
  /// - `config`: the parsed and validated flash configuration
  /// - `callback`: Optional callback function to receive status updates
  pub fn new(aml: AmlogicSoC<U>, store: S, config: FlashConfig, callback: Option<Callback>) -> Self {
    Self {
      aml,
      store,
      config,
      step: 0,
      callback,
    }
  }

  /// Execute the flash process based on the loaded configuration
  ///
  /// This will run through all steps defined in the flash configuration.
  ///
  /// # Returns
  /// - `Result<()>`: Success or an error
  pub async fn flash(&mut self) -> Result<()> {
    tracing::info!("beginning flashing process!");

    // i hate clones like this but i need self to be mutable due to the store
    let steps = self.config.steps.clone();
    for step in &steps {
      let step_span = tracing::info_span!("step", ?step);
      let _step_span_enter = step_span.enter();

      self.step += 1;
      if let Some(callback) = &self.callback {
        callback(Event::Step(self.step, step.clone()));
      }

      let outcome = match step {
        FlashStep::Identify { variable } => self.identify(variable).await?,
        FlashStep::Bulkcmd { value } => self.bulkcmd(value).await?,
        FlashStep::BulkcmdStat { value, variable } => self.bulkcmd_stat(value, variable).await?,
        FlashStep::Run { value } => self.run(value).await?,
        FlashStep::WriteSimpleMemory { value } => self.write_simple_memory(value).await?,
        FlashStep::WriteLargeMemory { value } => self.write_large_memory(value).await?,
        FlashStep::ReadSimpleMemory { value, variable } => self.read_simple_memory(value, variable).await?,
        FlashStep::ReadLargeMemory { value, variable } => self.read_large_memory(value, variable).await?,
        FlashStep::GetBootAMLC { variable } => self.get_boot_amlc(variable).await?,
        FlashStep::WriteAMLCData { value } => self.write_amlc_data(value).await?,
        FlashStep::Bl2Boot { value } => self.bl2_boot(value).await?,
        FlashStep::ValidatePartitionSize { value, variable } => self.validate_partition_size(value, variable).await?,
        FlashStep::RestorePartition { value } => self.restore_partition(value).await?,
        FlashStep::WriteBootPartition { value } => self.write_boot_partition(value).await?,
        FlashStep::WriteUserArea { value } => self.write_user_area(value).await?,
        FlashStep::WriteEnv { value } => self.write_env(value).await?,
        FlashStep::Log { value } => self.log(value)?,
        FlashStep::Wait { value } => self.wait(value).await?,
      };

      match outcome {
        FlashOutcome::Normal => continue,
        _ => tracing::warn!("handling return values is currently not supported: {:?}", &outcome),
      }
    }

    self.callback = None;
    Ok(())
  }

  async fn identify(&self, variable: &Option<String>) -> Result<FlashOutcome> {
    tracing::debug!("running identify with variable {:?}", variable);
    let start_time = Instant::now();
    let result = self.aml.identify().await;
    let elapsed = start_time.elapsed();
    tracing::trace!("identify completed in {:?}", elapsed);
    Ok(FlashOutcome::IdentifyResult(result?))
  }

  async fn bulkcmd(&self, value: &str) -> Result<FlashOutcome> {
    tracing::debug!("running bulkcmd with value {:?}", value);
    let start_time = Instant::now();
    let result = self.aml.bulkcmd(value).await;
    let elapsed = start_time.elapsed();
    tracing::trace!("bulkcmd completed in {:?}", elapsed);
    result?;
    Ok(FlashOutcome::Normal)
  }

  async fn bulkcmd_stat(&self, value: &str, variable: &Option<String>) -> Result<FlashOutcome> {
    tracing::debug!(
      "running bulkcmd_stat with value {:?} and variable {:?}",
      value,
      variable
    );
    let start_time = Instant::now();
    let result = self.aml.bulkcmd(value).await;
    let elapsed = start_time.elapsed();
    tracing::trace!("bulkcmd_stat completed in {:?}", elapsed);
    Ok(FlashOutcome::BulkcmdStatResult(result?))
  }

  async fn run(&self, value: &RunValue) -> Result<FlashOutcome> {
    tracing::debug!("running run with value {:?}", value);
    let start_time = Instant::now();
    let result = self.aml.run(value.address, value.keep_power).await;
    let elapsed = start_time.elapsed();
    tracing::trace!("run completed in {:?}", elapsed);
    result?;
    Ok(FlashOutcome::Normal)
  }

  async fn write_simple_memory(&mut self, value: &WriteSimpleMemoryValue) -> Result<FlashOutcome> {
    tracing::debug!("running write_simple_memory with value {:?}", value);
    let data = read_payload(&value.data, &mut self.store).await?;

    let start_time = Instant::now();
    let result = self.aml.write_simple_memory(value.address, &data).await;
    let elapsed = start_time.elapsed();
    tracing::trace!("write_simple_memory completed in {:?}", elapsed);

    result?;
    Ok(FlashOutcome::Normal)
  }

  async fn write_large_memory(&mut self, value: &WriteLargeMemoryValue) -> Result<FlashOutcome> {
    tracing::debug!("running write_large_memory with value {:?}", value);
    let start_time = Instant::now();

    let (file_size, mut source) = open_payload(&value.data, &mut self.store).await?;

    let caller_callback = self.callback.clone();
    let progress_callback = |progress: FlashProgress| {
      if let Some(callback) = &caller_callback {
        callback(Event::FlashProgress(progress.clone()));
      };
    };

    self
      .aml
      .write_large_memory_to_disk(
        value.address,
        source.as_mut(),
        file_size,
        value.block_length,
        value.append_zeros.unwrap_or(true),
        progress_callback,
      )
      .await?;

    let elapsed = start_time.elapsed();
    tracing::trace!("write_large_memory completed in {:?}", elapsed);
    Ok(FlashOutcome::Normal)
  }

  async fn read_simple_memory(&self, value: &ReadMemoryValue, variable: &Option<String>) -> Result<FlashOutcome> {
    tracing::debug!(
      "running read_simple_memory with value {:?} and variable {:?}",
      value,
      variable
    );
    let start_time = Instant::now();
    let result = self.aml.read_simple_memory(value.address, value.length).await;
    let elapsed = start_time.elapsed();
    tracing::trace!("read_simple_memory completed in {:?}", elapsed);
    result?;
    Ok(FlashOutcome::Normal)
  }

  async fn read_large_memory(&self, value: &ReadMemoryValue, variable: &Option<String>) -> Result<FlashOutcome> {
    tracing::debug!(
      "running read_large_memory with value {:?} and variable {:?}",
      value,
      variable
    );
    let start_time = Instant::now();
    let result = self.aml.read_memory(value.address, value.length).await;
    let elapsed = start_time.elapsed();
    tracing::trace!("read_large_memory completed in {:?}", elapsed);
    result?;
    Ok(FlashOutcome::Normal)
  }

  async fn get_boot_amlc(&self, variable: &Option<String>) -> Result<FlashOutcome> {
    tracing::debug!("running get_boot_amlc with variable {:?}", variable);
    let start_time = Instant::now();
    let result = self.aml.get_boot_amlc().await;
    let elapsed = start_time.elapsed();
    tracing::trace!("get_boot_amlc completed in {:?}", elapsed);
    result?;
    Ok(FlashOutcome::Normal)
  }

  async fn write_amlc_data(&mut self, value: &WriteAMLCDataValue) -> Result<FlashOutcome> {
    tracing::debug!("running write_amlc_data with value {:?}", value);
    let data = read_payload(&value.data, &mut self.store).await?;

    let start_time = Instant::now();
    let result = self
      .aml
      .write_amlc_data_packet(value.seq, value.amlc_offset, &data)
      .await;
    let elapsed = start_time.elapsed();
    tracing::trace!("write_amlc_data completed in {:?}", elapsed);

    result?;
    Ok(FlashOutcome::Normal)
  }

  async fn bl2_boot(&mut self, value: &BL2BootValue) -> Result<FlashOutcome> {
    tracing::debug!("running bl2_boot with value {:?}", value);
    let bl2 = read_payload(&value.bl2, &mut self.store).await?;
    let bootloader = read_payload(&value.bootloader, &mut self.store).await?;

    let start_time = Instant::now();
    let result = self.aml.bl2_boot(&bl2, &bootloader).await;
    let elapsed = start_time.elapsed();
    tracing::trace!("bl2_boot completed in {:?}", elapsed);

    result?;
    Ok(FlashOutcome::Normal)
  }

  async fn validate_partition_size(
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

    match self.aml.validate_partition_size(part_name, part_info).await {
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

  async fn restore_partition(&mut self, value: &RestorePartitionValue) -> Result<FlashOutcome> {
    tracing::debug!("running restore_partition with value {:?}", value);

    let part_name = &value.name;
    let validate_result = match self
      .validate_partition_size(
        &ValidatePartitionSizeValue {
          name: part_name.clone(),
        },
        &None,
      )
      .await?
    {
      FlashOutcome::ValidatePartitionResult(size, offset) => (size, offset),
      _ => (None, None),
    };

    let (part_size, _) = match validate_result {
      (Some(size), Some(offset)) => (size, offset),
      _ => return Err(Error::InvalidOperation("Failed to validate partition size!".into())),
    };

    let (file_size, mut source) = open_payload(&value.data, &mut self.store).await?;

    let caller_callback = self.callback.clone();
    let progress_callback = |progress: FlashProgress| {
      if let Some(callback) = &caller_callback {
        callback(Event::FlashProgress(progress.clone()));
      };
    };

    self
      .aml
      .restore_partition(part_name, part_size, source.as_mut(), file_size, progress_callback)
      .await?;

    Ok(FlashOutcome::Normal)
  }

  async fn write_boot_partition(&mut self, value: &WriteBootPartitionValue) -> Result<FlashOutcome> {
    tracing::debug!("running write_boot_partition with value {:?}", value);
    let data = read_payload(&value.data, &mut self.store).await?;

    let start_time = Instant::now();
    self.aml.write_boot_partition(value.hwpart, &data).await?;
    tracing::trace!("write_boot_partition completed in {:?}", start_time.elapsed());

    Ok(FlashOutcome::Normal)
  }

  async fn write_user_area(&mut self, value: &WriteUserAreaValue) -> Result<FlashOutcome> {
    tracing::debug!("running write_user_area with value {:?}", value);
    let (file_size, mut source) = open_payload(&value.data, &mut self.store).await?;

    let caller_callback = self.callback.clone();
    let progress_callback = |progress: FlashProgress| {
      if let Some(callback) = &caller_callback {
        callback(Event::FlashProgress(progress.clone()));
      };
    };

    let start_time = Instant::now();
    self
      .aml
      .write_user_area(
        value.lba,
        source.as_mut(),
        file_size,
        value.sparse.unwrap_or(false),
        progress_callback,
      )
      .await?;
    tracing::trace!("write_user_area completed in {:?}", start_time.elapsed());

    Ok(FlashOutcome::Normal)
  }

  async fn write_env(&mut self, value: &StringOrFile) -> Result<FlashOutcome> {
    tracing::debug!("running write_env with value {:?}", value);

    let env_data = read_text(value, &mut self.store).await?;

    if !env_data.is_ascii() {
      return Err(Error::InvalidOperation("env data must be ascii".into()));
    }

    let env_data_bytes = env_data.as_bytes();
    let env_size = env_data_bytes.len();
    let start_time = Instant::now();

    tracing::debug!("initializing env subsystem");
    self.aml.bulkcmd("amlmmc env").await?;

    tracing::debug!("sending env ({} bytes)", env_size);
    self
      .aml
      .write_large_memory(ADDR_TMP, env_data_bytes, TRANSFER_BLOCK_SIZE, true)
      .await?;

    self
      .aml
      .bulkcmd(&format!("env import -t {:#X} {:#X}", ADDR_TMP, env_size))
      .await?;

    let elapsed = start_time.elapsed();
    tracing::trace!("write_env completed in {:?}", elapsed);

    Ok(FlashOutcome::Normal)
  }

  fn log(&self, value: &str) -> Result<FlashOutcome> {
    tracing::debug!("running log with value {:?}", value);
    tracing::info!(">> {:?}", value);
    Ok(FlashOutcome::Normal)
  }

  async fn wait(&self, value: &WaitValue) -> Result<FlashOutcome> {
    tracing::debug!("running wait with value {:?}", value);
    match value {
      WaitValue::UserInput { .. } => panic!("wait for user input is not supported!"),
      WaitValue::Time { time } => sleep(Duration::from_millis(*time)).await,
    }
    Ok(FlashOutcome::Normal)
  }

  /// get the total number of steps in the flash config
  pub fn num_steps(&self) -> usize {
    self.config.steps.len()
  }

  /// get current step in the flashing process
  pub fn current_step(&self) -> usize {
    self.step + 1
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl Flasher<crate::native::NativeUsb, crate::native::FlashMode> {
  /// Create a new Flasher where the flash files are relative to the `cwd`.
  /// `path` MUST be the path to a directory.
  ///
  /// NOTE: Car Thing is expected to be plugged in at time of creation.
  ///
  /// # Parameters
  /// - `path`: [std::path::PathBuf] path to a directory
  pub async fn from_directory(path: std::path::PathBuf, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new flasher from directory at {:?}", &path);

    let config = FlashConfig::from_directory(&path)?;
    let aml = AmlogicSoC::connect(callback.clone()).await?;

    Ok(Self::new(
      aml,
      crate::native::FlashMode::Directory(path),
      config,
      callback,
    ))
  }

  /// Create a new Flasher where the zip archive is relative to the `cwd`.
  /// `path` MUST be the path to a zip archive.
  ///
  /// NOTE: Car Thing is expected to be plugged in at time of creation.
  ///
  /// # Parameters
  /// - `path`: [std::path::PathBuf] path to the zip archive
  pub async fn from_archive(path: std::path::PathBuf, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new flasher from archive at {:?}", &path);

    let mut zip = open_zip(&path)?;
    let config = FlashConfig::from_archive(&mut zip)?;
    let aml = AmlogicSoC::connect(callback.clone()).await?;

    Ok(Self::new(aml, crate::native::FlashMode::Archive(zip), config, callback))
  }

  /// Create a new Flasher from a standalone `meta.json`.
  /// This type of flasher will attempt to access files relative to cwd.
  ///
  /// NOTE: Car Thing is expected to be plugged in at time of creation.
  ///
  /// # Parameters
  /// - `meta`: [String] stringified json
  pub async fn from_json(meta: String, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new flasher from json string {:?}", &meta);

    let config = FlashConfig::from_standalone(&meta)?;
    let aml = AmlogicSoC::connect(callback.clone()).await?;

    Ok(Self::new(aml, crate::native::FlashMode::Standalone, config, callback))
  }

  /// Create a new Flasher where the flash files are relative to the `cwd`.
  /// `path` MUST be the path to a directory. This can only be used for stock flashing.
  ///
  /// NOTE: Car Thing is expected to be plugged in at time of creation.
  ///
  /// # Parameters
  /// - `path`: [std::path::PathBuf] path to a directory
  pub async fn from_stock_directory(path: std::path::PathBuf, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new flasher from directory at {:?}", &path);

    let config = FlashConfig::from_stock()?;
    let aml = AmlogicSoC::connect(callback.clone()).await?;

    Ok(Self::new(
      aml,
      crate::native::FlashMode::Directory(path),
      config,
      callback,
    ))
  }

  /// Create a new Flasher where the zip archive is relative to the `cwd`.
  /// `path` MUST be the path to a zip archive. This can only be used for stock flashing.
  ///
  /// NOTE: Car Thing is expected to be plugged in at time of creation.
  ///
  /// # Parameters
  /// - `path`: [std::path::PathBuf] path to the zip archive
  pub async fn from_stock_archive(path: std::path::PathBuf, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new flasher from archive at {:?}", &path);

    let zip = open_zip(&path)?;
    let config = FlashConfig::from_stock()?;
    let aml = AmlogicSoC::connect(callback.clone()).await?;

    Ok(Self::new(aml, crate::native::FlashMode::Archive(zip), config, callback))
  }
}

#[cfg(not(target_arch = "wasm32"))]
fn open_zip(path: &std::path::Path) -> Result<crate::native::Zip> {
  if !path.exists() || !path.is_file() {
    return Err(Error::NotFound);
  }

  let reader = std::io::BufReader::new(std::fs::File::open(path)?);
  Ok(zip::ZipArchive::new(reader)?)
}

async fn read_payload<S: PayloadStore>(data_or_file: &DataOrFile, store: &mut S) -> Result<Vec<u8>> {
  tracing::debug!("handling data or file {:?}", data_or_file);
  match data_or_file {
    DataOrFile::Data(data) => Ok(data.to_owned()),
    DataOrFile::File(file) => store.read_all(&file.file_path).await,
  }
}

async fn read_text<S: PayloadStore>(string_or_file: &StringOrFile, store: &mut S) -> Result<String> {
  tracing::debug!("handling string or file {:?}", string_or_file);
  match string_or_file {
    StringOrFile::String(data) => Ok(data.clone()),
    StringOrFile::File(file) => Ok(String::from_utf8(store.read_all(&file.file_path).await?)?),
  }
}

async fn open_payload<'a, S: PayloadStore>(
  data_or_file: &'a DataOrFile,
  store: &'a mut S,
) -> Result<(usize, Box<dyn PayloadSource + 'a>)> {
  tracing::debug!("handling data or file {:?}", data_or_file);
  match data_or_file {
    DataOrFile::Data(data) => Ok((data.len(), inline_source(data))),
    DataOrFile::File(file) => store.open(&file.file_path).await,
  }
}

/// Result of a flash step execution
///
/// This represents the outcome of executing a single flash step.
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
