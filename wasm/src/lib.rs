//! WebUSB bindings for flashthing.

mod conversion;
mod monitoring;

use std::sync::Arc;

use flashthing::{AmlogicSoC, Flasher, JsStore, WebUsb, config::FlashConfig, payload::PayloadStore};
use wasm_bindgen::prelude::*;

type WebFlasher = Flasher<WebUsb, JsStore>;

fn to_js(error: flashthing::Error) -> JsError {
  JsError::new(&error.to_string())
}

#[wasm_bindgen]
pub struct FlashThing {
  callback: flashthing::Callback,
  request_device: js_sys::Function,
  store: JsStore,

  aml: Option<AmlogicSoC<WebUsb>>,
  flasher: Option<WebFlasher>,
  num_steps: usize,
}

#[wasm_bindgen]
impl FlashThing {
  #[wasm_bindgen(constructor)]
  pub fn new(
    request_device: js_sys::Function,
    read_all: js_sys::Function,
    open: js_sys::Function,
    on_event: js_sys::Function,
    log_level: Option<String>,
  ) -> Self {
    console_error_panic_hook::set_once();
    monitoring::init_logger(log_level);

    let callback: flashthing::Callback = Arc::new(move |event: flashthing::Event| {
      if let Err(err) = on_event.call1(&JsValue::NULL, &conversion::event(event)) {
        tracing::error!("Error calling callback: {:?}", err);
      }
    });

    Self {
      callback,
      request_device,
      store: JsStore::new(read_all, open),
      aml: None,
      flasher: None,
      num_steps: 0,
    }
  }

  pub async fn connect(&mut self) -> Result<(), JsError> {
    let transport = WebUsb::new(self.request_device.clone(), Some(self.callback.clone()));
    let aml = AmlogicSoC::init(
      transport,
      flashthing::BL2_BIN,
      flashthing::BOOTLOADER_BIN,
      Some(self.callback.clone()),
    )
    .await
    .map_err(to_js)?;

    self.aml = Some(aml);
    Ok(())
  }

  #[wasm_bindgen(js_name = openJson)]
  pub fn open_json(&mut self, meta: String) -> Result<(), JsError> {
    let aml = self
      .aml
      .clone()
      .ok_or_else(|| JsError::new("connect must be called before loading a config"))?;
    let config = FlashConfig::from_standalone(&meta).map_err(to_js)?;

    let flasher = Flasher::new(aml, self.store.clone(), config, Some(self.callback.clone()));
    self.num_steps = flasher.num_steps();
    self.flasher = Some(flasher);

    Ok(())
  }

  pub async fn flash(&mut self) -> Result<(), JsError> {
    let flasher = self
      .flasher
      .as_mut()
      .ok_or_else(|| JsError::new("Flasher is not initialized"))?;

    flasher.flash().await.map_err(to_js)
  }

  pub async fn unbrick(&mut self, path: String) -> Result<(), JsError> {
    let aml = self
      .aml
      .clone()
      .ok_or_else(|| JsError::new("connect must be called before unbricking"))?;

    let (size, mut source) = self.store.open(&path).await.map_err(to_js)?;
    aml.unbrick_from(source.as_mut(), size).await.map_err(to_js)
  }

  pub async fn bulkcmd(&mut self, command: String) -> Result<String, JsError> {
    let aml = self
      .aml
      .clone()
      .ok_or_else(|| JsError::new("connect must be called before sending commands"))?;

    aml.bulkcmd(&command).await.map_err(to_js)
  }

  #[wasm_bindgen(js_name = getNumSteps)]
  pub fn get_num_steps(&self) -> u32 {
    self.num_steps as u32
  }
}
