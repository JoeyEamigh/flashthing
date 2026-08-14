use std::{future::Future, pin::Pin, time::Duration};

use super::client::{FASTBOOT_BUF_ADDR, Fastboot};
use crate::{
  Callback, ERASE_GROUP_SECTORS, Error, Event, PART_SECTOR_SIZE, Result,
  config::{FlashConfig, FlashStep, WaitValue},
  flash::{FlashProgress, open_payload, read_payload, read_text},
  partitions::SUPERBIRD_PARTITIONS,
  payload::{PayloadSource, PayloadStore, inline_source},
  time::{Instant, sleep},
  usb::UsbTransport,
};

const RAW_ALIAS: &str = "ft";
const MAX_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const UPLOAD_SHARE: f64 = 0.5;

struct ProgressTracker {
  total: usize,
  done: usize,
  start: Instant,
  chunk_start: Instant,
  chunks: usize,
  avg_chunk_secs: f64,
  last_rate: f64,
}

impl ProgressTracker {
  fn new(total: usize) -> Self {
    Self {
      total,
      done: 0,
      start: Instant::now(),
      chunk_start: Instant::now(),
      chunks: 0,
      avg_chunk_secs: 0.0,
      last_rate: 0.0,
    }
  }

  fn begin_chunk(&mut self) {
    self.chunk_start = Instant::now();
  }

  fn complete_chunk(&mut self, length: usize) {
    let secs = self.chunk_start.elapsed().as_secs_f64();
    self.done += length;
    self.chunks += 1;
    self.avg_chunk_secs += (secs - self.avg_chunk_secs) / self.chunks as f64;
    self.last_rate = if secs > 0.0 { length as f64 / secs / 1024.0 } else { 0.0 };
  }

  fn snapshot(&self, pending: f64) -> FlashProgress {
    let seen = (self.done as f64 + pending).min(self.total as f64);
    let elapsed_secs = self.start.elapsed().as_secs_f64();
    let bytes_per_sec = if elapsed_secs > 0.0 { seen / elapsed_secs } else { seen };
    let eta_secs = if bytes_per_sec > 0.0 {
      (self.total as f64 - seen) / bytes_per_sec
    } else {
      0.0
    };

    FlashProgress {
      percent: if self.total > 0 {
        seen / self.total as f64 * 100.0
      } else {
        100.0
      },
      elapsed: elapsed_secs * 1000.0,
      eta: eta_secs * 1000.0,
      rate: self.last_rate,
      avg_chunk_time: self.avg_chunk_secs * 1000.0,
      avg_rate: bytes_per_sec / 1024.0,
    }
  }
}

impl<U: UsbTransport> Fastboot<U> {
  pub async fn write_raw<F: Fn(FlashProgress)>(
    &self,
    start_lba: u64,
    source: &mut dyn PayloadSource,
    size: usize,
    sparse: bool,
    on_progress: F,
  ) -> Result<()> {
    tracing::info!(
      "streaming {} bytes to the user area starting at LBA {} (sparse: {})",
      size,
      start_lba,
      sparse
    );

    let limit = self.max_download_size().await;
    let chunk_bytes = (std::cmp::min(limit, MAX_CHUNK_BYTES) / PART_SECTOR_SIZE).max(1) * PART_SECTOR_SIZE;

    let (mut erased_from, mut erased_to) = (0usize, 0usize);
    if sparse {
      let span_sectors = size.div_ceil(PART_SECTOR_SIZE);
      let first = start_lba as usize;
      let start = first.div_ceil(ERASE_GROUP_SECTORS) * ERASE_GROUP_SECTORS;
      let end = (first + span_sectors) / ERASE_GROUP_SECTORS * ERASE_GROUP_SECTORS;

      if end > start {
        tracing::info!("erasing {} sectors at LBA {} before sparse write", end - start, start);
        match self.erase_raw(start as u64, (end - start) as u64).await {
          Ok(()) => {
            erased_from = (start - first) * PART_SECTOR_SIZE;
            erased_to = (end - first) * PART_SECTOR_SIZE;
          }
          Err(err) => tracing::warn!("erase failed ({err}); writing every chunk instead of skipping zeroes"),
        }
      } else {
        tracing::info!("sparse write spans no whole erase group at LBA {first}; writing in full");
      }
    }

    let mut tracker = ProgressTracker::new(size);
    let mut buffer = vec![0u8; chunk_bytes];
    let mut lba = start_lba;
    let mut offset = 0;

    let result = async {
      while offset < size {
        tracker.begin_chunk();

        let length = std::cmp::min(chunk_bytes, size - offset);
        source.read_exact(&mut buffer[..length]).await?;
        let chunk_start = offset;
        offset += length;
        let sectors = length.div_ceil(PART_SECTOR_SIZE);

        let erased = chunk_start >= erased_from && chunk_start + length <= erased_to;
        if erased && buffer[..length].iter().all(|&byte| byte == 0) {
          tracing::debug!("skipping all-zero chunk at LBA {} (already erased)", lba);
          tracker.complete_chunk(length);
          on_progress(tracker.snapshot(0.0));
          lba += sectors as u64;
          continue;
        }

        let padded = sectors * PART_SECTOR_SIZE;
        buffer[length..padded].fill(0);

        self.set_raw_target(RAW_ALIAS, lba, sectors as u64).await?;
        self
          .download(&buffer[..padded], |sent, _| {
            on_progress(tracker.snapshot(sent as f64 * UPLOAD_SHARE));
          })
          .await?;
        self.flash(RAW_ALIAS).await?;

        tracker.complete_chunk(length);
        on_progress(tracker.snapshot(0.0));
        lba += sectors as u64;
      }

      Ok(())
    }
    .await;

    if let Err(err) = self.clear_raw_target(RAW_ALIAS).await {
      tracing::warn!("could not clear the raw partition alias: {}", err);
    }

    result
  }

