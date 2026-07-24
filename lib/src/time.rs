#[cfg(not(target_arch = "wasm32"))]
mod imp {
  use std::time::Duration;
  pub use std::time::Instant;

  pub async fn sleep(duration: Duration) {
    std::thread::sleep(duration);
  }
}

#[cfg(target_arch = "wasm32")]
mod imp {
  use std::time::Duration;

  use wasm_bindgen::{JsCast, JsValue};
  use wasm_bindgen_futures::JsFuture;

  thread_local! {
    static PERFORMANCE: Option<web_sys::Performance> =
      js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("performance"))
        .ok()
        .and_then(|value| value.dyn_into::<web_sys::Performance>().ok());
  }

  fn now_ms() -> f64 {
    PERFORMANCE.with(|performance| match performance {
      Some(performance) => performance.now(),
      None => js_sys::Date::now(),
    })
  }

  #[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
  pub struct Instant(f64);

  impl Instant {
    pub fn now() -> Self {
      Self(now_ms())
    }

    pub fn elapsed(&self) -> Duration {
      Duration::from_secs_f64(((now_ms() - self.0) / 1000.0).max(0.0))
    }
  }

  /// A promise that resolves with `undefined` once `duration` has passed.
  pub(crate) fn timer_promise(duration: Duration) -> js_sys::Promise {
    let millis = JsValue::from_f64(duration.as_secs_f64() * 1000.0);

    js_sys::Promise::new(&mut |resolve, _reject| {
      let global = js_sys::global();
      let set_timeout = js_sys::Reflect::get(&global, &JsValue::from_str("setTimeout"))
        .ok()
        .and_then(|value| value.dyn_into::<js_sys::Function>().ok());

      match set_timeout {
        // resolving inline keeps callers from hanging forever if the host has no timer at all
        None => {
          let _ = resolve.call0(&JsValue::NULL);
        }
        Some(set_timeout) => {
          let _ = set_timeout.call2(&global, resolve.as_ref(), &millis);
        }
      }
    })
  }

  pub async fn sleep(duration: Duration) {
    let _ = JsFuture::from(timer_promise(duration)).await;
  }
}

#[cfg(target_arch = "wasm32")]
pub(crate) use imp::timer_promise;
pub use imp::{Instant, sleep};
