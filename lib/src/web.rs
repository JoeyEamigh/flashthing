use std::{cell::RefCell, future::Future, pin::Pin, time::Duration};

use js_sys::{Array, Function, Promise, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
  UsbControlTransferParameters, UsbDevice, UsbDeviceFilter, UsbDeviceRequestOptions, UsbDirection, UsbInTransferResult,
  UsbOutTransferResult, UsbRecipient, UsbRequestType, UsbTransferStatus,
};

use crate::{
  Callback, DeviceMode, Error, Event, PRODUCT_ID, PRODUCT_ID_FASTBOOT, PRODUCT_ID_NORMAL, Result, VENDOR_ID,
  VENDOR_ID_FASTBOOT, VENDOR_ID_NORMAL,
  payload::{PayloadSource, PayloadStore},
  time,
  usb::{UsbTarget, UsbTransport},
};

const MASKROM_INTERFACE: u8 = 0;

const FASTBOOT_CLASS: u8 = 0xff;
const FASTBOOT_SUBCLASS: u8 = 0x42;
const FASTBOOT_PROTOCOL: u8 = 0x03;

const SUPPORTED: [(u16, u16); 3] = [
  (VENDOR_ID, PRODUCT_ID),
  (VENDOR_ID_FASTBOOT, PRODUCT_ID_FASTBOOT),
  (VENDOR_ID_NORMAL, PRODUCT_ID_NORMAL),
];
const REACQUIRE_ATTEMPTS: usize = 8;
const REACQUIRE_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureReason {
  Initial,
  Reconnect,
}

impl GestureReason {
  fn as_str(self) -> &'static str {
    match self {
      Self::Initial => "initial",
      Self::Reconnect => "reconnect",
    }
  }
}

fn usb() -> Result<web_sys::Usb> {
  let window =
    web_sys::window().ok_or_else(|| Error::InvalidOperation("no window to read navigator.usb from".into()))?;
  Ok(window.navigator().usb())
}

fn supported(device: &UsbDevice) -> bool {
  SUPPORTED
    .iter()
    .any(|(vendor, product)| device.vendor_id() == *vendor && device.product_id() == *product)
}

fn filters() -> Vec<UsbDeviceFilter> {
  SUPPORTED
    .iter()
    .map(|(vendor, product)| {
      let filter = UsbDeviceFilter::new();
      filter.set_vendor_id(*vendor);
      filter.set_product_id(*product);
      filter
    })
    .collect()
}

async fn already_granted(target: Option<UsbTarget>) -> Option<UsbDevice> {
  let devices = JsFuture::from(usb().ok()?.get_devices()).await.ok()?;
  Array::from(&devices)
    .iter()
    .filter_map(|value| value.dyn_into::<UsbDevice>().ok())
    .find(|device| match target {
      Some(target) => {
        let (vendor, product) = target.ids();
        device.vendor_id() == vendor && device.product_id() == product
      }
      None => supported(device),
    })
}

fn error_name(value: &JsValue) -> Option<String> {
  js_sys::Reflect::get(value, &JsValue::from_str("name"))
    .ok()
    .and_then(|name| name.as_string())
}

fn js_error(value: JsValue) -> Error {
  let message = value
    .dyn_ref::<js_sys::Error>()
    .map(|error| String::from(error.message()))
    .or_else(|| value.as_string())
    .unwrap_or_else(|| format!("{:?}", value));

  Error::Js(message)
}

async fn with_timeout(promise: Promise, timeout: Duration) -> Result<JsValue> {
  let race = Array::new();
  race.push(promise.as_ref());
  race.push(time::timer_promise(timeout).as_ref());

  let value = JsFuture::from(Promise::race(race.as_ref())).await.map_err(js_error)?;
  if value.is_undefined() {
    return Err(Error::Timeout);
  }

  Ok(value)
}

fn setup(request: u8, value: u16, index: u16) -> UsbControlTransferParameters {
  UsbControlTransferParameters::new(index, UsbRecipient::Device, request, UsbRequestType::Vendor, value)
}

