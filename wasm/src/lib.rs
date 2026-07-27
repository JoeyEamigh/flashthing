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

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &'static str = r#"
/** Why the browser is asking for a click before it will open the device chooser. */
export type GestureReason = 'initial' | 'reconnect';

export interface FlashThingOptions {
  /** reads a whole file out of the bundle */
  readAll: (path: string) => Promise<Uint8Array>;
  /** streams a file out of the bundle, so a multi-hundred-megabyte image never lands in memory at once */
  open: (path: string) => Promise<{ size: number; read: (length: number) => Promise<Uint8Array> }>;
  /**
   * Resolves once the user has clicked something. The chooser only opens while that click is still live, so resolve
   * straight out of the event handler rather than after any further awaits.
   */
  awaitGesture: (reason: GestureReason) => Promise<void>;
  logLevelDirective?: string;
}
"#;

#[wasm_bindgen]
extern "C" {
  #[wasm_bindgen(typescript_type = "FlashThingOptions")]
  pub type FlashThingOptions;

  #[wasm_bindgen(method, getter, js_name = readAll)]
  fn read_all(this: &FlashThingOptions) -> js_sys::Function;

  #[wasm_bindgen(method, getter)]
  fn open(this: &FlashThingOptions) -> js_sys::Function;

  #[wasm_bindgen(method, getter, js_name = awaitGesture)]
  fn await_gesture(this: &FlashThingOptions) -> js_sys::Function;

  #[wasm_bindgen(method, getter, js_name = logLevelDirective)]
  fn log_level_directive(this: &FlashThingOptions) -> Option<String>;
}

#[wasm_bindgen]
pub struct FlashThing {
  callback: flashthing::Callback,
  store: JsStore,
  await_gesture: js_sys::Function,

  aml: Option<AmlogicSoC<WebUsb>>,
  flasher: Option<WebFlasher>,
  num_steps: usize,
}

#[wasm_bindgen]
impl FlashThing {
  #[wasm_bindgen(constructor)]
  pub fn new(on_event: js_sys::Function, options: FlashThingOptions) -> Self {
    console_error_panic_hook::set_once();
    monitoring::init_logger(options.log_level_directive());

    let callback: flashthing::Callback = Arc::new(move |event: flashthing::Event| {
      if let Err(err) = on_event.call1(&JsValue::NULL, &conversion::event(event)) {
        tracing::error!("Error calling callback: {:?}", err);
      }
    });

    Self {
      callback,
      store: JsStore::new(options.read_all(), options.open()),
      await_gesture: options.await_gesture(),
      aml: None,
      flasher: None,
      num_steps: 0,
    }
  }

  /// Claim the device, reusing the existing claim if something already connected during this session.
  async fn connected(&mut self) -> Result<AmlogicSoC<WebUsb>, JsError> {
    if let Some(aml) = &self.aml {
      return Ok(aml.clone());
    }

    let aml = AmlogicSoC::connect(self.await_gesture.clone(), Some(self.callback.clone()))
      .await
      .map_err(to_js)?;

    self.aml = Some(aml.clone());
    Ok(aml)
  }

  #[wasm_bindgen(js_name = openJson)]
  pub async fn open_json(&mut self, meta: String) -> Result<(), JsError> {
    let config = FlashConfig::from_standalone(&meta).map_err(to_js)?;
    let aml = self.connected().await?;

    let flasher = Flasher::new(aml, self.store.clone(), config, Some(self.callback.clone()));
    self.num_steps = flasher.num_steps();
    self.flasher = Some(flasher);

    Ok(())
  }

  pub async fn flash(&mut self) -> Result<(), JsError> {
    let flasher = self
      .flasher
      .as_mut()
      .ok_or_else(|| JsError::new("a bundle must be opened before flashing"))?;

    flasher.flash().await.map_err(to_js)
  }

  pub async fn unbrick(&mut self, path: String) -> Result<(), JsError> {
    let aml = self.connected().await?;
    let (size, mut source) = self.store.open(&path).await.map_err(to_js)?;

    aml.unbrick_from(source.as_mut(), size).await.map_err(to_js)
  }

  pub async fn bulkcmd(&mut self, command: String) -> Result<String, JsError> {
    let aml = self.connected().await?;
    aml.bulkcmd(&command).await.map_err(to_js)
  }

  #[wasm_bindgen(js_name = getNumSteps)]
  pub fn get_num_steps(&self) -> u32 {
    self.num_steps as u32
  }
}