  async fn erase_raw(&self, start_lba: u64, sectors: u64) -> Result<()> {
    self.set_raw_target(RAW_ALIAS, start_lba, sectors).await?;
    self.erase(RAW_ALIAS).await
  }

  pub async fn write_by_name<F: Fn(FlashProgress)>(
    &self,
    name: &str,
    source: &mut dyn PayloadSource,
    size: usize,
    on_progress: F,
  ) -> Result<()> {
    let limit = self.max_download_size().await;
    if size > limit {
      return Err(Error::InvalidOperation(format!(
        "{} is {} bytes, larger than the device's {}-byte download buffer; it needs to be a sparse image",
        name, size, limit
      )));
    }

    let mut tracker = ProgressTracker::new(size);
    tracker.begin_chunk();

    let mut data = vec![0u8; size];
    source.read_exact(&mut data).await?;

    self
      .download(&data, |sent, _| {
        on_progress(tracker.snapshot(sent as f64 * UPLOAD_SHARE));
      })
      .await?;
    self.flash(name).await?;

    tracker.complete_chunk(size);
    on_progress(tracker.snapshot(0.0));
    Ok(())
  }

  pub async fn write_env(&self, env: &str, save: bool) -> Result<()> {
    if !env.is_ascii() {
      return Err(Error::InvalidOperation("env data must be ascii".into()));
    }

    let normalised = if env.ends_with('\n') {
      env.to_owned()
    } else {
      format!("{}\n", env)
    };
    let bytes = normalised.as_bytes();

    self.download(bytes, |_, _| {}).await?;
    let output = self
      .console(&format!("env import -t {:#x} {}", FASTBOOT_BUF_ADDR, bytes.len()))
      .await?;
    if complains(&output) {
      return Err(Error::InvalidOperation(format!(
        "the bootloader rejected the environment: {}",
        output.trim()
      )));
    }

    if save {
      let saved = self.console("saveenv").await?;
      if complains(&saved) {
        return Err(Error::InvalidOperation(format!(
          "saving the environment failed: {}",
          saved.trim()
        )));
      }
    }

    Ok(())
  }

  pub async fn unbrick_from(&self, source: &mut dyn PayloadSource, size: usize) -> Result<()> {
    tracing::info!("starting unbrick procedure...");

    self
      .write_raw(0, source, size, true, |progress| {
        tracing::info!(
          "unbrick progress: {:.1}% | elapsed: {:.1}s | eta: {:.1}s | rate: {:.2} KB/s | avg rate: {:.2} KB/s",
          progress.percent,
          progress.elapsed / 1000.0,
          progress.eta / 1000.0,
          progress.rate,
          progress.avg_rate
        );
      })
      .await?;

    tracing::info!("unbrick procedure completed successfully!");
    Ok(())
  }

  #[cfg(not(target_arch = "wasm32"))]
  pub async fn unbrick(&self) -> Result<()> {
    let cursor = std::io::Cursor::new(crate::UNBRICK_BIN_ZIP);
    let mut archive = zip::ZipArchive::new(cursor)?;
    let file = archive.by_name("unbrick.bin")?;

    let size = file.size() as usize;
    let mut source = crate::payload::BlockingSource(file);
    self.unbrick_from(&mut source, size).await
  }
}

