use std::sync::Arc;

use napi::threadsafe_function::ThreadsafeFunctionCallMode;
use napi_derive::napi;

use crate::{conversion::FlashEvent, FlashCallback};

// Structure for the log messages sent to JavaScript
#[napi(object)]
#[derive(Debug, Clone)]
pub struct LogMessage {
  /// log level (TRACE, DEBUG, INFO, WARN, ERROR)
  pub level: String,
  /// log target
  pub target: String,
  /// log message
  pub message: String,
  /// timestamp in ISO 8601 format
  pub timestamp: String,
}

struct JavaScriptLogger {
  tsfn: Arc<FlashCallback>,
}

impl<S> tracing_subscriber::Layer<S> for JavaScriptLogger
where
  S: tracing::Subscriber,
{
  fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
    let metadata = event.metadata();
    let level = metadata.level().as_str().to_string();
    let target = metadata.target().to_string();

    let timestamp = chrono::Utc::now().to_rfc3339();

    let mut message = String::new();
    let mut visitor = MessageVisitor(&mut message);
    event.record(&mut visitor);

    let log_msg = LogMessage {
      level,
      target,
      message,
      timestamp,
    };

    let js_event = FlashEvent::Log { data: log_msg };
    let _ = self.tsfn.call(js_event, ThreadsafeFunctionCallMode::NonBlocking);
  }
}

struct MessageVisitor<'a>(&'a mut String);

impl tracing::field::Visit for MessageVisitor<'_> {
  fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
    if field.name() == "message" {
      self.0.push_str(value);
    } else {
      if !self.0.is_empty() {
        self.0.push(' ');
      }
      self.0.push_str(&format!("{}={}", field.name(), value));
    }
  }

  fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
    if field.name() == "message" {
      use std::fmt::Write;
      let _ = write!(self.0, "{:?}", value);
    } else {
      if !self.0.is_empty() {
        self.0.push(' ');
      }
      use std::fmt::Write;
      let _ = write!(self.0, "{}={:?}", field.name(), value);
    }
  }
}

pub fn init_logger(tsfn: Arc<FlashCallback>, level_directive: Option<String>) {
  use tracing::metadata::LevelFilter;
  use tracing_subscriber::{
    filter::Directive, prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer,
  };

  let default_directive = Directive::from(LevelFilter::INFO);
  let filter_directives = if let Some(directive) = level_directive {
    directive
  } else if let Ok(filter) = std::env::var("RUST_LOG") {
    filter
  } else {
    "flashthing_cli=info,flashthing=info".to_string()
  };

  let js_filter = EnvFilter::builder()
    .with_default_directive(default_directive)
    .parse_lossy(filter_directives);

  let js_logger = JavaScriptLogger { tsfn };
  tracing_subscriber::registry()
    .with(js_logger.with_filter(js_filter))
    .init();

  tracing::info!("initialized logger");
}
