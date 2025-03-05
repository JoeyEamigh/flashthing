use rusb::{Context, DeviceHandle, Direction, UsbContext};
use std::{io::Read, sync::Arc, thread::sleep, time::Duration};

use crate::{
  flash::FlashProgress, partitions::PartitionInfo, Callback, Error, Event, Result, ADDR_BL2, ADDR_TMP,
  AMLC_AMLS_BLOCK_LENGTH, AMLC_MAX_BLOCK_LENGTH, AMLC_MAX_TRANSFER_LENGTH, BL2_BIN, BOOTLOADER_BIN, FLAG_KEEP_POWER_ON,
  PART_SECTOR_SIZE, PRODUCT_ID, REQ_BULKCMD, REQ_GET_AMLC, REQ_IDENTIFY_HOST, REQ_READ_MEM, REQ_RUN_IN_ADDR,
  REQ_WRITE_AMLC, REQ_WRITE_MEM, REQ_WR_LARGE_MEM, TRANSFER_BLOCK_SIZE, TRANSFER_SIZE_THRESHOLD, UNBRICK_BIN_ZIP,
  VENDOR_ID,
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct AmlInner {
  handle: DeviceHandle<Context>,
  interface_number: u8,
  endpoint_in: u8,
  endpoint_out: u8,
}

#[derive(Clone)]
pub struct AmlogicSoC {
  inner: Arc<AmlInner>,
}

impl AmlogicSoC {
  pub fn init(callback: Option<Callback>) -> Result<Self> {
    if let Some(callback) = &callback {
      callback(Event::FindingDevice);
    };

    let mode = find_device();
    if let Some(callback) = &callback {
      callback(Event::DeviceMode(mode));
    };

    match mode {
      DeviceMode::Usb => {
        tracing::info!("device booted in usb mode - moving to usb burn mode");
        let device = Self::connect(callback.clone())?;
        if let Some(callback) = &callback {
          callback(Event::Bl2Boot);
        };

        device.bl2_boot(None, None)?;
        drop(device);

        if let Some(callback) = &callback {
          callback(Event::Resetting);
        };

        tracing::debug!("device successfully moved to usb burn mode, sleeping then grabbing new handle");
        sleep(Duration::from_millis(5000));
      }
      DeviceMode::UsbBurn => tracing::info!("device found!"),
      DeviceMode::Normal => {
        tracing::error!(
          "device is booted in normal mode. make sure to power on the car thing while holding buttons 1 & 4"
        );
        return Err(Error::WrongMode);
      }
      DeviceMode::NotFound => {
        tracing::error!("device not found!! make sure to power on the car thing while holding buttons 1 & 4");
        return Err(Error::NotFound);
      }
    };

    let mut attempts = 0;
    while attempts < 3 {
      match Self::connect(callback.clone()) {
        Ok(dev) => return Ok(dev),
        Err(e) => {
          tracing::debug!("failed to connect to device: {}. Attempt {}/3", e, attempts + 1);
          attempts += 1;
          sleep(Duration::from_secs(1));
        }
      }
    }

    Self::connect(callback)
  }

  fn connect(callback: Option<Callback>) -> Result<Self> {
    tracing::debug!("connecting to Amlogic device");
    if let Some(callback) = &callback {
      callback(Event::Connecting);
    };

    let context = Context::new()?;
    let handle = {
      let device = context
        .devices()?
        .iter()
        .find(|device| {
          if let Ok(desc) = device.device_descriptor() {
            desc.vendor_id() == VENDOR_ID && desc.product_id() == PRODUCT_ID
          } else {
            false
          }
        })
        .ok_or_else(|| Error::InvalidOperation("Device not found".into()))?;
      device.open()?
    };

    handle.set_active_configuration(1)?;
    let interface_number: u8 = 0;
    handle.claim_interface(interface_number)?;

    let device = handle.device();
    let config_desc = device.active_config_descriptor()?;
    let interface = config_desc
      .interfaces()
      .find(|i| i.number() == interface_number)
      .ok_or_else(|| Error::InvalidOperation("Interface not found".into()))?;
    let descriptor = interface
      .descriptors()
      .next()
      .ok_or_else(|| Error::InvalidOperation("No alt setting".into()))?;
    let mut endpoint_in = None;
    let mut endpoint_out = None;
    for ep in descriptor.endpoint_descriptors() {
      match ep.direction() {
        Direction::In => endpoint_in = Some(ep.address()),
        Direction::Out => endpoint_out = Some(ep.address()),
      }
    }
    let endpoint_in = endpoint_in.ok_or_else(|| Error::InvalidOperation("IN endpoint not found".into()))?;
    let endpoint_out = endpoint_out.ok_or_else(|| Error::InvalidOperation("OUT endpoint not found".into()))?;
    tracing::info!("device connected, claiming interface {}", interface_number);
    if let Some(callback) = &callback {
      callback(Event::Connected);
    };

    Ok(Self {
      inner: Arc::new(AmlInner {
        handle,
        interface_number,
        endpoint_in,
        endpoint_out,
      }),
    })
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn write_simple_memory(&self, address: u32, data: &[u8]) -> Result<()> {
    tracing::debug!(
      "writing simple memory at address: {:#X}, length: {}",
      address,
      data.len()
    );
    if data.len() > 64 {
      return Err(Error::InvalidOperation("Maximum size of 64 bytes".into()));
    }
    let value = (address >> 16) as u16;
    let index = (address & 0xffff) as u16;
    self
      .inner
      .handle
      .write_control(0x40, REQ_WRITE_MEM, value, index, data, COMMAND_TIMEOUT)?;
    tracing::trace!(
      "write_control completed for write_simple_memory at address: {:#X}",
      address
    );
    Ok(())
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn write_memory(&self, address: u32, data: &[u8]) -> Result<()> {
    tracing::debug!(
      "writing memory starting at address: {:#X} with total length: {}",
      address,
      data.len()
    );
    let mut offset = 0;
    let length = data.len();
    while offset < length {
      let chunk_size = std::cmp::min(64, length - offset);
      self.write_simple_memory(address + offset as u32, &data[offset..offset + chunk_size])?;
      tracing::trace!(
        "chunk written for write_memory at address: {:#X}, new offset: {}",
        address,
        offset + chunk_size
      );
      offset += chunk_size;
    }
    Ok(())
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn read_simple_memory(&self, address: u32, length: usize) -> Result<Vec<u8>> {
    tracing::debug!(
      "reading simple memory at address: {:#X} with length: {}",
      address,
      length
    );
    if length == 0 {
      return Ok(vec![]);
    }
    if length > 64 {
      return Err(Error::InvalidOperation("Maximum size of 64 bytes".into()));
    }
    let value = (address >> 16) as u16;
    let index = (address & 0xffff) as u16;
    let mut buf = vec![0u8; length];
    let read = self
      .inner
      .handle
      .read_control(0xC0, REQ_READ_MEM, value, index, &mut buf, COMMAND_TIMEOUT)?;
    tracing::trace!(
      "read_control completed for read_simple_memory at address: {:#X}, bytes read: {}",
      address,
      read
    );
    if read != length {
      return Err(Error::InvalidOperation("Incomplete read".into()));
    }
    Ok(buf)
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn read_memory(&self, address: u32, length: usize) -> Result<Vec<u8>> {
    tracing::debug!("reading memory at address: {:#X} with length: {}", address, length);
    let mut data = vec![0u8; length];
    let mut offset = 0;
    while offset < length {
      let read_length = std::cmp::min(64, length - offset);
      let chunk = self.read_simple_memory(address + offset as u32, read_length)?;
      data[offset..offset + read_length].copy_from_slice(&chunk);
      tracing::trace!(
        "chunk read for read_memory at address: {:#X}, offset: {}",
        address,
        offset
      );
      offset += read_length;
    }
    Ok(data)
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn run(&self, address: u32, keep_power: Option<bool>) -> Result<()> {
    let keep_power = keep_power.unwrap_or(true);
    tracing::debug!("running at address: {:#X} with keep_power: {}", address, keep_power);
    let data = if keep_power {
      address | FLAG_KEEP_POWER_ON
    } else {
      address
    };
    let buffer = data.to_le_bytes();
    let value = (address >> 16) as u16;
    let index = (address & 0xffff) as u16;
    self
      .inner
      .handle
      .write_control(0x40, REQ_RUN_IN_ADDR, value, index, &buffer, COMMAND_TIMEOUT)?;
    tracing::trace!("run command sent at address: {:#X}", address);
    Ok(())
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn identify(&self) -> Result<String> {
    tracing::debug!("identifying device");
    let mut buf = [0u8; 8];
    let read = self
      .inner
      .handle
      .read_control(0xC0, REQ_IDENTIFY_HOST, 0, 0, &mut buf, COMMAND_TIMEOUT)?;
    tracing::trace!("identify response received: {:?} ({} bytes)", &buf, read);
    if read != 8 {
      return Err(Error::InvalidOperation("Failed to read identify data".into()));
    }
    Ok(String::from_utf8(buf.to_vec())?)
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn write_large_memory(
    &self,
    memory_address: u32,
    data: &[u8],
    block_length: usize,
    append_zeros: bool,
  ) -> Result<()> {
    tracing::debug!(
      "writing large memory to address: {:#X} with data length: {}",
      memory_address,
      data.len()
    );

    let mut data_vec = data.to_vec();
    if append_zeros {
      let remainder = data_vec.len() % block_length;
      if remainder != 0 {
        let padding = block_length - remainder;
        data_vec.extend(vec![0u8; padding]);
      }
    } else if data_vec.len() % block_length != 0 {
      return Err(Error::InvalidOperation(
        "Large Data must be a multiple of block length".into(),
      ));
    }

    let total_bytes = data_vec.len() as u32;
    let block_count = (data_vec.len() / block_length) as u16;
    let mut control_data = Vec::with_capacity(16);
    control_data.extend_from_slice(&memory_address.to_le_bytes());
    control_data.extend_from_slice(&total_bytes.to_le_bytes());
    control_data.extend_from_slice(&0u32.to_le_bytes());
    control_data.extend_from_slice(&0u32.to_le_bytes());

    tracing::trace!("writing control data: {:?}", &control_data);
    self.inner.handle.write_control(
      0x40,
      REQ_WR_LARGE_MEM,
      block_length as u16,
      block_count,
      &control_data,
      COMMAND_TIMEOUT,
    )?;

    let mut data_offset = 0;
    while data_offset < data_vec.len() {
      let end = data_offset + block_length;
      let chunk = &data_vec[data_offset..end];
      tracing::trace!(target: "flashthing::aml::write_large_memory", "writing actual data from offset: {:#X}", &data_offset);

      self
        .inner
        .handle
        .write_bulk(self.inner.endpoint_out, chunk, Duration::from_millis(2000))?;

      tracing::trace!(target: "flashthing::aml::write_large_memory", "wrote actual data from offset: {:#X}", &data_offset);

      data_offset += block_length;
    }

    Ok(())
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn write_large_memory_to_disk<R: std::io::Read, F: Fn(FlashProgress)>(
    &self,
    disk_address: u32,
    reader: &mut R,
    data_size: usize,
    block_length: usize,
    append_zeros: bool,
    progress_callback: F,
  ) -> Result<()> {
    tracing::debug!("streaming {} bytes to disk address: {:#X}", data_size, disk_address);

    let start_time = std::time::Instant::now();
    let mut total_chunks = 0;
    let mut avg_chunk_time_secs = 0.0;

    // needed for write operations
    self.bulkcmd("mmc dev 1")?;
    self.bulkcmd("amlmmc key")?;

    let total_len = data_size;
    let max_bytes_per_transfer = TRANSFER_SIZE_THRESHOLD;
    let mut offset = 0;
    let mut buffer = vec![0u8; max_bytes_per_transfer];

    while offset < total_len {
      let chunk_start_time = std::time::Instant::now();

      let remaining = total_len - offset;
      let write_length = std::cmp::min(remaining, max_bytes_per_transfer);

      let data_slice = &mut buffer[..write_length];
      reader.read_exact(data_slice)?;

      self.write_large_memory(ADDR_TMP, &buffer[..write_length], block_length, append_zeros)?;

      let start_time_cmd = std::time::Instant::now();
      let mut retries = 0;
      let max_retries = 3;

      loop {
        match self.bulkcmd(&format!(
          "mmc write {:#X} {:#X} {:#X}",
          ADDR_TMP,
          (disk_address as usize + offset) / 512,
          write_length / 512
        )) {
          Ok(_) => {
            let elapsed = start_time_cmd.elapsed();
            if elapsed > Duration::from_millis(3000) {
              tracing::debug!("mmc write command took {}ms, cooling down for 5s", elapsed.as_millis());
              sleep(Duration::from_secs(5));
            }
            break;
          }
          Err(e) => {
            retries += 1;
            if retries >= max_retries {
              return Err(e);
            }
            sleep(Duration::from_secs(5)); // cooldown after error
          }
        }
      }

      let chunk_time = chunk_start_time.elapsed();
      let chunk_time_secs = chunk_time.as_secs_f64();
      total_chunks += 1;
      if total_chunks == 1 {
        avg_chunk_time_secs = chunk_time_secs;
      } else {
        avg_chunk_time_secs = avg_chunk_time_secs + (chunk_time_secs - avg_chunk_time_secs) / total_chunks as f64;
      }

      offset += write_length;
      let progress_percent = offset as f64 / total_len as f64 * 100.0;

      let elapsed = start_time.elapsed();
      let elapsed_secs = elapsed.as_secs_f64();
      let bytes_per_sec = if elapsed_secs > 0.0 {
        offset as f64 / elapsed_secs
      } else {
        offset as f64
      };

      let remaining_bytes = total_len - offset;
      let eta_secs = if bytes_per_sec > 0.0 {
        remaining_bytes as f64 / bytes_per_sec
      } else {
        0.0
      };

      tracing::info!(
        "progress: {:.1}% | elapsed: {:.1}s | eta: {:.1}s | rate: {:.2} KB/s | avg chunk: {:.1}s | avg rate: {:.2} KB/s",
        progress_percent,
        elapsed_secs,
        eta_secs,
        write_length as f64 / chunk_time_secs / 1024.0,
        avg_chunk_time_secs,
        bytes_per_sec / 1024.0
      );

      progress_callback(FlashProgress {
        percent: progress_percent,
        elapsed: elapsed_secs * 1000.0,
        eta: eta_secs * 1000.0,
        rate: write_length as f64 / chunk_time_secs / 1024.0,
        avg_chunk_time: avg_chunk_time_secs * 1000.0,
        avg_rate: bytes_per_sec / 1024.0,
      });
    }

    let total_elapsed = start_time.elapsed();
    let total_elapsed_secs = total_elapsed.as_secs_f64();
    let avg_bytes_per_sec = if total_elapsed_secs > 0.0 {
      total_len as f64 / total_elapsed_secs
    } else {
      total_len as f64
    };

    tracing::info!(
      "Transfer complete | total time: {:?} | avg rate: {:.2} KB/s",
      total_elapsed,
      avg_bytes_per_sec / 1024.0
    );

    Ok(())
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn write_amlc_data(&self, offset: u32, data: &[u8]) -> Result<()> {
    tracing::debug!("writing amlc data at offset: {:#X} with length: {}", offset, data.len());

    self.inner.handle.write_control(
      0x40,
      REQ_WRITE_AMLC,
      (offset / AMLC_AMLS_BLOCK_LENGTH as u32) as u16,
      (data.len() - 1) as u16,
      &[],
      COMMAND_TIMEOUT,
    )?;
    tracing::trace!("amlc header sent for data write at offset: {:#X}", offset);

    let max_chunk_size = AMLC_MAX_BLOCK_LENGTH;
    let mut data_offset = 0;
    let write_length = data.len();
    let mut remaining = write_length;

    let bulk_timeout = Duration::from_millis(1000);

    while remaining > 0 {
      let block_length = std::cmp::min(remaining, max_chunk_size);
      let chunk = &data[data_offset..data_offset + block_length];

      let mut retries = 0;
      let max_retries = 3;
      let mut success = false;

      while !success && retries < max_retries {
        match self
          .inner
          .handle
          .write_bulk(self.inner.endpoint_out, chunk, bulk_timeout)
        {
          Ok(written) => {
            if written == block_length {
              success = true;
              tracing::trace!(
                "bulk write in AMLC data, data_offset: {}, chunk: {}",
                data_offset,
                block_length
              );
            } else {
              tracing::warn!(
                "Incomplete bulk write: {} of {} bytes. Retry {}/{}",
                written,
                block_length,
                retries + 1,
                max_retries
              );
              retries += 1;
              sleep(Duration::from_millis(100));
            }
          }
          Err(e) => {
            tracing::warn!("Error in bulk write: {}. Retry {}/{}", e, retries + 1, max_retries);
            retries += 1;
            sleep(Duration::from_millis(100));

            if retries >= max_retries {
              return Err(Error::UsbError(e));
            }
          }
        }
      }

      data_offset += block_length;
      remaining -= block_length;

      sleep(Duration::from_millis(10));
    }

    let mut ack_buf = [0u8; 16];
    let mut retries = 0;
    let max_retries = 3;
    let mut read = 0;

    while retries < max_retries {
      match self
        .inner
        .handle
        .read_bulk(self.inner.endpoint_in, &mut ack_buf, bulk_timeout)
      {
        Ok(bytes_read) => {
          read = bytes_read;
          if read >= 4 {
            break;
          }
          tracing::warn!("short ack read: {} bytes. retry {}/{}", read, retries + 1, max_retries);
        }
        Err(e) => {
          tracing::warn!("error reading ack: {}. retry {}/{}", e, retries + 1, max_retries);
        }
      }
      retries += 1;
      sleep(Duration::from_millis(100));
    }

    tracing::trace!("received amlc ack: {:?} ({} bytes)", &ack_buf[..read], read);

    if read < 4 {
      return Err(Error::InvalidOperation("no acknowledgment received".into()));
    }

    let ack = String::from_utf8(ack_buf[0..4].to_vec())?;
    if ack != "OKAY" {
      return Err(Error::InvalidOperation(format!("invalid amlc data write ack: {}", ack)));
    }

    Ok(())
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn write_amlc_data_packet(&self, seq: u8, amlc_offset: u32, data: &[u8]) -> Result<()> {
    tracing::debug!("writing amlc data packet, seq: {}, offset: {:#X}", seq, amlc_offset);

    let data_len = data.len();
    let max_transfer_length = AMLC_MAX_TRANSFER_LENGTH;
    let transfer_count = data_len.div_ceil(max_transfer_length);

    if data_len > 0 {
      let mut offset = 0;
      for i in 0..transfer_count {
        let write_length = std::cmp::min(max_transfer_length, data_len - offset);
        tracing::trace!(
          "sending amlc data packet chunk {}/{} at offset: {} with length: {}",
          i + 1,
          transfer_count,
          offset,
          write_length
        );

        self.write_amlc_data(offset as u32, &data[offset..offset + write_length])?;
        sleep(Duration::from_millis(50));

        offset += write_length;
      }
    }

    let checksum = self.amlc_checksum(data)?;

    let mut amlc_header = [0u8; 16];
    amlc_header[0..4].copy_from_slice(b"AMLS"); // ! This is AMLS not AMLC for final packet - do not change
    amlc_header[4] = seq;
    amlc_header[8..12].copy_from_slice(&checksum.to_le_bytes());

    let mut amlc_data = vec![0u8; AMLC_AMLS_BLOCK_LENGTH];
    amlc_data[0..16].copy_from_slice(&amlc_header);

    if data.len() > 16 {
      let copy_len = std::cmp::min(AMLC_AMLS_BLOCK_LENGTH - 16, data.len() - 16);
      amlc_data[16..16 + copy_len].copy_from_slice(&data[16..16 + copy_len]);
    }

    tracing::debug!("sending AMLS block with seq {} to offset {:#X}", seq, amlc_offset);
    self.write_amlc_data(amlc_offset, &amlc_data)?;

    Ok(())
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn get_boot_amlc(&self) -> Result<(u32, u32)> {
    tracing::debug!("getting boot amlc data");
    self.inner.handle.write_control(
      0x40,
      REQ_GET_AMLC,
      AMLC_AMLS_BLOCK_LENGTH as u16,
      0,
      &[],
      COMMAND_TIMEOUT,
    )?;
    tracing::trace!("amlc get request sent");
    let mut buf = vec![0u8; AMLC_AMLS_BLOCK_LENGTH];
    let read = self
      .inner
      .handle
      .read_bulk(self.inner.endpoint_in, &mut buf, Duration::from_secs(2))?;
    tracing::trace!("amlc data received, length: {}", read);
    if read < AMLC_AMLS_BLOCK_LENGTH {
      return Err(Error::InvalidOperation("No amlc data received".into()));
    }
    let tag = String::from_utf8(buf[0..4].to_vec())?;
    if tag != "AMLC" {
      return Err(Error::InvalidOperation(format!("invalid amlc request: {}", tag)));
    }
    let length = u32::from_le_bytes(buf[8..12].try_into()?);
    let offset = u32::from_le_bytes(buf[12..16].try_into()?);
    let mut ack = [0u8; 16];
    ack[..4].copy_from_slice(b"OKAY");
    self
      .inner
      .handle
      .write_bulk(self.inner.endpoint_out, &ack, Duration::from_secs(2))?;
    tracing::trace!("acknowledgment sent for amlc data");
    Ok((length, offset))
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  fn amlc_checksum(&self, data: &[u8]) -> Result<u32> {
    let mut checksum: u32 = 0;
    let mut offset = 0;
    let uint32_max = u32::MAX as u64 + 1;
    while offset < data.len() {
      let remaining = data.len() - offset;
      let val: u32 = if remaining >= 4 {
        let v = u32::from_le_bytes(data[offset..offset + 4].try_into()?);
        offset += 4;
        v
      } else if remaining >= 3 {
        let mut temp = [0u8; 4];
        temp[..remaining].copy_from_slice(&data[offset..]);
        offset += 3;
        u32::from_le_bytes(temp) & 0xffffff
      } else if remaining >= 2 {
        let v = u16::from_le_bytes(data[offset..offset + 2].try_into()?) as u32;
        offset += 2;
        v
      } else {
        let v = data[offset] as u32;
        offset += 1;
        v
      };
      checksum = ((checksum as u64 + (val as i64).unsigned_abs()) % uint32_max) as u32;
    }
    Ok(checksum)
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn bl2_boot(&self, bl2: Option<&[u8]>, bootloader: Option<&[u8]>) -> Result<()> {
    let bl2 = bl2.unwrap_or(BL2_BIN);
    let bootloader = bootloader.unwrap_or(BOOTLOADER_BIN);

    tracing::info!("sending bl2 binary to address {:#X}...", ADDR_BL2);
    self.write_large_memory(ADDR_BL2, bl2, 4096, true)?;

    tracing::info!("booting from bl2...");
    self.run(ADDR_BL2, Some(true))?;

    tracing::debug!("waiting for bootloader to initialize...");
    sleep(Duration::from_secs(2));

    let mut prev_length: u32 = 0;
    let mut prev_offset: u32 = 0;
    let mut seq: u8 = 0;

    let max_retries = 3;
    let max_iterations = 50;
    let mut iterations = 0;

    tracing::info!("starting AMLC data transfer sequence...");

    loop {
      if iterations >= max_iterations {
        return Err(Error::InvalidOperation("maximum iterations reached in bl2_boot".into()));
      }
      iterations += 1;

      let mut retry_count = 0;
      let (length, offset) = loop {
        match self.get_boot_amlc() {
          Ok(result) => break result,
          Err(e) => {
            retry_count += 1;
            if retry_count >= max_retries {
              tracing::error!("failed to get boot amlc data after {} attempts: {}", max_retries, e);
              return Err(e);
            }
            tracing::warn!("failed to get boot amlc, retry {}/{}: {}", retry_count, max_retries, e);
            sleep(Duration::from_millis(500));
          }
        }
      };

      tracing::debug!("amlc request: dataSize={}, offset={}, seq={}", length, offset, seq);

      if length == prev_length && offset == prev_offset {
        tracing::debug!("amlc transfer complete - received same length/offset twice");
        break;
      }

      prev_length = length;
      prev_offset = offset;

      if offset as usize >= bootloader.len() {
        tracing::warn!(
          "amlc requested offset {} exceeds bootloader size {}",
          offset,
          bootloader.len()
        );
        let empty_slice = &[];
        self.write_amlc_data_packet(seq, offset, empty_slice)?;
      } else {
        let actual_length = std::cmp::min(length as usize, bootloader.len() - offset as usize);
        let data_slice = &bootloader[offset as usize..offset as usize + actual_length];

        tracing::debug!("sending {} bytes at offset {} with seq {}", actual_length, offset, seq);
        self.write_amlc_data_packet(seq, offset, data_slice)?;
      }

      seq = seq.wrapping_add(1);
      sleep(Duration::from_millis(100));
    }

    tracing::info!("bl2 boot sequence completed successfully!");
    Ok(())
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn bulkcmd(&self, command: &str) -> Result<String> {
    tracing::debug!("sending bulk command: {:?}", command);
    let mut command = command.as_bytes().to_vec();
    command.push(0x00);
    self
      .inner
      .handle
      .write_control(0x40, REQ_BULKCMD, 0, 0, &command, COMMAND_TIMEOUT)?;
    tracing::trace!("bulk command control write completed");

    let mut buf = vec![0u8; 512];
    let read = self
      .inner
      .handle
      .read_bulk(self.inner.endpoint_in, &mut buf, COMMAND_TIMEOUT)?;
    tracing::trace!("bulk command response received, length: {}", read);

    if read == 0 {
      return Err(Error::InvalidOperation("No response received for bulk command".into()));
    }
    let slice = &buf[..read];
    let start = slice.iter().position(|&b| b != 0).unwrap_or(0);
    let end = slice.iter().rposition(|&b| b != 0).map(|pos| pos + 1).unwrap_or(0);
    let trimmed = &slice[start..end];
    let response = String::from_utf8(trimmed.to_vec())?;
    if !response.to_lowercase().contains("success") {
      return Err(Error::InvalidOperation(format!(
        "Bulk command failed, response did not contain 'success': {}",
        response
      )));
    }
    Ok(response)
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn validate_partition_size(&self, part_name: &str, part_info: &PartitionInfo) -> Result<usize> {
    tracing::debug!("validating partition size for partition: {}", part_name);

    if part_name == "cache" {
      tracing::warn!("The \"cache\" partition is zero-length on superbird, you cannot read or write to it!");
      return Err(Error::InvalidOperation("Cache partition is zero-length".into()));
    }

    if part_name == "reserved" {
      tracing::warn!("The \"reserved\" partition cannot be read or written!");
      return Err(Error::InvalidOperation("Reserved partition cannot be accessed".into()));
    }

    let part_size = part_info.size * PART_SECTOR_SIZE;
    tracing::info!(
      "Validating size of partition: {} size: {:#x} {}MB - ...",
      part_name,
      part_size,
      part_size / 1024 / 1024
    );

    // Try to read the last sector
    match self.bulkcmd(&format!(
      "amlmmc read {} {:#x} {:#x} {:#x}",
      part_name,
      ADDR_TMP,
      part_size - PART_SECTOR_SIZE,
      PART_SECTOR_SIZE
    )) {
      Ok(_) => {
        tracing::info!(
          "Validating size of partition: {} size: {:#x} {}MB - OK",
          part_name,
          part_size,
          part_size / 1024 / 1024
        );
        Ok(part_size)
      }
      Err(e) => {
        tracing::warn!(
          "Validating size of partition: {} size: {:#x} {}MB - FAIL",
          part_name,
          part_size,
          part_size / 1024 / 1024
        );

        // Check if it's the data partition which can have an alternate size
        if part_name == "data" && part_info.size_alt.is_some() {
          let alt_size = part_info.size_alt.unwrap() * PART_SECTOR_SIZE;
          tracing::info!(
            "Failed while fetching last chunk of partition: {}, trying alternate size: {:#x} {}MB",
            part_name,
            alt_size,
            alt_size / 1024 / 1024
          );

          tracing::info!(
            "Validating size of partition: {} size: {:#x} {}MB - ...",
            part_name,
            alt_size,
            alt_size / 1024 / 1024
          );

          match self.bulkcmd(&format!(
            "amlmmc read {} {:#x} {:#x} {:#x}",
            part_name,
            ADDR_TMP,
            alt_size - PART_SECTOR_SIZE,
            PART_SECTOR_SIZE
          )) {
            Ok(_) => {
              tracing::info!(
                "Validating size of partition: {} size: {:#x} {}MB - OK",
                part_name,
                alt_size,
                alt_size / 1024 / 1024
              );
              Ok(alt_size)
            }
            Err(e2) => {
              tracing::error!(
                "Validating size of partition: {} size: {:#x} {}MB - FAIL",
                part_name,
                alt_size,
                alt_size / 1024 / 1024
              );
              tracing::error!(
                "Failed while validating size of partition: {}, is partition size {:#x} correct? error: {}",
                part_name,
                alt_size,
                e2
              );
              Err(e2)
            }
          }
        } else {
          tracing::error!(
            "Failed while validating size of partition: {}, is partition size {:#x} correct? error: {}",
            part_name,
            part_size,
            e
          );
          Err(e)
        }
      }
    }
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn restore_partition<R: Read, F: Fn(FlashProgress)>(
    &self,
    part_name: &str,
    part_size: usize,
    mut reader: R,
    file_size: usize,
    progress_callback: F,
  ) -> Result<()> {
    tracing::debug!("restoring partition: {} with file size: {}", part_name, file_size);

    let adjusted_part_size = if part_name == "bootloader" {
      // Bootloader is only 2MB, though dumps may be zero-padded to 4MB
      2 * 1024 * 1024
    } else {
      part_size
    };

    if file_size > adjusted_part_size && part_name != "bootloader" {
      return Err(Error::InvalidOperation(format!(
        "file is larger than target partition: {} bytes vs {} bytes",
        file_size, adjusted_part_size
      )));
    }

    let start_time = std::time::Instant::now();
    let mut total_chunks = 0;
    let mut avg_chunk_time_secs = 0.0;

    self.bulkcmd("amlmmc key")?;

    let total_len = file_size;
    let max_bytes_per_transfer = TRANSFER_SIZE_THRESHOLD;
    let mut offset = 0;
    let mut buffer = vec![0u8; max_bytes_per_transfer];

    while offset < total_len {
      let chunk_start_time = std::time::Instant::now();

      let remaining = total_len - offset;
      let write_length = std::cmp::min(remaining, max_bytes_per_transfer);

      let data_slice = &mut buffer[..write_length];
      reader.read_exact(data_slice)?;

      self.write_large_memory(ADDR_TMP, &buffer[..write_length], TRANSFER_BLOCK_SIZE, true)?;

      let start_time_cmd = std::time::Instant::now();
      let mut retries = 0;
      let max_retries = 3;

      // Special handling for bootloader partition
      if part_name == "bootloader" {
        // Bootloader writes always cause timeout - this is expected
        match self.bulkcmd(&format!(
          "amlmmc write {} {:#x} {:#x} {:#x}",
          part_name, ADDR_TMP, offset, write_length
        )) {
          Ok(_) => tracing::debug!("bootloader write succeeded unexpectedly"),
          Err(e) => tracing::debug!("expected timeout for bootloader write: {}", e),
        }
        sleep(Duration::from_secs(2)); // Allow time for write to complete
      } else {
        loop {
          match self.bulkcmd(&format!(
            "amlmmc write {} {:#x} {:#x} {:#x}",
            part_name, ADDR_TMP, offset, write_length
          )) {
            Ok(_) => {
              let elapsed = start_time_cmd.elapsed();
              if elapsed > Duration::from_millis(3000) {
                tracing::debug!("write command took {}ms, cooling down for 5s", elapsed.as_millis());
                sleep(Duration::from_secs(5));
              }
              break;
            }
            Err(e) => {
              retries += 1;
              if retries >= max_retries {
                return Err(e);
              }
              tracing::warn!("write command failed, retrying ({}/{}): {}", retries, max_retries, e);
              sleep(Duration::from_secs(5)); // cooldown after error
            }
          }
        }
      }

      let chunk_time = chunk_start_time.elapsed();
      let chunk_time_secs = chunk_time.as_secs_f64();
      total_chunks += 1;
      if total_chunks == 1 {
        avg_chunk_time_secs = chunk_time_secs;
      } else {
        avg_chunk_time_secs = avg_chunk_time_secs + (chunk_time_secs - avg_chunk_time_secs) / total_chunks as f64;
      }

      offset += write_length;
      let progress_percent = offset as f64 / total_len as f64 * 100.0;

      let elapsed = start_time.elapsed();
      let elapsed_secs = elapsed.as_secs_f64();
      let bytes_per_sec = if elapsed_secs > 0.0 {
        offset as f64 / elapsed_secs
      } else {
        offset as f64
      };

      let remaining_bytes = total_len - offset;
      let eta_secs = if bytes_per_sec > 0.0 {
        remaining_bytes as f64 / bytes_per_sec
      } else {
        0.0
      };

      tracing::info!(
        "progress: {:.1}% | elapsed: {:.1}s | eta: {:.1}s | rate: {:.2} KB/s | avg chunk: {:.1}s | avg rate: {:.2} KB/s",
        progress_percent,
        elapsed_secs,
        eta_secs,
        write_length as f64 / chunk_time_secs / 1024.0,
        avg_chunk_time_secs,
        bytes_per_sec / 1024.0
      );

      progress_callback(FlashProgress {
        percent: progress_percent,
        elapsed: elapsed_secs * 1000.0,
        eta: eta_secs * 1000.0,
        rate: write_length as f64 / chunk_time_secs / 1024.0,
        avg_chunk_time: avg_chunk_time_secs * 1000.0,
        avg_rate: bytes_per_sec / 1024.0,
      });
    }

    let total_elapsed = start_time.elapsed();
    let total_elapsed_secs = total_elapsed.as_secs_f64();
    let avg_bytes_per_sec = if total_elapsed_secs > 0.0 {
      total_len as f64 / total_elapsed_secs
    } else {
      total_len as f64
    };

    tracing::info!(
      "partition restore complete | total time: {:?} | avg rate: {:.2} KB/s",
      total_elapsed,
      avg_bytes_per_sec / 1024.0
    );

    Ok(())
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub fn unbrick(&self) -> Result<()> {
    tracing::info!("starting unbrick procedure...");

    let cursor = std::io::Cursor::new(UNBRICK_BIN_ZIP);

    let mut archive = match zip::ZipArchive::new(cursor) {
      Ok(archive) => archive,
      Err(e) => {
        tracing::error!("failed to open unbrick zip archive: {}", e);
        return Err(Error::Zip(e));
      }
    };

    let mut file = match archive.by_name("unbrick.bin") {
      Ok(file) => file,
      Err(e) => {
        tracing::error!("failed to find unbrick.bin in zip archive: {}", e);
        return Err(Error::Zip(e));
      }
    };

    let file_size = file.size() as usize;
    self.write_large_memory_to_disk(0, &mut file, file_size, TRANSFER_BLOCK_SIZE, true, |progress| {
      tracing::info!(
        "unbrick progress: {:.1}% | elapsed: {:.1}s | eta: {:.1}s | rate: {:.2} KB/s | avg rate: {:.2} KB/s",
        progress.percent,
        progress.elapsed,
        progress.eta,
        progress.rate,
        progress.avg_rate
      );
    })?;

    tracing::info!("unbrick procedure completed successfully!");
    Ok(())
  }

  /// Set up host environment for USB access
  pub fn host_setup() -> Result<()> {
    #[cfg(target_os = "linux")]
    crate::setup::setup_host_linux()?;

    Ok(())
  }
}

impl Drop for AmlogicSoC {
  fn drop(&mut self) {
    match self.inner.handle.release_interface(self.inner.interface_number) {
      Ok(()) => tracing::trace!("successfully dropped usb interface"),
      Err(err) => tracing::warn!("failed to release usb interface: {:?}", err),
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeviceMode {
  Normal,
  Usb,
  UsbBurn,
  NotFound,
}

#[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
fn find_device() -> DeviceMode {
  let context = match Context::new() {
    Ok(c) => c,
    Err(_) => return DeviceMode::NotFound,
  };
  let devices = match context.devices() {
    Ok(d) => d,
    Err(_) => return DeviceMode::NotFound,
  };
  for device in devices.iter() {
    let desc = match device.device_descriptor() {
      Ok(d) => d,
      Err(_) => continue,
    };
    // Match normal mode: vendor=0x18d1, product=0x4e40
    if desc.vendor_id() == 0x18d1 && desc.product_id() == 0x4e40 {
      tracing::debug!("Found device booted normally, with USB Gadget (adb/usbnet) enabled");
      return DeviceMode::Normal;
    }
    // Match USB burn/usb mode: vendor=0x1b8e, product=0xc003
    if desc.vendor_id() == 0x1b8e && desc.product_id() == 0xc003 {
      // Attempt to open device and read product string
      match device.open() {
        Ok(handle) => {
          // Common language ID
          let lang = handle.read_languages(COMMAND_TIMEOUT).unwrap_or_default();
          let Some(lang) = lang.first() else {
            tracing::debug!("Found device in USB Burn Mode (unable to read product string)");
            return DeviceMode::UsbBurn;
          };

          let prod = handle
            .read_product_string(*lang, &desc, Duration::from_millis(100))
            .ok();
          if prod.as_deref() == Some("GX-CHIP") {
            tracing::debug!("Found device booted in USB Mode (buttons 1 & 4 held at boot)");
            return DeviceMode::Usb;
          } else {
            tracing::debug!("Found device booted in USB Burn Mode (ready for commands)");
            return DeviceMode::UsbBurn;
          }
        }
        Err(_) => {
          tracing::debug!("Found device in USB Burn Mode (unable to read product string)");
          return DeviceMode::UsbBurn;
        }
      }
    }
  }

  tracing::debug!("No device found!");
  DeviceMode::NotFound
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_amlogic_soc_connect() {
    let soc = AmlogicSoC::init(None);
    // This test will only pass if the device is connected
    assert!(soc.is_ok());
  }
}
