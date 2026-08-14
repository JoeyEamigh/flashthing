use flashthing::{Fastboot, fastboot::MAX_COMMAND_BYTES};

fn connect() -> Fastboot<flashthing::NativeUsb> {
  pollster::block_on(Fastboot::connect(None)).expect("could not reach the device in fastboot")
}

#[test]
#[ignore]
fn reports_a_usable_download_buffer() {
  let fastboot = connect();
  let size = pollster::block_on(fastboot.max_download_size());

  assert!(
    size >= 8 * 1024 * 1024,
    "device reported a {size}-byte download buffer, which is smaller than one chunk"
  );
}

#[test]
#[ignore]
fn raw_partition_alias_round_trips() {
  let fastboot = connect();

  let (lba, sectors) = (3_256_496u64, 4_476_752u64);
  assert!(
    format!("oem console setenv fastboot_raw_partition_ft {lba} {sectors}").len() <= MAX_COMMAND_BYTES,
    "the alias command has outgrown u-boot's command buffer"
  );

  pollster::block_on(fastboot.set_raw_target("ft", lba, sectors)).expect("could not set the raw target");

  let set = pollster::block_on(fastboot.console("printenv fastboot_raw_partition_ft")).expect("printenv failed");
  assert!(
    set.contains(&format!("{lba} {sectors}")),
    "u-boot did not store the range we asked for: {set:?}"
  );

  pollster::block_on(fastboot.clear_raw_target("ft")).expect("could not clear the raw target");

  let cleared = pollster::block_on(fastboot.console("printenv fastboot_raw_partition_ft")).expect("printenv failed");
  assert!(
    cleared.contains("not defined"),
    "the alias outlived the write it was made for: {cleared:?}"
  );
}

#[test]
#[ignore]
fn download_fills_the_scratch_buffer() {
  let fastboot = connect();

  let payload: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();

  let last_sent = std::cell::Cell::new(0usize);
  pollster::block_on(fastboot.download(&payload, |sent, total| {
    assert_eq!(total, payload.len());
    assert!(sent > last_sent.get(), "progress went backwards");
    last_sent.set(sent);
  }))
  .expect("download failed");
  assert_eq!(last_sent.get(), payload.len(), "progress never reached the end");

  let expected = format!("{:08x}", crc32(&payload));
  let output =
    pollster::block_on(fastboot.console(&format!("crc32 0x6000000 {:x}", payload.len()))).expect("crc32 failed");
  assert!(
    output.to_lowercase().contains(&expected),
    "the device's copy does not match what we sent: {output:?} (expected {expected})"
  );
}

fn crc32(input: &[u8]) -> u32 {
  let mut crc = 0xffff_ffffu32;
  for &byte in input {
    crc ^= byte as u32;
    for _ in 0..8 {
      crc = if crc & 1 != 0 {
        (crc >> 1) ^ 0xedb8_8320
      } else {
        crc >> 1
      };
    }
  }
  !crc
}

#[cfg(test)]
mod tests {
  use super::crc32;

  #[test]
  fn crc32_matches_the_known_vectors() {
    assert_eq!(crc32(b""), 0x0000_0000);
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414f_a339);
  }

  #[test]
  fn crc32_matches_what_the_device_computed() {
    let payload: Vec<u8> = (0..256u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(crc32(&payload), 0x5708_a3cc);
  }
}

const SCRATCH_SECTORS: u64 = 40960; // 20 MiB
const SCRATCH_BYTES: usize = SCRATCH_SECTORS as usize * 512;
const BACKUP_ADDR: u64 = 0x1000_0000;
const VERIFY_ADDR: u64 = 0x1400_0000;
const CHUNK_BYTES: usize = 8 * 1024 * 1024;

fn scratch_lba() -> Option<u64> {
  match std::env::var("FLASHTHING_SCRATCH_LBA") {
    Ok(raw) => Some(
      raw
        .trim()
        .parse()
        .expect("FLASHTHING_SCRATCH_LBA must be a decimal LBA"),
    ),
    Err(_) => {
      eprintln!("skipping: set FLASHTHING_SCRATCH_LBA to an LBA in unused space to run the write tests");
      None
    }
  }
}

fn pattern(len: usize, seed: u32) -> Vec<u8> {
  let mut state = seed | 1;
  (0..len)
    .map(|_| {
      state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
      (state >> 24) as u8
    })
    .collect()
}

