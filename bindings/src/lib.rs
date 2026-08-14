#![allow(clippy::missing_safety_doc)]

mod conversion;
mod monitoring;

use std::{path::PathBuf, sync::Arc};

use conversion::*;
use monitoring::init_logger;
use napi::{bindgen_prelude::*, threadsafe_function::*};
use napi_derive::napi;

type FlashCallback = ThreadsafeFunction<FlashEvent, Unknown<'static>, FlashEvent, Status, false>;
type FlasherCallbackHandler = Arc<dyn Fn(flashthing::Event) + Send + Sync>;

#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct FlashThingOptions {
  pub log_level_directive: Option<String>,
  pub fastboot: Option<bool>,
  pub force_sparse: Option<bool>,
}

type AmlFlasher = flashthing::Flasher<flashthing::NativeUsb, flashthing::FlashMode>;
type FbFlasher = flashthing::FastbootFlasher<flashthing::NativeUsb, flashthing::FlashMode>;

enum Runner {
  Aml(AmlFlasher),
  Fastboot(FbFlasher),
}

impl Runner {
  fn num_steps(&self) -> usize {
    match self {
      Self::Aml(flasher) => flasher.num_steps(),
      Self::Fastboot(flasher) => flasher.num_steps(),
    }
  }

  async fn flash(&mut self) -> flashthing::Result<()> {
    match self {
      Self::Aml(flasher) => flasher.flash().await,
      Self::Fastboot(flasher) => flasher.flash().await,
    }
  }
}

#[napi]
pub struct FlashThing {
  callback: FlasherCallbackHandler,
  fastboot: bool,
  force_sparse: bool,
  flasher: Option<Runner>,
  num_steps: usize,
}