pub struct FastbootFlasher<U: UsbTransport, S: PayloadStore> {
  fastboot: Fastboot<U>,
  store: S,
  config: FlashConfig,

  step: usize,
  callback: Option<Callback>,
  force_sparse: bool,
}

async fn bootloader_lead(lba: u32, size: usize, source: &mut dyn PayloadSource) -> Result<Option<Vec<u8>>> {
  use crate::boot_image::{BOOT_IMAGE_BYTES, INFO_SECTOR_BYTES, info_sector, needs_info_sector};

  if lba != 0 || !(INFO_SECTOR_BYTES..=BOOT_IMAGE_BYTES).contains(&size) {
    return Ok(None);
  }

  let mut head = vec![0u8; INFO_SECTOR_BYTES];
  source.read_exact(&mut head).await?;

  if !needs_info_sector(&head) {
    return Ok(Some(head));
  }

  tracing::info!("payload at LBA 0 is a bare bootloader; prepending an info sector so BL2 lands at LBA 1");
  let mut lead = info_sector().to_vec();
  lead.extend_from_slice(&head);
  Ok(Some(lead))
}

struct PrefixedSource<'a> {
  prefix: Vec<u8>,
  offset: usize,
  rest: &'a mut dyn PayloadSource,
}

impl<'a> PrefixedSource<'a> {
  fn new(prefix: Vec<u8>, rest: &'a mut dyn PayloadSource) -> Self {
    Self {
      prefix,
      offset: 0,
      rest,
    }
  }
}

impl PayloadSource for PrefixedSource<'_> {
  fn read_exact<'b>(&'b mut self, buf: &'b mut [u8]) -> Pin<Box<dyn Future<Output = Result<()>> + 'b>> {
    Box::pin(async move {
      let taken = (self.prefix.len() - self.offset).min(buf.len());
      if taken > 0 {
        buf[..taken].copy_from_slice(&self.prefix[self.offset..self.offset + taken]);
        self.offset += taken;
      }
      if taken < buf.len() {
        self.rest.read_exact(&mut buf[taken..]).await?;
      }
      Ok(())
    })
  }
}

impl<U: UsbTransport, S: PayloadStore> FastbootFlasher<U, S> {
  pub fn new(fastboot: Fastboot<U>, store: S, config: FlashConfig, callback: Option<Callback>) -> Self {
    Self {
      fastboot,
      store,
      config,
      step: 0,
      callback,
      force_sparse: false,
    }
  }

  pub fn force_sparse(mut self, force: bool) -> Self {
    self.force_sparse = force;
    self
  }

  pub fn device(&self) -> &Fastboot<U> {
    &self.fastboot
  }

  pub async fn flash(&mut self) -> Result<()> {
    tracing::info!("beginning flashing process!");

    let steps = self.config.steps.clone();
    for step in &steps {
      tracing::trace!("starting step: {:?}", step);

      self.step += 1;
      if let Some(callback) = &self.callback {
        callback(Event::Step(self.step, step.clone()));
      }

      self.run_step(step).await?;
    }

    self.callback = None;
    Ok(())
  }

  async fn write_boot_hwpart(&self, hwpart: u8, image: &[u8]) -> Result<()> {
    let target = match hwpart {
      1 => "mmc0boot0",
      2 => "mmc0boot1",
      other => {
        return Err(Error::InvalidOperation(format!(
          "boot hwpart must be 1 or 2, got {other}"
        )));
      }
    };

    let image = match image.split_at_checked(crate::boot_image::BOOT_HWPART_BYTES) {
      Some((head, tail)) => {
        if tail.iter().any(|&byte| byte != 0) {
          return Err(Error::InvalidOperation(format!(
            "boot image carries content past {} bytes and will not fit a 2 MiB boot hwpart",
            crate::boot_image::BOOT_HWPART_BYTES
          )));
        }
        head
      }
      None => image,
    };

    let mut tracker = ProgressTracker::new(image.len());
    tracker.begin_chunk();

    let progress = progress_reporter(&self.callback);
    self
      .fastboot
      .download(image, |sent, _| {
        progress(tracker.snapshot(sent as f64 * UPLOAD_SHARE));
      })
      .await?;
    self.fastboot.flash(target).await?;

    tracker.complete_chunk(image.len());
    progress(tracker.snapshot(0.0));

    self.fastboot.select_hwpart(0).await
  }