fn device_crc(fastboot: &Fastboot<flashthing::NativeUsb>, lba: u64, len: usize) -> u32 {
  let sectors = len.div_ceil(512);
  let read = pollster::block_on(fastboot.console(&format!("mmc read {VERIFY_ADDR:#x} {lba:x} {sectors:x}")))
    .expect("mmc read failed");
  assert!(read.contains("OK"), "mmc read did not report OK: {read:?}");

  let output = pollster::block_on(fastboot.console(&format!("crc32 {VERIFY_ADDR:#x} {len:x}"))).expect("crc32 failed");
  let hex = output
    .rsplit("==>")
    .next()
    .expect("crc32 printed no result")
    .split_whitespace()
    .next()
    .expect("crc32 printed no value");
  u32::from_str_radix(hex, 16).unwrap_or_else(|_| panic!("could not parse the crc out of {output:?}"))
}

fn write_scratch(fastboot: &Fastboot<flashthing::NativeUsb>, lba: u64, data: &[u8], sparse: bool) -> Vec<f64> {
  let percents = std::cell::RefCell::new(Vec::new());
  let mut source = flashthing::payload::inline_source(data);

  pollster::block_on(
    fastboot.write_raw(lba, source.as_mut(), data.len(), sparse, |progress| {
      percents.borrow_mut().push(progress.percent);
    }),
  )
  .expect("write_raw failed");

  percents.into_inner()
}

#[test]
#[ignore]
fn writes_to_the_emmc_and_puts_it_back() {
  let Some(lba) = scratch_lba() else { return };
  let fastboot = connect();

  assert!(
    pollster::block_on(fastboot.max_download_size()) >= CHUNK_BYTES,
    "the device's download buffer is too small for these tests to chunk the way they assume"
  );

  let original_crc = device_crc(&fastboot, lba, SCRATCH_BYTES);
  let backup = pollster::block_on(fastboot.console(&format!("mmc read {BACKUP_ADDR:#x} {lba:x} {SCRATCH_SECTORS:x}")))
    .expect("could not back the scratch window up");
  assert!(backup.contains("OK"), "backup read did not report OK: {backup:?}");
  eprintln!("backed up {SCRATCH_BYTES} bytes at LBA {lba} (crc {original_crc:08x})");

  let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    let payload = pattern(SCRATCH_BYTES, 0xa5a5_0001);
    let percents = write_scratch(&fastboot, lba, &payload, false);

    assert_eq!(
      device_crc(&fastboot, lba, SCRATCH_BYTES),
      crc32(&payload),
      "a {SCRATCH_BYTES}-byte write across {} chunks did not land intact",
      SCRATCH_BYTES.div_ceil(CHUNK_BYTES)
    );

    assert!(
      percents.windows(2).all(|pair| pair[1] >= pair[0]),
      "progress went backwards: {percents:?}"
    );
    assert!(
      percents.last().is_some_and(|last| (last - 100.0).abs() < f64::EPSILON),
      "progress ended at {:?} rather than 100%",
      percents.last()
    );

    let ragged = pattern(1024 * 1024 + 100, 0xa5a5_0002);
    write_scratch(&fastboot, lba, &ragged, false);

    assert_eq!(
      device_crc(&fastboot, lba, ragged.len()),
      crc32(&ragged),
      "the ragged tail of a write was not preserved"
    );

    let pad = 512 - (ragged.len() % 512);
    let padded_crc = device_crc(&fastboot, lba, ragged.len() + pad);
    let mut expected = ragged.clone();
    expected.extend(std::iter::repeat_n(0u8, pad));
    assert_eq!(
      padded_crc,
      crc32(&expected),
      "the tail sector was padded with something other than zeroes"
    );

    let known = pattern(SCRATCH_BYTES, 0xa5a5_0003);
    write_scratch(&fastboot, lba, &known, false);
    assert_eq!(device_crc(&fastboot, lba, SCRATCH_BYTES), crc32(&known));

    let zeroes = vec![0u8; SCRATCH_BYTES];
    write_scratch(&fastboot, lba, &zeroes, true);
    assert_eq!(
      device_crc(&fastboot, lba, SCRATCH_BYTES),
      crc32(&zeroes),
      "a sparse write of all zeroes left the previous contents behind — it skipped without erasing"
    );

    let mut mixed = pattern(SCRATCH_BYTES, 0xa5a5_0004);
    mixed[CHUNK_BYTES..2 * CHUNK_BYTES].fill(0);

    write_scratch(&fastboot, lba, &known, false);
    write_scratch(&fastboot, lba, &mixed, true);
    let sparse_crc = device_crc(&fastboot, lba, SCRATCH_BYTES);

    write_scratch(&fastboot, lba, &known, false);
    write_scratch(&fastboot, lba, &mixed, false);
    let plain_crc = device_crc(&fastboot, lba, SCRATCH_BYTES);

    assert_eq!(sparse_crc, crc32(&mixed), "the sparse write did not match the payload");
    assert_eq!(sparse_crc, plain_crc, "sparse and non-sparse writes disagreed");

    let alias = pollster::block_on(fastboot.console("printenv fastboot_raw_partition_ft")).expect("printenv failed");
    assert!(
      alias.contains("not defined"),
      "write_raw left its raw alias behind: {alias:?}"
    );
  }));

  let restore =
    pollster::block_on(fastboot.console(&format!("mmc write {BACKUP_ADDR:#x} {lba:x} {SCRATCH_SECTORS:x}")))
      .expect("could not restore the scratch window");
  assert!(restore.contains("OK"), "restore write did not report OK: {restore:?}");

  let restored_crc = device_crc(&fastboot, lba, SCRATCH_BYTES);
  eprintln!("restored LBA {lba} (crc {restored_crc:08x})");

  if let Err(panic) = outcome {
    std::panic::resume_unwind(panic);
  }

  assert_eq!(
    restored_crc, original_crc,
    "the scratch window was not restored to what it held before the test"
  );
}

