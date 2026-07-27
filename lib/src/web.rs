use std::{cell::RefCell, future::Future, pin::Pin, time::Duration};

use js_sys::{Array, Function, Promise, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
  UsbControlTransferParameters, UsbDevice, UsbDeviceFilter, UsbDeviceRequestOptions, UsbDirection, UsbInTransferResult,
  UsbOutTransferResult, UsbRecipient, UsbRequestType, UsbTransferStatus,
};

use crate::{
  Callback, DeviceMode, Error, Event, PRODUCT_ID, PRODUCT_ID_NORMAL, Result, VENDOR_ID, VENDOR_ID_NORMAL,
  payload::{PayloadSource, PayloadStore},
  time,
  usb::UsbTransport,
};

const INTERFACE_NUMBER: u8 = 0;
const SUPPORTED: [(u16, u16); 2] = [(VENDOR_ID, PRODUCT_ID), (VENDOR_ID_NORMAL, PRODUCT_ID_NORMAL)];
const REACQUIRE_ATTEMPTS: usize = 8;
const REACQUIRE_INTERVAL: Duration = Duration::from_millis(250);

/// Why the page is being asked for a click. Handed to the gesture callback so the UI can explain itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureReason {
  /// No device has been chosen yet.
  Initial,
  /// The device re-enumerated and its old permission no longer applies.
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

/// The chooser filter list, built from the same constants [`supported`] matches against.
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

/// A device this origin has already been granted, if one is plugged in.
async fn already_granted() -> Option<UsbDevice> {
  let devices = JsFuture::from(usb().ok()?.get_devices()).await.ok()?;
  Array::from(&devices)
    .iter()
    .filter_map(|value| value.dyn_into::<UsbDevice>().ok())
    .find(supported)
}

/// The `DOMException` name behind a rejection, which is how the chooser distinguishes a dismissal from a real fault.
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

/// Awaits `promise`, giving up once `timeout` elapses.
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
  endpoint_in: u8,
  endpoint_out: u8,
}

/// [`UsbTransport`] over WebUSB.
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

  /// Open the chooser, which the browser only allows while a click the page just handled is still live.
  ///
  /// A chooser dismissed without a pick is not fatal: the device may simply not have finished re-enumerating, so ask
  /// the page for another click. Anything else, a lost user gesture above all, fails rather than spinning.
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

  /// Find a device, asking the page for a click only when the browser gives us no other way.
  async fn next_device(&self, reason: GestureReason) -> Result<UsbDevice> {
    if let Some(device) = already_granted().await {
      return Ok(device);
    }

    // the amlogic rom reports no serial number, so chromium can only hold its grant against the live connection and
    // drops it the moment a reset disconnects the device. poll in case this one did survive, then ask for a click.
    if reason == GestureReason::Reconnect {
      for _ in 0..REACQUIRE_ATTEMPTS {
        time::sleep(REACQUIRE_INTERVAL).await;
        if let Some(device) = already_granted().await {
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

    let device = self.next_device(GestureReason::Initial).await.ok()?;
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

fn endpoints(device: &UsbDevice) -> Result<(u8, u8)> {
  let configuration = device
    .configuration()
    .ok_or_else(|| Error::InvalidOperation("Interface not found".into()))?;
  let interface = configuration
    .interfaces()
    .iter()
    .find(|interface| interface.interface_number() == INTERFACE_NUMBER)
    .ok_or_else(|| Error::InvalidOperation("Interface not found".into()))?;

  let mut endpoint_in = None;
  let mut endpoint_out = None;
  for endpoint in interface.alternate().endpoints().iter() {
    match endpoint.direction() {
      UsbDirection::In => endpoint_in = Some(endpoint.endpoint_number()),
      UsbDirection::Out => endpoint_out = Some(endpoint.endpoint_number()),
      _ => {}
    }
  }

  let endpoint_in = endpoint_in.ok_or_else(|| Error::InvalidOperation("IN endpoint not found".into()))?;
  let endpoint_out = endpoint_out.ok_or_else(|| Error::InvalidOperation("OUT endpoint not found".into()))?;

  Ok((endpoint_in, endpoint_out))
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

  async fn acquire(&self) -> Result<()> {
    tracing::debug!("connecting to Amlogic device");
    if let Some(callback) = &self.callback {
      callback(Event::Connecting);
    };

    // having already claimed an interface means this is the far side of a reset, not a cold start.
    let stale = self.claimed.borrow_mut().take();
    let reason = match &stale {
      Some(_) => GestureReason::Reconnect,
      None => GestureReason::Initial,
    };

    if let Some(stale) = stale {
      let _ = JsFuture::from(stale.device.release_interface(INTERFACE_NUMBER)).await;
      let _ = JsFuture::from(stale.device.close()).await;
    }
    self.device.borrow_mut().take();

    let device = self.next_device(reason).await?;
    *self.device.borrow_mut() = Some(device.clone());

    if !device.opened() {
      JsFuture::from(device.open()).await.map_err(js_error)?;
    }
    JsFuture::from(device.select_configuration(1)).await.map_err(js_error)?;
    JsFuture::from(device.claim_interface(INTERFACE_NUMBER))
      .await
      .map_err(js_error)?;

    let (endpoint_in, endpoint_out) = endpoints(&device)?;
    tracing::info!("device connected, claiming interface {}", INTERFACE_NUMBER);

    *self.claimed.borrow_mut() = Some(Claimed {
      device,
      endpoint_in,
      endpoint_out,
    });

    if let Some(callback) = &self.callback {
      callback(Event::Connected);
    };

    Ok(())
  }
}

/// [`PayloadStore`] backed by the page.
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