fn check_out(result: &UsbOutTransferResult) -> Result<usize> {
  match result.status() {
    UsbTransferStatus::Ok => Ok(result.bytes_written() as usize),
    status => Err(Error::InvalidOperation(format!("usb out transfer {:?}", status))),
  }
}

fn copy_in(result: &UsbInTransferResult, buf: &mut [u8]) -> Result<usize> {
  match result.status() {
    UsbTransferStatus::Ok => {}
    status => return Err(Error::InvalidOperation(format!("usb in transfer {:?}", status))),
  }

  let Some(view) = result.data() else { return Ok(0) };
  let length = std::cmp::min(view.byte_length(), buf.len());
  let bytes = Uint8Array::new_with_byte_offset_and_length(&view.buffer(), view.byte_offset() as u32, length as u32);
  bytes.copy_to(&mut buf[..length]);

  Ok(length)
}

struct Claimed {
  device: UsbDevice,
  interface: u8,
  endpoint_in: u8,
  endpoint_out: u8,
}

pub struct WebUsb {
  await_gesture: Function,
  claimed: RefCell<Option<Claimed>>,
  device: RefCell<Option<UsbDevice>>,
  callback: Option<Callback>,
}

impl WebUsb {
  pub fn new(await_gesture: Function, callback: Option<Callback>) -> Self {
    Self {
      await_gesture,
      claimed: RefCell::new(None),
      device: RefCell::new(None),
      callback,
    }
  }

  async fn prompt(&self, reason: GestureReason) -> Result<UsbDevice> {
    loop {
      let promise = self
        .await_gesture
        .call1(&JsValue::NULL, &JsValue::from_str(reason.as_str()))
        .map_err(js_error)?
        .dyn_into::<Promise>()
        .map_err(|_| Error::InvalidOperation("gesture callback did not return a promise".into()))?;
      JsFuture::from(promise).await.map_err(js_error)?;

      match JsFuture::from(usb()?.request_device(&UsbDeviceRequestOptions::new(&filters()))).await {
        Ok(device) => {
          return device
            .dyn_into::<UsbDevice>()
            .map_err(|_| Error::InvalidOperation("chooser did not return a usb device".into()));
        }
        Err(error) => {
          let name = error_name(&error);
          tracing::warn!("device chooser rejected with {:?}", name);

          if name.as_deref() != Some("NotFoundError") {
            return Err(js_error(error));
          }
        }
      }
    }
  }

  async fn next_device(&self, reason: GestureReason, target: Option<UsbTarget>) -> Result<UsbDevice> {
    if let Some(device) = already_granted(target).await {
      return Ok(device);
    }

    if reason == GestureReason::Reconnect {
      for _ in 0..REACQUIRE_ATTEMPTS {
        time::sleep(REACQUIRE_INTERVAL).await;
        if let Some(device) = already_granted(target).await {
          return Ok(device);
        }
      }
    }

    self.prompt(reason).await
  }

  async fn known_device(&self) -> Option<UsbDevice> {
    if let Some(device) = self.device.borrow().clone() {
      return Some(device);
    }

    let device = self.next_device(GestureReason::Initial, None).await.ok()?;
    *self.device.borrow_mut() = Some(device.clone());
    Some(device)
  }

  fn claimed(&self) -> Result<(UsbDevice, u8, u8)> {
    let borrowed = self.claimed.borrow();
    let claimed = borrowed
      .as_ref()
      .ok_or_else(|| Error::InvalidOperation("device is not claimed".into()))?;

    Ok((claimed.device.clone(), claimed.endpoint_in, claimed.endpoint_out))
  }
}

