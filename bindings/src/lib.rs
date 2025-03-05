#![allow(clippy::missing_safety_doc)]

mod conversion;
mod monitoring;

use conversion::*;
use monitoring::init_logger;

use napi::{bindgen_prelude::*, threadsafe_function::*};
use napi_derive::napi;
use std::{path::PathBuf, sync::Arc};

type FlashCallback = ThreadsafeFunction<FlashEvent, Unknown, FlashEvent, false>;
type FlasherCallbackHandler = Arc<dyn Fn(flashthing::Event) + Send + Sync>;

#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct FlashThingOptions {
  pub log_level_directive: Option<String>,
}

// The main FlashThing class
#[napi]
pub struct FlashThing {
  callback: FlasherCallbackHandler,
  flasher: Option<flashthing::Flasher>,
  num_steps: usize,
}

#[napi]
impl FlashThing {
  #[napi(
    constructor,
    ts_args_type = "callback: (event: FlashEvent) => void, options?: FlashThingOptions"
  )]
  pub fn new(callback: Function<FlashEvent>, options: Option<FlashThingOptions>) -> Result<Self> {
    let (tsfn, callback) = create_callback(callback)?;
    init_logger(tsfn, options.unwrap_or_default().log_level_directive);

    Ok(Self {
      callback,

      flasher: None,
      num_steps: 0,
    })
  }

  #[napi]
  pub async unsafe fn open_directory(&mut self, path: String) -> Result<()> {
    let path_buf = PathBuf::from(path);
    match flashthing::Flasher::from_directory(path_buf, Some(self.callback.clone())) {
      Ok(flasher) => {
        self.num_steps = flasher.num_steps();
        self.flasher = Some(flasher);
        Ok(())
      }
      Err(e) => Err(Error::from_reason(format!("Failed to create flasher: {}", e))),
    }
  }

  #[napi]
  pub async unsafe fn open_archive(&mut self, path: String) -> Result<()> {
    let path_buf = PathBuf::from(path);
    match flashthing::Flasher::from_archive(path_buf, Some(self.callback.clone())) {
      Ok(flasher) => {
        self.num_steps = flasher.num_steps();
        self.flasher = Some(flasher);
        Ok(())
      }
      Err(e) => Err(Error::from_reason(format!("Failed to create flasher: {}", e))),
    }
  }

  #[napi]
  pub async unsafe fn open_json(&mut self, json: String) -> Result<()> {
    match flashthing::Flasher::from_json(json, Some(self.callback.clone())) {
      Ok(flasher) => {
        self.num_steps = flasher.num_steps();
        self.flasher = Some(flasher);
        Ok(())
      }
      Err(e) => Err(Error::from_reason(format!("Failed to create flasher: {}", e))),
    }
  }

  #[napi]
  pub async unsafe fn open_stock_directory(&mut self, path: String) -> Result<()> {
    let path_buf = PathBuf::from(path);
    match flashthing::Flasher::from_stock_directory(path_buf, Some(self.callback.clone())) {
      Ok(flasher) => {
        self.num_steps = flasher.num_steps();
        self.flasher = Some(flasher);
        Ok(())
      }
      Err(e) => Err(Error::from_reason(format!("Failed to create flasher: {}", e))),
    }
  }

  #[napi]
  pub async unsafe fn open_stock_archive(&mut self, path: String) -> Result<()> {
    let path_buf = PathBuf::from(path);
    match flashthing::Flasher::from_stock_archive(path_buf, Some(self.callback.clone())) {
      Ok(flasher) => {
        self.num_steps = flasher.num_steps();
        self.flasher = Some(flasher);
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

    match flasher.flash() {
      Ok(_) => Ok(()),
      Err(e) => Err(Error::from_reason(format!("Flashing failed: {}", e))),
    }
  }

  /// Utility method to unbrick a device
  #[napi]
  pub async unsafe fn unbrick(&mut self) -> Result<()> {
    match flashthing::AmlogicSoC::init(Some(self.callback.clone())) {
      Ok(aml) => match aml.unbrick() {
        Ok(()) => Ok(()),
        Err(e) => Err(Error::from_reason(format!("Failed to unbrick: {}", e))),
      },
      Err(e) => Err(Error::from_reason(format!("Failed to initialize device: {}", e))),
    }
  }

  /// Generate udev rules for Linux systems
  #[napi]
  pub fn host_setup(&self) -> Result<()> {
    match flashthing::AmlogicSoC::host_setup() {
      Ok(()) => Ok(()),
      Err(e) => Err(Error::from_reason(format!("Failed to set up host: {}", e))),
    }
  }
}

fn create_callback(callback: Function<FlashEvent>) -> Result<(Arc<FlashCallback>, FlasherCallbackHandler)> {
  let tsfn = Arc::new(callback.build_threadsafe_function().build()?);

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
