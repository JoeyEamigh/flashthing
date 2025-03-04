mod monitoring;

use clap::Parser;
use flashthing::Flasher;
use std::{env, ffi::OsStr, path::PathBuf};

#[derive(Parser, Debug)]
#[command(
  author = "Joey Eamigh",
  version = "0.1.0",
  about = "cli for flashing the Spotify Car Thing",
  long_about = None
)]
struct Args {
  /// Path to a zip file or a directory. Defaults to the current working directory if omitted.
  path: Option<PathBuf>,
  /// Whether the directory or archive contains a stock dump with no `meta.json` file.
  #[arg(short, long, action)]
  stock: bool,
  /// Whether to unbrick the device.
  #[arg(long, action)]
  unbrick: bool,
}

fn main() {
  monitoring::init_logger();

  let args = Args::parse();
  let path = args
    .path
    .unwrap_or_else(|| env::current_dir().expect("could not determine current directory"));

  if args.unbrick {
    tracing::info!("unbricking device...");
    let Ok(aml) = flashthing::AmlogicSoC::init(None) else {
      tracing::error!("could not find device!");
      panic!("could not find device!");
    };

    match aml.unbrick() {
      Ok(()) => tracing::info!("done!"),
      Err(err) => tracing::error!("failed to unbrick device: {}", err),
    }

    return;
  }

  match flash(path, args.stock) {
    Ok(()) => tracing::info!("done!"),
    Err(err) => tracing::error!("failed to flash device: {}", err),
  }
}

fn flash(path: PathBuf, stock: bool) -> flashthing::Result<()> {
  let mut device = if path.is_file() && path.extension() == Some(OsStr::new("zip")) {
    if stock {
      Flasher::from_stock_archive(path, None)?
    } else {
      Flasher::from_archive(path, None)?
    }
  } else if path.is_dir() {
    if stock {
      Flasher::from_stock_directory(path, None)?
    } else {
      Flasher::from_directory(path, None)?
    }
  } else {
    tracing::error!("could not find anything to flash!");
    panic!("could not find anything to flash!");
  };

  device.flash()?;

  Ok(())
}