#[test]
#[ignore]
fn runs_a_flash_config_end_to_end() {
  let Some(lba) = scratch_lba() else { return };

  let payload = pattern(3 * 1024 * 1024, 0xa5a5_0005);
  let dir = std::env::temp_dir().join("flashthing-fastboot-e2e");
  std::fs::create_dir_all(&dir).unwrap();
  std::fs::write(dir.join("payload.bin"), &payload).unwrap();
  std::fs::write(
    dir.join("meta.json"),
    format!(
      r#"{{
        "name": "flashthing fastboot hardware test",
        "version": "0.0.0",
        "description": "drives the fastboot translation layer against a real device",
        "metadataVersion": 2,
        "steps": [
          {{ "type": "log", "value": "hello from the hardware test" }},
          {{ "type": "wait", "value": {{ "type": "time", "time": 10 }} }},
          {{ "type": "bulkcmd", "value": "amlmmc key" }},
          {{ "type": "bulkcmd", "value": "mmc dev 1" }},
          {{ "type": "writeUserArea", "value": {{ "lba": {lba}, "data": {{ "filePath": "./payload.bin" }} }} }}
        ]
      }}"#
    ),
  )
  .unwrap();

  let steps = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
  let progress = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
  let callback = {
    let (steps, progress) = (steps.clone(), progress.clone());
    std::sync::Arc::new(move |event: flashthing::Event| match event {
      flashthing::Event::Step(index, _) => steps.lock().unwrap().push(index),
      flashthing::Event::FlashProgress(value) => progress.lock().unwrap().push(value.percent),
      _ => {}
    }) as flashthing::Callback
  };

  let mut flasher = pollster::block_on(flashthing::FastbootFlasher::from_directory(dir.clone(), Some(callback)))
    .expect("could not open the config");
  assert_eq!(flasher.num_steps(), 5);
  pollster::block_on(flasher.flash()).expect("flashing failed");

  assert_eq!(
    *steps.lock().unwrap(),
    vec![1, 2, 3, 4, 5],
    "every step should have announced itself, in order"
  );
  let progress = progress.lock().unwrap().clone();
  assert!(!progress.is_empty(), "the user-area write reported no progress");
  assert!(
    progress.last().is_some_and(|last| (last - 100.0).abs() < f64::EPSILON),
    "progress ended at {:?} rather than 100%",
    progress.last()
  );

  let fastboot = flasher.device();
  assert_eq!(
    device_crc(fastboot, lba, payload.len()),
    crc32(&payload),
    "the config's payload did not land on the eMMC intact"
  );

  std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[ignore]
