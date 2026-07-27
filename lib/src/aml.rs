use std::{sync::Arc, time::Duration};

use crate::{
  ADDR_BL2, ADDR_TMP, AMLC_AMLS_BLOCK_LENGTH, AMLC_MAX_BLOCK_LENGTH, AMLC_MAX_TRANSFER_LENGTH, Callback,
  ERASE_GROUP_SECTORS, Error, Event, FLAG_KEEP_POWER_ON, PART_SECTOR_SIZE, REQ_BULKCMD, REQ_GET_AMLC,
  REQ_IDENTIFY_HOST, REQ_READ_MEM, REQ_RUN_IN_ADDR, REQ_WR_LARGE_MEM, REQ_WRITE_AMLC, REQ_WRITE_MEM, Result,
  TRANSFER_BLOCK_SIZE, TRANSFER_SIZE_THRESHOLD,
  flash::FlashProgress,
  partitions::PartitionInfo,
  payload::PayloadSource,
  time::{Instant, sleep},
  usb::{COMMAND_TIMEOUT, UsbTransport},
};

struct AmlInner<U> {
  transport: U,
  callback: Option<Callback>,
}

/// The main interface for interacting with Amlogic-based hardware
///
/// This provides low-level access to the Amlogic SoC on the Superbird device,
/// allowing for memory operations, partition management, and firmware flashing.
pub struct AmlogicSoC<U: UsbTransport> {
  inner: Arc<AmlInner<U>>,
}

impl<U: UsbTransport> Clone for AmlogicSoC<U> {
  fn clone(&self) -> Self {
    Self {
      inner: self.inner.clone(),
    }
  }
}

impl<U: UsbTransport> AmlogicSoC<U> {
  /// Bring a device up in USB burn mode and claim it
  ///
  /// If the device is in USB mode it is booted through BL2 first, then reclaimed once it re-enumerates.
  ///
  /// # Parameters
  /// - `transport`: the USB backend to drive the device through
  /// - `bl2`: BL2 binary, used only when the device still needs to leave USB mode
  /// - `bootloader`: bootloader binary streamed to the SoC during the BL2 sequence
  /// - `callback`: Optional callback function to receive status updates
  ///
  /// # Returns
  /// - `Result<Self>`: A connected AmlogicSoC instance or an error
  pub async fn init(transport: U, bl2: &[u8], bootloader: &[u8], callback: Option<Callback>) -> Result<Self> {
    if let Some(callback) = &callback {
      callback(Event::FindingDevice);
    };

    let mode = transport.mode().await;
    if let Some(callback) = &callback {
      callback(Event::DeviceMode(mode));
    };

    match mode {
      DeviceMode::Usb => tracing::info!("device booted in usb mode - moving to usb burn mode"),
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

    transport.acquire().await?;

    let device = Self {
      inner: Arc::new(AmlInner { transport, callback }),
    };

    if mode == DeviceMode::Usb {
      device.bl2_boot(bl2, bootloader).await?;
    }

    Ok(device)
  }

  fn emit(&self, event: Event) {
    if let Some(callback) = &self.inner.callback {
      callback(event);
    };
  }

  /// Write data to device memory
  ///
  /// This writes a small amount of data (up to 64 bytes) to device memory.
  /// For larger transfers, use `write_large_memory` instead.
  ///
  /// # Parameters
  /// - `address`: The memory address to write to
  /// - `data`: The data to write, must be <= 64 bytes
  ///
  /// # Returns
  /// - `Result<()>`: Success or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn write_simple_memory(&self, address: u32, data: &[u8]) -> Result<()> {
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
      .transport
      .control_out(REQ_WRITE_MEM, value, index, data, COMMAND_TIMEOUT)
      .await?;
    tracing::trace!(
      "write_control completed for write_simple_memory at address: {:#X}",
      address
    );
    Ok(())
  }

