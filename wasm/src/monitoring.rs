use std::io::Write;

use tracing::Level;
use tracing_subscriber::fmt::MakeWriter;
use wasm_bindgen::JsValue;

struct ConsoleWriter(Vec<u8>);

impl Write for ConsoleWriter {
  fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
    self.0.extend_from_slice(buf);
    Ok(buf.len())
  }

  fn flush(&mut self) -> std::io::Result<()> {
    if self.0.is_empty() {
      return Ok(());
    }

    let line = String::from_utf8_lossy(&self.0);
    web_sys::console::log_1(&JsValue::from_str(line.trim_end()));
    self.0.clear();

    Ok(())
  }
}

impl Drop for ConsoleWriter {
  fn drop(&mut self) {
    let _ = self.flush();
  }
}

struct MakeConsoleWriter;

impl MakeWriter<'_> for MakeConsoleWriter {
  type Writer = ConsoleWriter;

  fn make_writer(&self) -> Self::Writer {
    ConsoleWriter(Vec::new())
  }
}

pub fn init_logger(level: Option<String>) {
  let level = level
    .and_then(|level| level.parse::<Level>().ok())
    .unwrap_or(Level::INFO);

  let _ = tracing_subscriber::fmt()
    .with_writer(MakeConsoleWriter)
    .with_ansi(false)
    .without_time()
    .with_max_level(level)
    .try_init();
}
