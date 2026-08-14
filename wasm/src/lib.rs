//! WebUSB bindings for flashthing.

mod conversion;
mod monitoring;

use std::sync::Arc;

use flashthing::{
  AmlogicSoC, Fastboot, FastbootFlasher, Flasher, JsStore, WebUsb, config::FlashConfig, payload::PayloadStore,
};
use wasm_bindgen::prelude::*;

type WebFlasher = Flasher<WebUsb, JsStore>;
type WebFastbootFlasher = FastbootFlasher<WebUsb, JsStore>;

enum Backend {
  Aml(Option<AmlogicSoC<WebUsb>>),
  Fastboot(Option<Fastboot<WebUsb>>),
}

enum Runner {
  Aml(WebFlasher),
  Fastboot(WebFastbootFlasher),
}

fn to_js(error: flashthing::Error) -> JsError {
  JsError::new(&error.to_string())
}

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &'static str = r#"
export type GestureReason = 'initial' | 'reconnect';

export interface FlashThingOptions {
  /** reads a whole file out of the bundle */
  readAll: (path: string) => Promise<Uint8Array>;
  /** streams a file out of the bundle, so a multi-hundred-megabyte image never lands in memory at once */
  open: (path: string) => Promise<{ size: number; read: (length: number) => Promise<Uint8Array> }>;
  /** Resolves once the user has clicked something. */
  awaitGesture: (reason: GestureReason) => Promise<void>;
  /** Flash over fastboot against mainline u-boot instead of amlogic burn mode */
  fastboot?: boolean;
  /** Treat every raw write as sparse, skipping all-zero chunks. Fastboot only, defaults to false. */
  forceSparse?: boolean;
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

  #[wasm_bindgen(method, getter)]
  fn fastboot(this: &FlashThingOptions) -> Option<bool>;

  #[wasm_bindgen(method, getter, js_name = forceSparse)]
  fn force_sparse(this: &FlashThingOptions) -> Option<bool>;

  #[wasm_bindgen(method, getter, js_name = logLevelDirective)]
  fn log_level_directive(this: &FlashThingOptions) -> Option<String>;
}

#[wasm_bindgen]
pub struct FlashThing {
  callback: flashthing::Callback,
  store: JsStore,
  await_gesture: js_sys::Function,

  backend: Backend,
  force_sparse: bool,
  flasher: Option<Runner>,
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

    let backend = match options.fastboot().unwrap_or(false) {
      true => Backend::Fastboot(None),
      false => Backend::Aml(None),
    };

    Self {
      callback,
      store: JsStore::new(options.read_all(), options.open()),
      await_gesture: options.await_gesture(),
      backend,
      force_sparse: options.force_sparse().unwrap_or(false),
      flasher: None,
      num_steps: 0,
    }
  }

  async fn aml(&mut self) -> Result<AmlogicSoC<WebUsb>, JsError> {
    let Backend::Aml(slot) = &mut self.backend else {
      return Err(JsError::new("this FlashThing was constructed for fastboot"));
    };
    if let Some(aml) = slot {
      return Ok(aml.clone());
    }

    let aml = AmlogicSoC::connect(self.await_gesture.clone(), Some(self.callback.clone()))
      .await
      .map_err(to_js)?;

    *slot = Some(aml.clone());
    Ok(aml)
  }

  async fn fastboot(&mut self) -> Result<Fastboot<WebUsb>, JsError> {
    let Backend::Fastboot(slot) = &mut self.backend else {
      return Err(JsError::new("this FlashThing was constructed for amlogic burn mode"));
    };
    if let Some(fastboot) = slot {
      return Ok(fastboot.clone());
    }

    let fastboot = Fastboot::connect(self.await_gesture.clone(), Some(self.callback.clone()))
      .await
      .map_err(to_js)?;

    *slot = Some(fastboot.clone());
    Ok(fastboot)
  }

  #[wasm_bindgen(js_name = openJson)]
  pub async fn open_json(&mut self, meta: String) -> Result<(), JsError> {
    let config = FlashConfig::from_standalone(&meta).map_err(to_js)?;

    let flasher = match self.backend {
      Backend::Aml(_) => {
        let aml = self.aml().await?;
        Runner::Aml(Flasher::new(
          aml,
          self.store.clone(),
          config,
          Some(self.callback.clone()),
        ))
      }
      Backend::Fastboot(_) => {
        let fastboot = self.fastboot().await?;
        Runner::Fastboot(
          FastbootFlasher::new(fastboot, self.store.clone(), config, Some(self.callback.clone()))
            .force_sparse(self.force_sparse),
        )
      }
    };

    self.num_steps = match &flasher {
      Runner::Aml(flasher) => flasher.num_steps(),
      Runner::Fastboot(flasher) => flasher.num_steps(),
    };
    self.flasher = Some(flasher);

    Ok(())
  }

  pub async fn flash(&mut self) -> Result<(), JsError> {
    let flasher = self
      .flasher
      .as_mut()
      .ok_or_else(|| JsError::new("a bundle must be opened before flashing"))?;

    match flasher {
      Runner::Aml(flasher) => flasher.flash().await.map_err(to_js),
      Runner::Fastboot(flasher) => flasher.flash().await.map_err(to_js),
    }
  }

  pub async fn unbrick(&mut self, path: String) -> Result<(), JsError> {
    let device = match self.backend {
      Backend::Aml(_) => Backend::Aml(Some(self.aml().await?)),
      Backend::Fastboot(_) => Backend::Fastboot(Some(self.fastboot().await?)),
    };

    let (size, mut source) = self.store.open(&path).await.map_err(to_js)?;

    match device {
      Backend::Aml(Some(aml)) => aml.unbrick_from(source.as_mut(), size).await.map_err(to_js),
      Backend::Fastboot(Some(fastboot)) => fastboot.unbrick_from(source.as_mut(), size).await.map_err(to_js),
      _ => unreachable!("the device was just connected"),
    }
  }

  pub async fn bulkcmd(&mut self, command: String) -> Result<String, JsError> {
    let aml = self.aml().await?;
    aml.bulkcmd(&command).await.map_err(to_js)
  }

  pub async fn console(&mut self, command: String) -> Result<String, JsError> {
    let fastboot = self.fastboot().await?;
    fastboot.console(&command).await.map_err(to_js)
  }

  #[wasm_bindgen(js_name = getNumSteps)]
  pub fn get_num_steps(&self) -> u32 {
    self.num_steps as u32
  }
}