#[napi]
impl FlashThing {
  #[napi(
    constructor,
    ts_args_type = "callback: (event: FlashEvent) => void, options?: FlashThingOptions"
  )]
  pub fn new(callback: Function<FlashEvent, Unknown<'static>>, options: Option<FlashThingOptions>) -> Result<Self> {
    let (tsfn, callback) = create_callback(callback)?;
    let options = options.unwrap_or_default();
    let fastboot = options.fastboot.unwrap_or(false);
    let force_sparse = options.force_sparse.unwrap_or(false);
    init_logger(tsfn, options.log_level_directive);

    Ok(Self {
      callback,
      fastboot,
      force_sparse,

      flasher: None,
      num_steps: 0,
    })
  }

  /// Store a freshly built flasher and remember how many steps it will run
  fn adopt(&mut self, flasher: Runner) {
    self.num_steps = flasher.num_steps();
    self.flasher = Some(flasher);
  }

  #[napi]
  pub async unsafe fn open_directory(&mut self, path: String) -> Result<()> {
    let path_buf = PathBuf::from(path);
    let flasher = if self.fastboot {
      pollster::block_on(flashthing::FastbootFlasher::from_directory(
        path_buf,
        Some(self.callback.clone()),
      ))
      .map(|flasher| Runner::Fastboot(flasher.force_sparse(self.force_sparse)))
    } else {
      pollster::block_on(flashthing::Flasher::from_directory(
        path_buf,
        Some(self.callback.clone()),
      ))
      .map(Runner::Aml)
    };

    match flasher {
      Ok(flasher) => {
        self.adopt(flasher);
        Ok(())
      }
      Err(e) => Err(Error::from_reason(format!("Failed to create flasher: {}", e))),
    }
  }

  #[napi]
  pub async unsafe fn open_archive(&mut self, path: String) -> Result<()> {
    let path_buf = PathBuf::from(path);
    let flasher = if self.fastboot {
      pollster::block_on(flashthing::FastbootFlasher::from_archive(
        path_buf,
        Some(self.callback.clone()),
      ))
      .map(|flasher| Runner::Fastboot(flasher.force_sparse(self.force_sparse)))
    } else {
      pollster::block_on(flashthing::Flasher::from_archive(path_buf, Some(self.callback.clone()))).map(Runner::Aml)
    };

    match flasher {
      Ok(flasher) => {
        self.adopt(flasher);
        Ok(())
      }
      Err(e) => Err(Error::from_reason(format!("Failed to create flasher: {}", e))),
    }
  }

  #[napi]
  pub async unsafe fn open_json(&mut self, json: String) -> Result<()> {
    let flasher = if self.fastboot {
      pollster::block_on(flashthing::FastbootFlasher::from_json(
        json,
        Some(self.callback.clone()),
      ))
      .map(|flasher| Runner::Fastboot(flasher.force_sparse(self.force_sparse)))
    } else {
      pollster::block_on(flashthing::Flasher::from_json(json, Some(self.callback.clone()))).map(Runner::Aml)
    };

    match flasher {
      Ok(flasher) => {
        self.adopt(flasher);
        Ok(())
      }
      Err(e) => Err(Error::from_reason(format!("Failed to create flasher: {}", e))),
    }
  }

  #[napi]
  pub async unsafe fn open_stock_directory(&mut self, path: String) -> Result<()> {
    let path_buf = PathBuf::from(path);
    let flasher = if self.fastboot {
      pollster::block_on(flashthing::FastbootFlasher::from_stock_directory(
        path_buf,
        Some(self.callback.clone()),
      ))
      .map(|flasher| Runner::Fastboot(flasher.force_sparse(self.force_sparse)))
    } else {
      pollster::block_on(flashthing::Flasher::from_stock_directory(
        path_buf,
        Some(self.callback.clone()),
      ))
      .map(Runner::Aml)
    };

    match flasher {
      Ok(flasher) => {
        self.adopt(flasher);
        Ok(())
      }
      Err(e) => Err(Error::from_reason(format!("Failed to create flasher: {}", e))),
    }
  }

  #[napi]
  pub async unsafe fn open_stock_archive(&mut self, path: String) -> Result<()> {
    let path_buf = PathBuf::from(path);
    let flasher = if self.fastboot {
      pollster::block_on(flashthing::FastbootFlasher::from_stock_archive(
        path_buf,
        Some(self.callback.clone()),
      ))
      .map(|flasher| Runner::Fastboot(flasher.force_sparse(self.force_sparse)))
    } else {
      pollster::block_on(flashthing::Flasher::from_stock_archive(
        path_buf,
        Some(self.callback.clone()),
      ))
      .map(Runner::Aml)
    };

    match flasher {
      Ok(flasher) => {
        self.adopt(flasher);
        Ok(())
      }
      Err(e) => Err(Error::from_reason(format!("Failed to create flasher: {}", e))),
    }
  }

  /// Method to get total number of steps
  #[napi]
  pub fn get_num_steps(&self) -> u32 {
    self.num_steps as u32
  }

  ///  Method to flash with progress callback
  #[napi]
  pub async unsafe fn flash(&mut self) -> Result<()> {
    let Some(flasher) = &mut self.flasher else {
      return Err(Error::from_reason("Flasher is not initialized".to_string()));
    };

    match pollster::block_on(flasher.flash()) {
      Ok(()) => Ok(()),
      Err(e) => Err(Error::from_reason(format!("Flashing failed: {}", e))),
    }
  }

  /// Utility method to unbrick a device
  #[napi]
  pub async unsafe fn unbrick(&mut self) -> Result<()> {
    let result = if self.fastboot {
      pollster::block_on(flashthing::Fastboot::connect(Some(self.callback.clone())))
        .and_then(|fastboot| pollster::block_on(fastboot.unbrick()))
    } else {
      pollster::block_on(flashthing::AmlogicSoC::connect(Some(self.callback.clone())))
        .and_then(|aml| pollster::block_on(aml.unbrick()))
    };

    result.map_err(|e| Error::from_reason(format!("Failed to unbrick: {}", e)))
  }

  /// Run a single u-boot command over fastboot's `oem console` and return its console output.
  #[napi]
  pub async unsafe fn console(&mut self, command: String) -> Result<String> {
    pollster::block_on(flashthing::Fastboot::connect(Some(self.callback.clone())))
      .and_then(|fastboot| pollster::block_on(fastboot.console(&command)))
      .map_err(|e| Error::from_reason(format!("Console command failed: {}", e)))
  }

  /// Set up host for flashing (this currently only does anything on Linux)
  #[napi]
  pub fn host_setup(&self) -> Result<()> {
    match flashthing::host_setup() {
      Ok(()) => Ok(()),
      Err(e) => Err(Error::from_reason(format!("Failed to set up host: {}", e))),
    }
  }
}

fn create_callback(
  callback: Function<FlashEvent, Unknown<'static>>,
) -> Result<(Arc<FlashCallback>, FlasherCallbackHandler)> {
  let tsfn = Arc::new(callback.build_threadsafe_function().callee_handled::<false>().build()?);

  let callback = tsfn.clone();
  let callback = move |event: flashthing::Event| {
    let callback = callback.clone();

    match callback.call(event.into(), ThreadsafeFunctionCallMode::NonBlocking) {
      napi::Status::Ok => {}
      err => tracing::error!("Error calling callback: {}", err),
    }
  };

  Ok((tsfn, Arc::new(callback)))
}
