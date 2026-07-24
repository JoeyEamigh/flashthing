use js_sys::{JSON, Object, Reflect};
use wasm_bindgen::JsValue;

fn set(object: &Object, key: &str, value: &JsValue) {
  let _ = Reflect::set(object, &JsValue::from_str(key), value);
}

fn tagged(kind: &str) -> Object {
  let object = Object::new();
  set(&object, "type", &JsValue::from_str(kind));
  object
}

fn device_mode(mode: flashthing::DeviceMode) -> &'static str {
  match mode {
    flashthing::DeviceMode::Normal => "normal",
    flashthing::DeviceMode::Usb => "usb",
    flashthing::DeviceMode::UsbBurn => "usbBurn",
    flashthing::DeviceMode::NotFound => "notFound",
  }
}

fn progress(value: &flashthing::FlashProgress) -> Object {
  let object = Object::new();
  set(&object, "percent", &JsValue::from_f64(value.percent));
  set(&object, "elapsed", &JsValue::from_f64(value.elapsed));
  set(&object, "eta", &JsValue::from_f64(value.eta));
  set(&object, "rate", &JsValue::from_f64(value.rate));
  set(&object, "avgChunkTime", &JsValue::from_f64(value.avg_chunk_time));
  set(&object, "avgRate", &JsValue::from_f64(value.avg_rate));
  object
}

fn step(value: &flashthing::config::FlashStep) -> JsValue {
  serde_json::to_string(value)
    .ok()
    .and_then(|json| JSON::parse(&json).ok())
    .unwrap_or(JsValue::NULL)
}

pub fn event(value: flashthing::Event) -> JsValue {
  let object = match value {
    flashthing::Event::FindingDevice => tagged("findingDevice"),
    flashthing::Event::Connecting => tagged("connecting"),
    flashthing::Event::Connected => tagged("connected"),
    flashthing::Event::Bl2Boot => tagged("bl2Boot"),
    flashthing::Event::Resetting => tagged("resetting"),
    flashthing::Event::DeviceMode(mode) => {
      let object = tagged("deviceMode");
      set(&object, "mode", &JsValue::from_str(device_mode(mode)));
      object
    }
    flashthing::Event::Step(index, value) => {
      let object = tagged("step");
      set(&object, "step", &JsValue::from_f64(index as f64));
      set(&object, "data", &step(&value));
      object
    }
    flashthing::Event::FlashProgress(value) => {
      let object = tagged("flashProgress");
      set(&object, "data", progress(&value).as_ref());
      object
    }
  };

  object.into()
}