  async fn run_step(&mut self, step: &FlashStep) -> Result<()> {
    match step {
      FlashStep::Log { value } => {
        tracing::info!(">> {:?}", value);
      }

      FlashStep::Wait { value } => match value {
        WaitValue::UserInput { .. } => return Err(Error::UnsupportedFeature(step.to_owned())),
        WaitValue::Time { time } => sleep(Duration::from_millis(*time)).await,
      },

      FlashStep::WriteUserArea { value } => {
        let sparse = value.sparse.unwrap_or(false) || self.force_sparse;
        let (size, mut source) = open_payload(&value.data, &mut self.store).await?;
        let progress = progress_reporter(&self.callback);

        let (lead, size) = match bootloader_lead(value.lba, size, source.as_mut()).await? {
          Some(lead) => {
            let total = size - crate::boot_image::INFO_SECTOR_BYTES + lead.len();
            (lead, total)
          }
          None => (Vec::new(), size),
        };

        let mut source = PrefixedSource::new(lead, source.as_mut());
        self
          .fastboot
          .write_raw(value.lba as u64, &mut source, size, sparse, progress)
          .await?;
      }

      FlashStep::WriteLargeMemory { value } => {
        if !(value.address as usize).is_multiple_of(PART_SECTOR_SIZE) {
          return Err(Error::InvalidOperation(format!(
            "writeLargeMemory address {:#x} is not a multiple of the {}-byte sector size",
            value.address, PART_SECTOR_SIZE
          )));
        }
        let (size, mut source) = open_payload(&value.data, &mut self.store).await?;
        let progress = progress_reporter(&self.callback);
        self
          .fastboot
          .write_raw(
            value.address as u64 / PART_SECTOR_SIZE as u64,
            source.as_mut(),
            size,
            self.force_sparse,
            progress,
          )
          .await?;
      }

      FlashStep::WriteBootPartition { value } => {
        let data = read_payload(&value.data, &mut self.store).await?;
        let image = crate::boot_image::to_boot_image(&data);
        self.write_boot_hwpart(value.hwpart, &image).await?;
      }

      FlashStep::RestorePartition { value } if value.name == "bootloader" => {
        let data = read_payload(&value.data, &mut self.store).await?;
        let image = crate::boot_image::to_boot_image(&data);

        let progress = progress_reporter(&self.callback);
        self
          .fastboot
          .write_raw(0, inline_source(&image).as_mut(), image.len(), false, progress)
          .await?;
        self.write_boot_hwpart(1, &image).await?;
        self.write_boot_hwpart(2, &image).await?;
      }

      FlashStep::RestorePartition { value } => {
        let name = value.name.as_str();
        let (size, mut source) = open_payload(&value.data, &mut self.store).await?;
        let progress = progress_reporter(&self.callback);

        match SUPERBIRD_PARTITIONS.get(name) {
          Some(partition) => {
            let limit = partition.size * PART_SECTOR_SIZE;
            if partition.size > 0 && size > limit {
              return Err(Error::InvalidOperation(format!(
                "{} image is {} bytes but the partition only holds {}",
                name, size, limit
              )));
            }
            self
              .fastboot
              .write_raw(
                partition.offset as u64,
                source.as_mut(),
                size,
                self.force_sparse,
                progress,
              )
              .await?;
          }
          None => {
            tracing::info!("{} is not a stock partition, flashing it by GPT name", name);
            self
              .fastboot
              .write_by_name(name, source.as_mut(), size, progress)
              .await?;
          }
        }
      }

      FlashStep::WriteEnv { value } => {
        let env = read_text(value, &mut self.store).await?;
        self.fastboot.write_env(&env, true).await?;
      }

      FlashStep::Bulkcmd { value } | FlashStep::BulkcmdStat { value, .. } => {
        let Some(translated) = translate_vendor_command(value) else {
          tracing::info!("skipping vendor-only command: {}", value);
          return Ok(());
        };
        if translated != *value {
          tracing::info!("{} -> {}", value, translated);
        }

        let output = self.fastboot.console(&translated).await?;
        if !output.trim().is_empty() {
          tracing::info!(">> {}", output.trim());
        }
        if output.to_lowercase().contains("unknown command") {
          return Err(Error::InvalidOperation(format!(
            "the bootloader does not understand {:?}",
            translated
          )));
        }
      }

      FlashStep::Identify { .. } => {
        let version = self
          .fastboot
          .getvar("version-bootloader")
          .await
          .unwrap_or_else(|_| "unknown".into());
        let product = self
          .fastboot
          .getvar("product")
          .await
          .unwrap_or_else(|_| "unknown".into());
        tracing::info!("device: {}, bootloader {}", product.trim(), version.trim());
      }

      FlashStep::ValidatePartitionSize { value, .. } => {
        let name = value.name.as_str();
        match SUPERBIRD_PARTITIONS.get(name) {
          Some(partition) => tracing::info!("{}: {} sectors", name, partition.size),
          None => {
            let size = self.fastboot.getvar(&format!("partition-size:{}", name)).await?;
            tracing::info!("{}: {}", name, size.trim());
          }
        }
      }

      FlashStep::Bl2Boot { .. }
      | FlashStep::Run { .. }
      | FlashStep::WriteSimpleMemory { .. }
      | FlashStep::WriteAMLCData { .. }
      | FlashStep::GetBootAMLC { .. } => {
        tracing::info!("skipping {:?}: the device is already booted into fastboot", step);
      }

      FlashStep::ReadSimpleMemory { .. } | FlashStep::ReadLargeMemory { .. } => {
        return Err(Error::UnsupportedFeature(step.to_owned()));
      }
    }

    Ok(())
  }