fn interface_for(device: &UsbDevice, target: UsbTarget) -> Result<(u8, u8, u8)> {
  let configuration = device
    .configuration()
    .ok_or_else(|| Error::InvalidOperation("Interface not found".into()))?;

  for interface in configuration.interfaces().iter() {
    let Ok(interface) = interface.dyn_into::<web_sys::UsbInterface>() else {
      continue;
    };
    let alternate = interface.alternate();

    let matches = match target {
      UsbTarget::Maskrom => interface.interface_number() == MASKROM_INTERFACE,
      UsbTarget::Fastboot => {
        alternate.interface_class() == FASTBOOT_CLASS
          && alternate.interface_subclass() == FASTBOOT_SUBCLASS
          && alternate.interface_protocol() == FASTBOOT_PROTOCOL
      }
    };
    if !matches {
      continue;
    }

    let mut endpoint_in = None;
    let mut endpoint_out = None;
    for endpoint in alternate.endpoints().iter() {
      let Ok(endpoint) = endpoint.dyn_into::<web_sys::UsbEndpoint>() else {
        continue;
      };
      match endpoint.direction() {
        UsbDirection::In => endpoint_in = endpoint_in.or(Some(endpoint.endpoint_number())),
        UsbDirection::Out => endpoint_out = endpoint_out.or(Some(endpoint.endpoint_number())),
        _ => {}
      }
    }

    if let (Some(endpoint_in), Some(endpoint_out)) = (endpoint_in, endpoint_out) {
      return Ok((interface.interface_number(), endpoint_in, endpoint_out));
    }
  }

  Err(Error::InvalidOperation(format!(
    "no usable {:?} interface on this device",
    target
  )))
}

impl UsbTransport for WebUsb {
  async fn control_out(&self, request: u8, value: u16, index: u16, data: &[u8], timeout: Duration) -> Result<usize> {
    let (device, _, _) = self.claimed()?;
    let payload = Uint8Array::from(data);
    let promise = device
      .control_transfer_out_with_u8_array(&setup(request, value, index), &payload)
      .map_err(js_error)?;

    let result = with_timeout(promise.unchecked_into(), timeout).await?;
    check_out(result.unchecked_ref())
  }

  async fn control_in(&self, request: u8, value: u16, index: u16, buf: &mut [u8], timeout: Duration) -> Result<usize> {
    let (device, _, _) = self.claimed()?;
    let promise = device.control_transfer_in(&setup(request, value, index), buf.len() as u16);

    let result = with_timeout(promise.unchecked_into(), timeout).await?;
    copy_in(result.unchecked_ref(), buf)
  }

  async fn bulk_out(&self, data: &[u8], timeout: Duration) -> Result<usize> {
    let (device, _, endpoint_out) = self.claimed()?;
    let payload = Uint8Array::from(data);
    let promise = device
      .transfer_out_with_u8_array(endpoint_out, &payload)
      .map_err(js_error)?;

    let result = with_timeout(promise.unchecked_into(), timeout).await?;
    check_out(result.unchecked_ref())
  }

  async fn bulk_in(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
    let (device, endpoint_in, _) = self.claimed()?;
    let promise = device.transfer_in(endpoint_in, buf.len() as u32);

    let result = with_timeout(promise.unchecked_into(), timeout).await?;
    copy_in(result.unchecked_ref(), buf)
  }

  async fn mode(&self) -> DeviceMode {
    let Some(device) = self.known_device().await else {
      tracing::debug!("No device found!");
      return DeviceMode::NotFound;
    };

    if device.vendor_id() == VENDOR_ID_FASTBOOT && device.product_id() == PRODUCT_ID_FASTBOOT {
      tracing::debug!("Found device running mainline u-boot's fastboot gadget");
      return DeviceMode::Fastboot;
    }

    if device.vendor_id() == VENDOR_ID_NORMAL && device.product_id() == PRODUCT_ID_NORMAL {
      tracing::debug!("Found device booted normally, with USB Gadget (adb/usbnet) enabled");
      return DeviceMode::Normal;
    }

    if device.vendor_id() != VENDOR_ID || device.product_id() != PRODUCT_ID {
      tracing::debug!("No device found!");
      return DeviceMode::NotFound;
    }

    if device.product_name().as_deref() == Some("GX-CHIP") {
      tracing::debug!("Found device booted in USB Mode (buttons 1 & 4 held at boot)");
      DeviceMode::Usb
    } else {
      tracing::debug!("Found device booted in USB Burn Mode (ready for commands)");
      DeviceMode::UsbBurn
    }
  }