  /// Write arbitrary size data to device memory
  ///
  /// This breaks down larger transfers into multiple write_simple_memory operations.
  ///
  /// # Parameters
  /// - `address`: The memory address to write to
  /// - `data`: The data to write
  ///
  /// # Returns
  /// - `Result<()>`: Success or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn write_memory(&self, address: u32, data: &[u8]) -> Result<()> {
    tracing::debug!(
      "writing memory starting at address: {:#X} with total length: {}",
      address,
      data.len()
    );
    let mut offset = 0;
    let length = data.len();
    while offset < length {
      let chunk_size = std::cmp::min(64, length - offset);
      self
        .write_simple_memory(address + offset as u32, &data[offset..offset + chunk_size])
        .await?;
      tracing::trace!(
        "chunk written for write_memory at address: {:#X}, new offset: {}",
        address,
        offset + chunk_size
      );
      offset += chunk_size;
    }
    Ok(())
  }

  /// Read a small amount of data from device memory
  ///
  /// This reads up to 64 bytes from device memory.
  /// For larger transfers, use `read_memory` instead.
  ///
  /// # Parameters
  /// - `address`: The memory address to read from
  /// - `length`: The number of bytes to read (must be <= 64)
  ///
  /// # Returns
  /// - `Result<Vec<u8>>`: The read data or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn read_simple_memory(&self, address: u32, length: usize) -> Result<Vec<u8>> {
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
      .transport
      .control_in(REQ_READ_MEM, value, index, &mut buf, COMMAND_TIMEOUT)
      .await?;
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

  /// Read arbitrary size data from device memory
  ///
  /// This breaks down larger transfers into multiple read_simple_memory operations.
  ///
  /// # Parameters
  /// - `address`: The memory address to read from
  /// - `length`: The number of bytes to read
  ///
  /// # Returns
  /// - `Result<Vec<u8>>`: The read data or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn read_memory(&self, address: u32, length: usize) -> Result<Vec<u8>> {
    tracing::debug!("reading memory at address: {:#X} with length: {}", address, length);
    let mut data = vec![0u8; length];
    let mut offset = 0;
    while offset < length {
      let read_length = std::cmp::min(64, length - offset);
      let chunk = self.read_simple_memory(address + offset as u32, read_length).await?;
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

  /// Execute code at the specified memory address
  ///
  /// # Parameters
  /// - `address`: The memory address to execute code from
  /// - `keep_power`: Whether to keep power on after execution
  ///
  /// # Returns
  /// - `Result<()>`: Success or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn run(&self, address: u32, keep_power: Option<bool>) -> Result<()> {
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
      .transport
      .control_out(REQ_RUN_IN_ADDR, value, index, &buffer, COMMAND_TIMEOUT)
      .await?;
    tracing::trace!("run command sent at address: {:#X}", address);
    Ok(())
  }

  /// Identify the device
  ///
  /// # Returns
  /// - `Result<String>`: The device identification string or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn identify(&self) -> Result<String> {
    tracing::debug!("identifying device");
    let mut buf = [0u8; 8];
    let read = self
      .inner
      .transport
      .control_in(REQ_IDENTIFY_HOST, 0, 0, &mut buf, COMMAND_TIMEOUT)
      .await?;
    tracing::trace!("identify response received: {:?} ({} bytes)", &buf, read);
    if read != 8 {
      return Err(Error::InvalidOperation("Failed to read identify data".into()));
    }
    Ok(String::from_utf8(buf.to_vec())?)
  }

  /// Write large blocks of data to device memory
  ///
  /// This is used for writing firmware images and other large data blocks.
  ///
  /// # Parameters
  /// - `memory_address`: The memory address to write to
  /// - `data`: The data to write
  /// - `block_length`: The size of each block to transfer
  /// - `append_zeros`: Whether to pad data with zeros to match block_length
  ///
  /// # Returns
  /// - `Result<()>`: Success or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn write_large_memory(
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
    } else if !data_vec.len().is_multiple_of(block_length) {
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
    self
      .inner
      .transport
      .control_out(
        REQ_WR_LARGE_MEM,
        block_length as u16,
        block_count,
        &control_data,
        COMMAND_TIMEOUT,
      )
      .await?;

    let mut data_offset = 0;
    while data_offset < data_vec.len() {
      let end = data_offset + block_length;
      let chunk = &data_vec[data_offset..end];
      tracing::trace!(target: "flashthing::aml::write_large_memory", "writing actual data from offset: {:#X}", &data_offset);

      self
        .inner
        .transport
        .bulk_out(chunk, Duration::from_millis(2000))
        .await?;

      tracing::trace!(target: "flashthing::aml::write_large_memory", "wrote actual data from offset: {:#X}", &data_offset);

      data_offset += block_length;
    }

    Ok(())
  }

  /// Write large blocks of data directly to a disk address with progress tracking
  ///
  /// # Parameters
  /// - `disk_address`: The disk address to write to
  /// - `source`: The payload providing the data to write
  /// - `data_size`: The total size of data to write
  /// - `block_length`: The size of each block to transfer
  /// - `append_zeros`: Whether to pad data with zeros to match block_length
  /// - `progress_callback`: Function to call with progress updates
  ///
  /// # Returns
  /// - `Result<()>`: Success or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn write_large_memory_to_disk<F: Fn(FlashProgress)>(
    &self,
    disk_address: u32,
    source: &mut dyn PayloadSource,
    data_size: usize,
    block_length: usize,
    append_zeros: bool,
    progress_callback: F,
  ) -> Result<()> {
    tracing::debug!("streaming {} bytes to disk address: {:#X}", data_size, disk_address);

    let start_time = Instant::now();
    let mut total_chunks = 0;
    let mut avg_chunk_time_secs = 0.0;

    // needed for write operations
    self.bulkcmd("mmc dev 1").await?;
    self.bulkcmd("amlmmc key").await?;

    let total_len = data_size;
    let max_bytes_per_transfer = TRANSFER_SIZE_THRESHOLD;
    let mut offset = 0;
    let mut buffer = vec![0u8; max_bytes_per_transfer];

    while offset < total_len {
      let chunk_start_time = Instant::now();

      let remaining = total_len - offset;
      let write_length = std::cmp::min(remaining, max_bytes_per_transfer);

      source.read_exact(&mut buffer[..write_length]).await?;

      self
        .write_large_memory(ADDR_TMP, &buffer[..write_length], block_length, append_zeros)
        .await?;

      let start_time_cmd = Instant::now();
      let mut retries = 0;
      let max_retries = 3;

      loop {
        match self
          .bulkcmd(&format!(
            "mmc write {:#X} {:#X} {:#X}",
            ADDR_TMP,
            (disk_address as usize + offset) / 512,
            write_length / 512
          ))
          .await
        {
          Ok(_) => {
            let elapsed = start_time_cmd.elapsed();
            if elapsed > Duration::from_millis(3000) {
              tracing::debug!("mmc write command took {}ms, cooling down for 5s", elapsed.as_millis());
              sleep(Duration::from_secs(5)).await;
            }
            break;
          }
          Err(e) => {
            retries += 1;
            if retries >= max_retries {
              return Err(e);
            }
            sleep(Duration::from_secs(5)).await; // cooldown after error
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
  pub async fn write_amlc_data(&self, offset: u32, data: &[u8]) -> Result<()> {
    tracing::debug!("writing amlc data at offset: {:#X} with length: {}", offset, data.len());

    self
      .inner
      .transport
      .control_out(
        REQ_WRITE_AMLC,
        (offset / AMLC_AMLS_BLOCK_LENGTH as u32) as u16,
        (data.len() - 1) as u16,
        &[],
        COMMAND_TIMEOUT,
      )
      .await?;
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
        match self.inner.transport.bulk_out(chunk, bulk_timeout).await {
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
              sleep(Duration::from_millis(100)).await;
            }
          }
          Err(e) => {
            tracing::warn!("Error in bulk write: {}. Retry {}/{}", e, retries + 1, max_retries);
            retries += 1;
            sleep(Duration::from_millis(100)).await;

            if retries >= max_retries {
              return Err(e);
            }
          }
        }
      }

      data_offset += block_length;
      remaining -= block_length;

      sleep(Duration::from_millis(10)).await;
    }

    let mut ack_buf = [0u8; 16];
    let mut retries = 0;
    let max_retries = 3;
    let mut read = 0;

    while retries < max_retries {
      match self.inner.transport.bulk_in(&mut ack_buf, bulk_timeout).await {
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
      sleep(Duration::from_millis(100)).await;
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
  pub async fn write_amlc_data_packet(&self, seq: u8, amlc_offset: u32, data: &[u8]) -> Result<()> {
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

        self
          .write_amlc_data(offset as u32, &data[offset..offset + write_length])
          .await?;
        sleep(Duration::from_millis(50)).await;

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
    self.write_amlc_data(amlc_offset, &amlc_data).await?;

    Ok(())
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn get_boot_amlc(&self) -> Result<(u32, u32)> {
    tracing::debug!("getting boot amlc data");
    self
      .inner
      .transport
      .control_out(REQ_GET_AMLC, AMLC_AMLS_BLOCK_LENGTH as u16, 0, &[], COMMAND_TIMEOUT)
      .await?;
    tracing::trace!("amlc get request sent");
    let mut buf = vec![0u8; AMLC_AMLS_BLOCK_LENGTH];
    let read = self.inner.transport.bulk_in(&mut buf, Duration::from_secs(2)).await?;
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
    self.inner.transport.bulk_out(&ack, Duration::from_secs(2)).await?;
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

  /// Execute the BL2 boot sequence
  ///
  /// This boots the device using the specified BL2 and bootloader binaries, then waits out the reset
  /// and claims the device again under its USB burn mode identity.
  ///
  /// # Parameters
  /// - `bl2`: BL2 binary data
  /// - `bootloader`: bootloader binary data
  ///
  /// # Returns
  /// - `Result<()>`: Success or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn bl2_boot(&self, bl2: &[u8], bootloader: &[u8]) -> Result<()> {
    self.emit(Event::Bl2Boot);

    tracing::info!("sending bl2 binary to address {:#X}...", ADDR_BL2);
    self.write_large_memory(ADDR_BL2, bl2, 4096, true).await?;

    tracing::info!("booting from bl2...");
    self.run(ADDR_BL2, Some(true)).await?;

    tracing::debug!("waiting for bootloader to initialize...");
    sleep(Duration::from_secs(2)).await;

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
        match self.get_boot_amlc().await {
          Ok(result) => break result,
          Err(e) => {
            retry_count += 1;
            if retry_count >= max_retries {
              tracing::error!("failed to get boot amlc data after {} attempts: {}", max_retries, e);
              return Err(e);
            }
            tracing::warn!("failed to get boot amlc, retry {}/{}: {}", retry_count, max_retries, e);
            sleep(Duration::from_millis(500)).await;
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
        self.write_amlc_data_packet(seq, offset, empty_slice).await?;
      } else {
        let actual_length = std::cmp::min(length as usize, bootloader.len() - offset as usize);
        let data_slice = &bootloader[offset as usize..offset as usize + actual_length];

        tracing::debug!("sending {} bytes at offset {} with seq {}", actual_length, offset, seq);
        self.write_amlc_data_packet(seq, offset, data_slice).await?;
      }

      seq = seq.wrapping_add(1);
      sleep(Duration::from_millis(100)).await;
    }

    tracing::info!("bl2 boot sequence completed successfully!");

    self.emit(Event::Resetting);
    tracing::debug!("device successfully moved to usb burn mode, sleeping then grabbing new handle");
    sleep(Duration::from_millis(5000)).await;
    self.inner.transport.acquire().await?;

    Ok(())
  }

  /// Send a bulk command to the device
  ///
  /// # Parameters
  /// - `command`: The command string to send
  ///
  /// # Returns
  /// - `Result<String>`: The command response or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn bulkcmd(&self, command: &str) -> Result<String> {
    tracing::debug!("sending bulk command: {:?}", command);
    let mut command = command.as_bytes().to_vec();
    command.push(0x00);
    self
      .inner
      .transport
      .control_out(REQ_BULKCMD, 0, 0, &command, COMMAND_TIMEOUT)
      .await?;
    tracing::trace!("bulk command control write completed");

    let mut buf = vec![0u8; 512];
    let read = self.inner.transport.bulk_in(&mut buf, COMMAND_TIMEOUT).await?;
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

  /// Validate the size of a partition
  ///
  /// # Parameters
  /// - `part_name`: The name of the partition
  /// - `part_info`: Partition information
  ///
  /// # Returns
  /// - `Result<usize>`: The validated partition size or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn validate_partition_size(&self, part_name: &str, part_info: &PartitionInfo) -> Result<usize> {
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
    match self
      .bulkcmd(&format!(
        "amlmmc read {} {:#x} {:#x} {:#x}",
        part_name,
        ADDR_TMP,
        part_size - PART_SECTOR_SIZE,
        PART_SECTOR_SIZE
      ))
      .await
    {
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
        if part_name == "data"
          && let Some(alt_size_raw) = part_info.size_alt
        {
          let alt_size = alt_size_raw * PART_SECTOR_SIZE;
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

          match self
            .bulkcmd(&format!(
              "amlmmc read {} {:#x} {:#x} {:#x}",
              part_name,
              ADDR_TMP,
              alt_size - PART_SECTOR_SIZE,
              PART_SECTOR_SIZE
            ))
            .await
          {
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

  /// Write a boot hwpartition (boot0 / boot1) wholesale.
  ///
  /// Switches the eMMC to the named hwpart, single-shot DDR-stages the bytes,
  /// `mmc write`s them at LBA 0, then restores the user area selection.
  ///
  /// # Parameters
  /// - `hwpart`: 1 for boot0, 2 for boot1.
  /// - `data`: payload (signed boot.bin). Capped at `TRANSFER_SIZE_THRESHOLD`.
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn write_boot_partition(&self, hwpart: u8, data: &[u8]) -> Result<()> {
    if !(1..=2).contains(&hwpart) {
      return Err(Error::InvalidOperation(format!(
        "boot hwpart must be 1 or 2, got {hwpart}"
      )));
    }
    if data.len() > TRANSFER_SIZE_THRESHOLD {
      return Err(Error::InvalidOperation(format!(
        "boot partition payload {} bytes exceeds single-transfer cap {}",
        data.len(),
        TRANSFER_SIZE_THRESHOLD
      )));
    }

    tracing::info!("writing {} bytes to boot{}", data.len(), hwpart - 1);

    self.bulkcmd(&format!("mmc dev 1 {hwpart}")).await?;
    self.bulkcmd("amlmmc key").await?;

    self
      .write_large_memory(ADDR_TMP, data, TRANSFER_BLOCK_SIZE, true)
      .await?;

    let sector_count = data.len().div_ceil(PART_SECTOR_SIZE);
    self
      .bulkcmd(&format!("mmc write {ADDR_TMP:#X} 0 {sector_count:#X}"))
      .await?;

    self.bulkcmd("mmc dev 1 0").await?;
    Ok(())
  }

  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn write_user_area<F: Fn(FlashProgress)>(
    &self,
    lba_offset: u32,
    source: &mut dyn PayloadSource,
    data_size: usize,
    sparse: bool,
    progress_callback: F,
  ) -> Result<()> {
    tracing::info!(
      "streaming {} bytes to user area starting at LBA {} (sparse: {})",
      data_size,
      lba_offset,
      sparse
    );

    let start_time = Instant::now();
    let mut total_chunks = 0;
    let mut avg_chunk_time_secs = 0.0;

    self.bulkcmd("mmc dev 1 0").await?;
    self.bulkcmd("amlmmc key").await?;

    let (mut erased_from, mut erased_to) = (0usize, 0usize);

    if sparse {
      let span_sectors = data_size.div_ceil(PART_SECTOR_SIZE);
      let first = lba_offset as usize;
      let start = first.div_ceil(ERASE_GROUP_SECTORS) * ERASE_GROUP_SECTORS;
      let end = (first + span_sectors) / ERASE_GROUP_SECTORS * ERASE_GROUP_SECTORS;

      if end > start {
        let count = end - start;
        tracing::info!("erasing {} sectors at LBA {} before sparse write", count, start);
        self.bulkcmd(&format!("mmc erase {start:#X} {count:#X}")).await?;
        erased_from = (start - first) * PART_SECTOR_SIZE;
        erased_to = (end - first) * PART_SECTOR_SIZE;
      } else {
        tracing::info!("sparse write spans no whole erase group at LBA {first}; writing in full");
      }
    }

    let max_bytes_per_transfer = TRANSFER_SIZE_THRESHOLD;
    let mut offset = 0;
    let mut buffer = vec![0u8; max_bytes_per_transfer];

    while offset < data_size {
      let chunk_start_time = Instant::now();

      let remaining = data_size - offset;
      let write_length = std::cmp::min(remaining, max_bytes_per_transfer);

      source.read_exact(&mut buffer[..write_length]).await?;

      let erased = offset >= erased_from && offset + write_length <= erased_to;
      let skip = sparse && erased && buffer[..write_length].iter().all(|&b| b == 0);
      if skip {
        tracing::debug!(
          "skipping all-zero chunk at LBA {:#X}",
          lba_offset as usize + offset / PART_SECTOR_SIZE
        );
      } else {
        self
          .write_large_memory(ADDR_TMP, &buffer[..write_length], TRANSFER_BLOCK_SIZE, true)
          .await?;

        let chunk_lba = lba_offset as usize + offset / PART_SECTOR_SIZE;
        let chunk_sectors = write_length / PART_SECTOR_SIZE;

        let cmd_start = Instant::now();
        let mut retries = 0;
        let max_retries = 3;
        loop {
          match self
            .bulkcmd(&format!("mmc write {ADDR_TMP:#X} {chunk_lba:#X} {chunk_sectors:#X}"))
            .await
          {
            Ok(_) => {
              if cmd_start.elapsed() > Duration::from_millis(3000) {
                tracing::debug!("mmc write took {}ms, cooling down 5s", cmd_start.elapsed().as_millis());
                sleep(Duration::from_secs(5)).await;
              }
              break;
            }
            Err(e) => {
              retries += 1;
              if retries >= max_retries {
                return Err(e);
              }
              tracing::warn!(
                "mmc write failed at LBA {chunk_lba:#X}, retrying ({}/{}): {}",
                retries,
                max_retries,
                e
              );
              sleep(Duration::from_secs(5)).await;
            }
          }
        }
      }

      let chunk_time_secs = chunk_start_time.elapsed().as_secs_f64();
      total_chunks += 1;
      if total_chunks == 1 {
        avg_chunk_time_secs = chunk_time_secs;
      } else {
        avg_chunk_time_secs += (chunk_time_secs - avg_chunk_time_secs) / total_chunks as f64;
      }

      offset += write_length;
      let progress_percent = offset as f64 / data_size as f64 * 100.0;
      let elapsed_secs = start_time.elapsed().as_secs_f64();
      let bytes_per_sec = if elapsed_secs > 0.0 {
        offset as f64 / elapsed_secs
      } else {
        offset as f64
      };
      let eta_secs = if bytes_per_sec > 0.0 {
        (data_size - offset) as f64 / bytes_per_sec
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

    tracing::info!(
      "user-area write complete: {} bytes in {:?}",
      data_size,
      start_time.elapsed()
    );
    Ok(())
  }

  /// Restore a partition from a data source
  ///
  /// # Parameters
  /// - `part_name`: The name of the partition to restore
  /// - `part_size`: The size of the partition
  /// - `source`: The payload providing the partition data
  /// - `file_size`: The size of the data being read
  /// - `progress_callback`: Function to call with progress updates
  ///
  /// # Returns
  /// - `Result<()>`: Success or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn restore_partition<F: Fn(FlashProgress)>(
    &self,
    part_name: &str,
    part_size: usize,
    source: &mut dyn PayloadSource,
    file_size: usize,
    progress_callback: F,
  ) -> Result<()> {
    tracing::debug!("restoring partition: {} with file size: {}", part_name, file_size);

    let adjusted_part_size = if part_name == "bootloader" {
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

    let start_time = Instant::now();
    let mut total_chunks = 0;
    let mut avg_chunk_time_secs = 0.0;

    self.bulkcmd("amlmmc key").await?;

    let total_len = file_size;
    let max_bytes_per_transfer = TRANSFER_SIZE_THRESHOLD;
    let mut offset = 0;
    let mut buffer = vec![0u8; max_bytes_per_transfer];

    while offset < total_len {
      let chunk_start_time = Instant::now();

      let remaining = total_len - offset;
      let write_length = std::cmp::min(remaining, max_bytes_per_transfer);

      source.read_exact(&mut buffer[..write_length]).await?;

      self
        .write_large_memory(ADDR_TMP, &buffer[..write_length], TRANSFER_BLOCK_SIZE, true)
        .await?;

      let start_time_cmd = Instant::now();
      let mut retries = 0;
      let max_retries = 3;

      if part_name == "bootloader" {
        match self
          .bulkcmd(&format!(
            "amlmmc write {} {:#x} {:#x} {:#x}",
            part_name, ADDR_TMP, offset, write_length
          ))
          .await
        {
          Ok(_) => tracing::debug!("bootloader write succeeded unexpectedly"),
          Err(e) => tracing::debug!("expected timeout for bootloader write: {}", e),
        }
        sleep(Duration::from_secs(2)).await;
      } else {
        loop {
          match self
            .bulkcmd(&format!(
              "amlmmc write {} {:#x} {:#x} {:#x}",
              part_name, ADDR_TMP, offset, write_length
            ))
            .await
          {
            Ok(_) => {
              let elapsed = start_time_cmd.elapsed();
              if elapsed > Duration::from_millis(3000) {
                tracing::debug!("write command took {}ms, cooling down for 5s", elapsed.as_millis());
                sleep(Duration::from_secs(5)).await;
              }
              break;
            }
            Err(e) => {
              retries += 1;
              if retries >= max_retries {
                return Err(e);
              }
              tracing::warn!("write command failed, retrying ({}/{}): {}", retries, max_retries, e);
              sleep(Duration::from_secs(5)).await;
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

  /// Execute the unbrick procedure
  ///
  /// This writes the emergency unbrick image over the start of the eMMC.
  ///
  /// # Parameters
  /// - `source`: The payload providing the unpacked unbrick image
  /// - `size`: The size of the unbrick image
  ///
  /// # Returns
  /// - `Result<()>`: Success or an error
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn unbrick_from(&self, source: &mut dyn PayloadSource, size: usize) -> Result<()> {
    tracing::info!("starting unbrick procedure...");

    self
      .write_large_memory_to_disk(0, source, size, TRANSFER_BLOCK_SIZE, true, |progress| {
        tracing::info!(
          "unbrick progress: {:.1}% | elapsed: {:.1}s | eta: {:.1}s | rate: {:.2} KB/s | avg rate: {:.2} KB/s",
          progress.percent,
          progress.elapsed,
          progress.eta,
          progress.rate,
          progress.avg_rate
        );
      })
      .await?;

    tracing::info!("unbrick procedure completed successfully!");
    Ok(())
  }

  /// Execute the unbrick procedure using the unbrick image bundled with this crate
  ///
  /// # Returns
  /// - `Result<()>`: Success or an error
  #[cfg(not(target_arch = "wasm32"))]
  #[cfg_attr(feature = "instrument", tracing::instrument(level = "trace", skip_all))]
  pub async fn unbrick(&self) -> Result<()> {
    let cursor = std::io::Cursor::new(crate::UNBRICK_BIN_ZIP);

    let mut archive = match zip::ZipArchive::new(cursor) {
      Ok(archive) => archive,
      Err(e) => {
        tracing::error!("failed to open unbrick zip archive: {}", e);
        return Err(Error::Zip(e));
      }
    };

    let file = match archive.by_name("unbrick.bin") {
      Ok(file) => file,
      Err(e) => {
        tracing::error!("failed to find unbrick.bin in zip archive: {}", e);
        return Err(Error::Zip(e));
      }
    };

    let file_size = file.size() as usize;
    let mut source = crate::payload::BlockingSource(file);
    self.unbrick_from(&mut source, file_size).await
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl AmlogicSoC<crate::native::NativeUsb> {
  /// Find and claim the locally connected device, BL2 booting it into USB burn mode if needed
  ///
  /// # Parameters
  /// - `callback`: Optional callback function to receive status updates
  ///
  /// # Returns
  /// - `Result<Self>`: A connected AmlogicSoC instance or an error
  pub async fn connect(callback: Option<Callback>) -> Result<Self> {
    let transport = crate::native::NativeUsb::new(callback.clone());
    Self::init(transport, crate::BL2_BIN, crate::BOOTLOADER_BIN, callback).await
  }
}

#[cfg(target_arch = "wasm32")]
impl AmlogicSoC<crate::web::WebUsb> {
  /// Choose and claim a device over WebUSB, BL2 booting it into USB burn mode if needed
  ///
  /// # Parameters
  /// - `await_gesture`: Called with a reason string when the browser needs a click before it will open the device
  ///   chooser; must resolve once the user has clicked
  /// - `callback`: Optional callback function to receive status updates
  ///
  /// # Returns
  /// - `Result<Self>`: A connected AmlogicSoC instance or an error
  pub async fn connect(await_gesture: js_sys::Function, callback: Option<Callback>) -> Result<Self> {
    let transport = crate::web::WebUsb::new(await_gesture, callback.clone());
    Self::init(transport, crate::BL2_BIN, crate::BOOTLOADER_BIN, callback).await
  }
}

/// Set up the host environment for USB access
///
/// On Linux, this creates udev rules to allow access to the device.
///
/// # Returns
/// - `Result<()>`: Success or an error
#[cfg(not(target_arch = "wasm32"))]
pub fn host_setup() -> Result<()> {
  #[cfg(target_os = "linux")]
  crate::setup::setup_host_linux()?;

  Ok(())
}

/// The current mode of the Superbird device
///
/// The device can be in different modes depending on how it was powered on
/// and what stage of the boot process it's in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeviceMode {
  /// Normal operating mode (running regular firmware)
  Normal,
  /// USB mode (entered by holding buttons 1 & 4 during power-on)
  Usb,
  /// USB Burn mode (ready for flashing operations)
  UsbBurn,
  /// Device not detected
  NotFound,
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
  use super::*;

  // needs a car thing attached in burn mode, so it is opt-in: cargo test -- --ignored
  #[test]
  #[ignore]
  fn test_amlogic_soc_connect() {
    let soc = pollster::block_on(AmlogicSoC::connect(None));
    assert!(soc.is_ok());
  }
}