  pub fn num_steps(&self) -> usize {
    self.config.steps.len()
  }

  pub fn current_step(&self) -> usize {
    self.step + 1
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl FastbootFlasher<crate::native::NativeUsb, crate::native::FlashMode> {
  pub async fn from_directory(path: std::path::PathBuf, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new fastboot flasher from directory at {:?}", &path);

    let config = FlashConfig::from_directory(&path)?;
    let fastboot = Fastboot::connect(callback.clone()).await?;

    Ok(Self::new(
      fastboot,
      crate::native::FlashMode::Directory(path),
      config,
      callback,
    ))
  }

  pub async fn from_archive(path: std::path::PathBuf, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new fastboot flasher from archive at {:?}", &path);

    let mut zip = crate::flash::open_zip(&path)?;
    let config = FlashConfig::from_archive(&mut zip)?;
    let fastboot = Fastboot::connect(callback.clone()).await?;

    Ok(Self::new(
      fastboot,
      crate::native::FlashMode::Archive(zip),
      config,
      callback,
    ))
  }

  pub async fn from_json(meta: String, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new fastboot flasher from json string");

    let config = FlashConfig::from_standalone(&meta)?;
    let fastboot = Fastboot::connect(callback.clone()).await?;

    Ok(Self::new(
      fastboot,
      crate::native::FlashMode::Standalone,
      config,
      callback,
    ))
  }

  pub async fn from_stock_directory(path: std::path::PathBuf, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new stock fastboot flasher from directory at {:?}", &path);

    let config = FlashConfig::from_stock()?;
    let fastboot = Fastboot::connect(callback.clone()).await?;

    Ok(Self::new(
      fastboot,
      crate::native::FlashMode::Directory(path),
      config,
      callback,
    ))
  }

  pub async fn from_stock_archive(path: std::path::PathBuf, callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("creating new stock fastboot flasher from archive at {:?}", &path);

    let zip = crate::flash::open_zip(&path)?;
    let config = FlashConfig::from_stock()?;
    let fastboot = Fastboot::connect(callback.clone()).await?;

    Ok(Self::new(
      fastboot,
      crate::native::FlashMode::Archive(zip),
      config,
      callback,
    ))
  }
}

fn progress_reporter(callback: &Option<Callback>) -> impl Fn(FlashProgress) + use<> {
  let callback = callback.clone();
  move |progress: FlashProgress| {
    if let Some(callback) = &callback {
      callback(Event::FlashProgress(progress));
    }
  }
}

fn complains(output: &str) -> bool {
  let lower = output.to_lowercase();
  lower.contains("error") || lower.contains("failed") || lower.contains("unknown command")
}

pub fn translate_vendor_command(command: &str) -> Option<String> {
  let trimmed = command.trim();
  let lower = trimmed.to_lowercase();

  if lower.starts_with("disk_initial")
    || lower.starts_with("amlmmc key")
    || lower.starts_with("amlmmc part")
    || lower.starts_with("amlmmc partition")
  {
    return None;
  }

  if lower.starts_with("mmc dev 1") || lower.starts_with("amlmmc dev 1") {
    return Some("mmc dev 0 0".into());
  }

  if let Some(rest) = trimmed.strip_prefix("amlmmc") {
    return Some(format!("mmc{}", rest));
  }

  Some(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn drops_vendor_only_commands() {
    assert_eq!(translate_vendor_command("amlmmc key"), None);
    assert_eq!(translate_vendor_command("amlmmc part 1"), None);
    assert_eq!(translate_vendor_command("disk_initial 0"), None);
  }

  #[test]
  fn remaps_the_vendor_emmc_device_number() {
    assert_eq!(translate_vendor_command("mmc dev 1").as_deref(), Some("mmc dev 0 0"));
    assert_eq!(translate_vendor_command("amlmmc dev 1").as_deref(), Some("mmc dev 0 0"));
  }

  #[test]
  fn rewrites_amlmmc_to_mmc_and_passes_everything_else_through() {
    assert_eq!(
      translate_vendor_command("amlmmc write bootloader 0x1080000 0x0 0x1000").as_deref(),
      Some("mmc write bootloader 0x1080000 0x0 0x1000")
    );
    assert_eq!(translate_vendor_command("  saveenv  ").as_deref(), Some("saveenv"));
  }

  #[test]
  fn console_output_is_checked_for_complaints() {
    assert!(complains("Unknown command 'env' - try 'help'"));
    assert!(complains("## Error: failed to import"));
    assert!(!complains("Saving Environment to FAT... OK"));
  }

  #[test]
  fn progress_credits_pending_upload_bytes_without_overshooting() {
    let mut tracker = ProgressTracker::new(1000);
    tracker.begin_chunk();
    assert_eq!(tracker.snapshot(250.0).percent, 25.0);

    tracker.complete_chunk(500);
    assert_eq!(tracker.snapshot(0.0).percent, 50.0);
    assert_eq!(tracker.snapshot(9999.0).percent, 100.0);
  }

  #[test]
  fn prefixed_source_splices_across_the_boundary() {
    let rest: Vec<u8> = (0..32u8).collect();
    let mut inner = crate::payload::BlockingSource(std::io::Cursor::new(rest.clone()));
    let mut source = PrefixedSource::new(vec![0xaa; 8], &mut inner);

    let mut out = vec![0u8; 40];
    pollster::block_on(source.read_exact(&mut out[..3])).unwrap();
    pollster::block_on(source.read_exact(&mut out[3..12])).unwrap();
    pollster::block_on(source.read_exact(&mut out[12..])).unwrap();

    assert_eq!(&out[..8], &[0xaa; 8]);
    assert_eq!(&out[8..], &rest[..]);
  }

  fn lead_for(lba: u32, payload: &[u8]) -> Option<Vec<u8>> {
    let mut source = crate::payload::BlockingSource(std::io::Cursor::new(payload.to_vec()));
    pollster::block_on(bootloader_lead(lba, payload.len(), &mut source)).unwrap()
  }

  #[test]
  fn a_bare_bootloader_at_lba_0_gains_an_info_sector() {
    let payload = vec![0x5a; crate::boot_image::BOOT_IMAGE_BYTES];
    let lead = lead_for(0, &payload).expect("should be classified");

    assert_eq!(lead.len(), 2 * crate::boot_image::INFO_SECTOR_BYTES);
    assert_eq!(
      &lead[..crate::boot_image::INFO_SECTOR_BYTES],
      &crate::boot_image::info_sector()
    );
    assert_eq!(
      &lead[crate::boot_image::INFO_SECTOR_BYTES..],
      &payload[..crate::boot_image::INFO_SECTOR_BYTES]
    );
  }

  #[test]
  fn a_whole_disk_image_at_lba_0_is_left_alone() {
    let payload = vec![0x5a; crate::boot_image::BOOT_IMAGE_BYTES + 1];
    assert!(lead_for(0, &payload).is_none());
  }

  #[test]
  fn a_prepared_bootloader_at_lba_0_is_not_shifted() {
    let mut payload = crate::boot_image::info_sector().to_vec();
    payload.resize(crate::boot_image::BOOT_IMAGE_BYTES, 0x5a);

    let lead = lead_for(0, &payload).expect("should be classified");
    assert_eq!(lead.len(), crate::boot_image::INFO_SECTOR_BYTES);
    assert_eq!(&lead[..], &payload[..crate::boot_image::INFO_SECTOR_BYTES]);
  }

  #[test]
  fn writes_away_from_lba_0_are_never_reshaped() {
    let payload = vec![0x5a; 4096];
    assert!(lead_for(1, &payload).is_none());
    assert!(lead_for(438272, &payload).is_none());
  }
}