  async fn acquire(&self, target: UsbTarget) -> Result<()> {
    tracing::debug!("connecting to device as {:?}", target);
    if let Some(callback) = &self.callback {
      callback(Event::Connecting);
    };

    let stale = self.claimed.borrow_mut().take();
    let reason = match &stale {
      Some(_) => GestureReason::Reconnect,
      None => GestureReason::Initial,
    };

    if let Some(stale) = stale {
      let _ = JsFuture::from(stale.device.release_interface(stale.interface)).await;
      let _ = JsFuture::from(stale.device.close()).await;
    }
    self.device.borrow_mut().take();

    let device = self.next_device(reason, Some(target)).await?;
    *self.device.borrow_mut() = Some(device.clone());

    if !device.opened() {
      JsFuture::from(device.open()).await.map_err(js_error)?;
    }
    JsFuture::from(device.select_configuration(1)).await.map_err(js_error)?;

    let (interface, endpoint_in, endpoint_out) = interface_for(&device, target)?;
    JsFuture::from(device.claim_interface(interface))
      .await
      .map_err(js_error)?;
    tracing::info!("device connected, claiming interface {}", interface);

    *self.claimed.borrow_mut() = Some(Claimed {
      device,
      interface,
      endpoint_in,
      endpoint_out,
    });

    if let Some(callback) = &self.callback {
      callback(Event::Connected);
    };

    Ok(())
  }
}

#[derive(Clone)]
pub struct JsStore {
  read_all: Function,
  open: Function,
}

impl JsStore {
  pub fn new(read_all: Function, open: Function) -> Self {
    Self { read_all, open }
  }
}

async fn call_for_promise(function: &Function, argument: &JsValue) -> Result<JsValue> {
  let promise = function
    .call1(&JsValue::NULL, argument)
    .map_err(js_error)?
    .dyn_into::<Promise>()
    .map_err(|_| Error::InvalidOperation("payload store callback did not return a promise".into()))?;

  JsFuture::from(promise).await.map_err(js_error)
}

impl PayloadStore for JsStore {
  fn read_all<'a>(&'a mut self, path: &'a str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + 'a>> {
    Box::pin(async move {
      let value = call_for_promise(&self.read_all, &JsValue::from_str(path)).await?;
      let bytes = value
        .dyn_into::<Uint8Array>()
        .map_err(|_| Error::InvalidOperation(format!("readAll({}) did not resolve with a Uint8Array", path)))?;

      Ok(bytes.to_vec())
    })
  }

  fn open<'a>(
    &'a mut self,
    path: &'a str,
  ) -> Pin<Box<dyn Future<Output = Result<(usize, Box<dyn PayloadSource + 'a>)>> + 'a>> {
    Box::pin(async move {
      let handle = call_for_promise(&self.open, &JsValue::from_str(path)).await?;

      let size = js_sys::Reflect::get(&handle, &JsValue::from_str("size"))
        .ok()
        .and_then(|value| value.as_f64())
        .ok_or_else(|| Error::InvalidOperation(format!("open({}) resolved without a numeric size", path)))?;
      let read = js_sys::Reflect::get(&handle, &JsValue::from_str("read"))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok())
        .ok_or_else(|| Error::InvalidOperation(format!("open({}) resolved without a read function", path)))?;

      let source: Box<dyn PayloadSource + 'a> = Box::new(JsSource { read });
      Ok((size as usize, source))
    })
  }
}

struct JsSource {
  read: Function,
}

impl PayloadSource for JsSource {
  fn read_exact<'a>(&'a mut self, buf: &'a mut [u8]) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
    Box::pin(async move {
      let value = call_for_promise(&self.read, &JsValue::from_f64(buf.len() as f64)).await?;
      let bytes = value
        .dyn_into::<Uint8Array>()
        .map_err(|_| Error::InvalidOperation("payload read did not resolve with a Uint8Array".into()))?;

      if bytes.length() as usize != buf.len() {
        return Err(Error::InvalidOperation(format!(
          "payload read returned {} bytes, expected {}",
          bytes.length(),
          buf.len()
        )));
      }

      bytes.copy_to(buf);
      Ok(())
    })
  }
}