fn writes_and_persists_the_uboot_environment() {
  if scratch_lba().is_none() {
    return;
  }
  let fastboot = connect();

  pollster::block_on(fastboot.write_env("flashthing_test_key=hello world\n", true)).expect("write_env failed");

  let set = pollster::block_on(fastboot.console("printenv flashthing_test_key")).expect("printenv failed");
  assert!(
    set.contains("hello world"),
    "the environment did not take the value we imported: {set:?}"
  );

  let reloaded = pollster::block_on(fastboot.console("env import -d -b")).expect("env reload failed");
  assert!(
    !reloaded.to_lowercase().contains("error"),
    "could not reload the environment from storage: {reloaded:?}"
  );
  let persisted = pollster::block_on(fastboot.console("printenv flashthing_test_key")).expect("printenv failed");
  assert!(
    persisted.contains("hello world"),
    "the value never reached the env partition: {persisted:?}"
  );

  pollster::block_on(fastboot.console("setenv flashthing_test_key")).expect("could not unset the key");
  pollster::block_on(fastboot.console("saveenv")).expect("saveenv failed");

  let cleared = pollster::block_on(fastboot.console("printenv flashthing_test_key")).expect("printenv failed");
  assert!(
    cleared.contains("not defined"),
    "the throwaway key outlived the test: {cleared:?}"
  );
}

#[test]
#[ignore]
fn writes_a_partition_by_gpt_name() {
  let Ok(name) = std::env::var("FLASHTHING_SCRATCH_PARTITION") else {
    eprintln!("skipping: set FLASHTHING_SCRATCH_PARTITION to a partition whose start can be overwritten");
    return;
  };
  let fastboot = connect();

  let size = pollster::block_on(fastboot.getvar(&format!("partition-size:{name}")))
    .unwrap_or_else(|err| panic!("the device does not know a partition called {name:?}: {err}"));
  eprintln!("{name} is {} bytes", size.trim());

  let listing = pollster::block_on(fastboot.console("mmc part")).expect("mmc part failed");
  let start = partition_start(&listing, &name)
    .unwrap_or_else(|| panic!("could not find {name:?} in the device's partition table"));

  let payload = pattern(4 * 1024 * 1024, 0xa5a5_0006);
  let sectors = payload.len() as u64 / 512;

  let original_crc = device_crc(&fastboot, start, payload.len());
  let backup = pollster::block_on(fastboot.console(&format!("mmc read {BACKUP_ADDR:#x} {start:x} {sectors:x}")))
    .expect("could not back the partition prefix up");
  assert!(backup.contains("OK"), "backup read did not report OK: {backup:?}");
  eprintln!(
    "backed up the first {} bytes of {name} at LBA {start} (crc {original_crc:08x})",
    payload.len()
  );

  let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    let mut source = flashthing::payload::inline_source(&payload);
    pollster::block_on(fastboot.write_by_name(&name, source.as_mut(), payload.len(), |_| {}))
      .expect("write_by_name failed");

    assert_eq!(
      device_crc(&fastboot, start, payload.len()),
      crc32(&payload),
      "flashing {name} by GPT name did not land the payload at its start"
    );
  }));

  let restore = pollster::block_on(fastboot.console(&format!("mmc write {BACKUP_ADDR:#x} {start:x} {sectors:x}")))
    .expect("could not restore the partition prefix");
  assert!(restore.contains("OK"), "restore write did not report OK: {restore:?}");
  eprintln!(
    "restored {name} (crc {:08x})",
    device_crc(&fastboot, start, payload.len())
  );

  if let Err(panic) = outcome {
    std::panic::resume_unwind(panic);
  }
}

fn partition_start(listing: &str, name: &str) -> Option<u64> {
  listing.lines().find_map(|line| {
    let mut fields = line.split_whitespace();
    let _index = fields.next()?;
    let start = fields.next()?.strip_prefix("0x")?;
    let _end = fields.next()?;
    let found = fields.next()?.trim_matches('"');
    (found == name).then(|| u64::from_str_radix(start, 16).ok())?
  })
}

#[cfg(test)]
mod partition_parsing {
  use super::partition_start;

  const LISTING: &str = "\nPartition Map for mmc device 0  --   Partition Type: EFI\n\nPart\tStart LBA\tEnd LBA\t\tName\n\tAttributes\n  1\t0x00002000\t0x00005fff\t\"env\"\n\tattrs:\t0x0000000000000000\n  3\t0x00026000\t0x00127fff\t\"root_a\"\n";

  #[test]
  fn finds_a_partition_start_and_ignores_the_noise() {
    assert_eq!(partition_start(LISTING, "env"), Some(0x2000));
    assert_eq!(partition_start(LISTING, "root_a"), Some(0x26000));
    assert_eq!(partition_start(LISTING, "nope"), None);
  }
}
