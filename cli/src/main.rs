mod monitoring;

use std::{env, ffi::OsStr, path::PathBuf};

use clap::Parser;
use flashthing::{FastbootFlasher, Flasher};

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
  /// setup host - this currently only sets up udev rules on Linux
  #[arg(long, action)]
  setup: bool,
  /// Send a single u-boot command to a device in USB burn mode and print its response.
  #[arg(long, value_name = "CMD")]
  bulkcmd: Option<String>,
  /// Flash over fastboot against mainline u-boot instead of amlogic burn mode
  #[arg(short, long, action)]
  fastboot: bool,
  /// Run a single u-boot command over fastboot's `oem console` and print its output
  #[arg(long, value_name = "CMD")]
  console: Option<String>,
  /// Treat every raw write as sparse, skipping all-zero chunks
  #[arg(long, action)]
  sparse: bool,
}

fn main() {
  monitoring::init_logger();

  let args = Args::parse();
  if args.setup {
    tracing::info!("setting up host...");
    match flashthing::host_setup() {
      Ok(()) => tracing::info!("host set up successfully"),
      Err(err) => tracing::error!("failed to set up host: {}", err),
    }
    return;
  }

  if args.unbrick {
    tracing::info!("unbricking device...");

    let result = if args.fastboot {
      let Ok(fastboot) = pollster::block_on(flashthing::Fastboot::connect(None)) else {
        tracing::error!("could not find device!");
        std::process::exit(1);
      };
      pollster::block_on(fastboot.unbrick())
    } else {
      let Ok(aml) = pollster::block_on(flashthing::AmlogicSoC::connect(None)) else {
        tracing::error!("could not find device!");
        std::process::exit(1);
      };
      pollster::block_on(aml.unbrick())
    };

    match result {
      Ok(()) => tracing::info!("done!"),
      Err(err) => tracing::error!("failed to unbrick device: {}", err),
    }

    return;
  }

  if let Some(cmd) = args.console {
    let Ok(fastboot) = pollster::block_on(flashthing::Fastboot::connect(None)) else {
      tracing::error!("could not find device!");
      std::process::exit(1);
    };

    match pollster::block_on(fastboot.console(&cmd)) {
      Ok(output) => println!("{}", output),
      Err(err) => {
        tracing::error!("console command failed: {}", err);
        std::process::exit(1);
      }
    }
    return;
  }

  if let Some(cmd) = args.bulkcmd {
    let Ok(aml) = pollster::block_on(flashthing::AmlogicSoC::connect(None)) else {
      tracing::error!("could not find device!");
      std::process::exit(1);
    };

    match pollster::block_on(aml.bulkcmd(&cmd)) {
      Ok(response) => print!("{}", response),
      Err(err) => {
        tracing::error!("bulkcmd failed: {}", err);
        std::process::exit(1);
      }
    }
    return;
  }

  let path = args
    .path
    .unwrap_or_else(|| env::current_dir().expect("could not determine current directory"));

  let result = if args.fastboot {
    pollster::block_on(flash_fastboot(path, args.stock, args.sparse))
  } else {
    pollster::block_on(flash_aml(path, args.stock))
  };

  match result {
    Ok(()) => tracing::info!("done!"),
    Err(err) => tracing::error!("failed to flash device: {}", err),
  }
}

enum Source {
  Archive(PathBuf),
  Directory(PathBuf),
}

fn classify(path: PathBuf) -> Source {
  if path.is_file() && path.extension() == Some(OsStr::new("zip")) {
    Source::Archive(path)
  } else if path.is_dir() {
    Source::Directory(path)
  } else {
    tracing::error!("could not find anything to flash at {}!", path.display());
    std::process::exit(1);
  }
}

async fn flash_aml(path: PathBuf, stock: bool) -> flashthing::Result<()> {
  let mut device = match (classify(path), stock) {
    (Source::Archive(path), true) => Flasher::from_stock_archive(path, None).await?,
    (Source::Archive(path), false) => Flasher::from_archive(path, None).await?,
    (Source::Directory(path), true) => Flasher::from_stock_directory(path, None).await?,
    (Source::Directory(path), false) => Flasher::from_directory(path, None).await?,
  };

  device.flash().await?;

  Ok(())
}

async fn flash_fastboot(path: PathBuf, stock: bool, sparse: bool) -> flashthing::Result<()> {
  let mut device = match (classify(path), stock) {
    (Source::Archive(path), true) => FastbootFlasher::from_stock_archive(path, None).await?,
    (Source::Archive(path), false) => FastbootFlasher::from_archive(path, None).await?,
    (Source::Directory(path), true) => FastbootFlasher::from_stock_directory(path, None).await?,
    (Source::Directory(path), false) => FastbootFlasher::from_directory(path, None).await?,
  };

  device = device.force_sparse(sparse);
  device.flash().await?;

  Ok(())
}
