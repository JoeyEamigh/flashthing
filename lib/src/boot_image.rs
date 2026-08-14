pub const INFO_SECTOR_BYTES: usize = 512;
pub const BOOT_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub const BOOT_HWPART_BYTES: usize = 2 * 1024 * 1024;
pub const STOCK_BL2_PREFIX: [u8; 8] = [0x0c, 0x62, 0x7a, 0x15, 0xbe, 0x94, 0x07, 0xb2];
const INFO_SECTOR_RESERVED_FROM: usize = 0x18;

pub fn info_sector() -> [u8; INFO_SECTOR_BYTES] {
  let mut sector = [0u8; INFO_SECTOR_BYTES];

  let mut put = |offset: usize, value: u32| sector[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
  put(0x000, 1); // version
  put(0x004, 0x12000); // rsv_base_addr, in sectors
  put(0x008, 0); // dtb.addr — vendor leaves these zero
  put(0x00c, 0); // dtb.size
  put(0x010, 0x4000); // ddr.addr, in sectors, relative to the reserved region
  put(0x014, 4); // ddr.size, in sectors

  let checksum = sector[..INFO_SECTOR_BYTES - 4].chunks_exact(4).fold(0u32, |acc, word| {
    acc.wrapping_add(u32::from_le_bytes(word.try_into().unwrap()))
  });
  sector[INFO_SECTOR_BYTES - 4..].copy_from_slice(&checksum.to_le_bytes());

  sector
}

fn has_info_sector(data: &[u8]) -> bool {
  if data.len() < INFO_SECTOR_BYTES {
    return false;
  }
  if data[INFO_SECTOR_RESERVED_FROM..INFO_SECTOR_BYTES - 4]
    .iter()
    .any(|&byte| byte != 0)
  {
    return false;
  }

  let checksum = data[..INFO_SECTOR_BYTES - 4].chunks_exact(4).fold(0u32, |acc, word| {
    acc.wrapping_add(u32::from_le_bytes(word.try_into().unwrap()))
  });
  checksum == u32::from_le_bytes(data[INFO_SECTOR_BYTES - 4..INFO_SECTOR_BYTES].try_into().unwrap())
}

pub fn needs_info_sector(data: &[u8]) -> bool {
  !has_info_sector(data)
}

pub fn to_boot_image(data: &[u8]) -> Vec<u8> {
  if !needs_info_sector(data) {
    return data[..data.len().min(BOOT_IMAGE_BYTES)].to_vec();
  }

  let mut image = Vec::with_capacity(BOOT_IMAGE_BYTES.min(data.len() + INFO_SECTOR_BYTES));
  image.extend_from_slice(&info_sector());
  image.extend_from_slice(data);
  image.truncate(BOOT_IMAGE_BYTES);
  image
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn info_sector_matches_hardware() {
    let sector = info_sector();
    assert_eq!(&sector[0x000..0x004], &1u32.to_le_bytes());
    assert_eq!(&sector[0x004..0x008], &0x12000u32.to_le_bytes());
    assert_eq!(&sector[0x010..0x014], &0x4000u32.to_le_bytes());
    assert_eq!(&sector[0x014..0x018], &4u32.to_le_bytes());
    assert_eq!(&sector[0x1fc..0x200], &0x16005u32.to_le_bytes());
    assert!(sector[0x018..0x1fc].iter().all(|&b| b == 0));
  }

  #[test]
  fn bare_images_get_an_info_sector() {
    let mut bare = STOCK_BL2_PREFIX.to_vec();
    bare.extend_from_slice(&[0xab; 1024]);

    let image = to_boot_image(&bare);
    assert_eq!(&image[..INFO_SECTOR_BYTES], &info_sector());
    assert_eq!(&image[INFO_SECTOR_BYTES..], &bare[..]);
  }

  #[test]
  fn differently_signed_bootloaders_are_still_bare() {
    let bare: Vec<u8> = (0..4096u32)
      .map(|i| i.wrapping_mul(2654435761).to_le_bytes()[0])
      .collect();
    assert!(!bare.starts_with(&STOCK_BL2_PREFIX));
    assert!(needs_info_sector(&bare));

    let image = to_boot_image(&bare);
    assert_eq!(&image[..INFO_SECTOR_BYTES], &info_sector());
    assert_eq!(&image[INFO_SECTOR_BYTES..], &bare[..]);
  }

  #[test]
  fn prepared_images_are_left_alone() {
    let mut prepared = info_sector().to_vec();
    prepared.extend_from_slice(&STOCK_BL2_PREFIX);

    assert_eq!(to_boot_image(&prepared), prepared);
  }

  #[test]
  fn an_all_zero_info_sector_counts_as_prepared() {
    let mut prepared = vec![0u8; INFO_SECTOR_BYTES];
    prepared.extend_from_slice(&STOCK_BL2_PREFIX);

    assert!(!needs_info_sector(&prepared));
    assert_eq!(to_boot_image(&prepared), prepared);
  }

  #[test]
  fn short_payloads_are_bare() {
    assert!(needs_info_sector(&[0u8; INFO_SECTOR_BYTES - 1]));
  }

  #[test]
  fn a_bad_checksum_is_not_an_info_sector() {
    let mut sector = info_sector();
    sector[0x1fc] ^= 0xff;

    assert!(needs_info_sector(&sector));
  }

  #[test]
  fn a_real_bootloader_fits_the_smaller_hwpart() {
    let bootloader = crate::BOOTLOADER_BIN;
    assert!(bootloader.len() + INFO_SECTOR_BYTES < BOOT_HWPART_BYTES);

    let image = to_boot_image(bootloader);
    assert!(image[BOOT_HWPART_BYTES.min(image.len())..].iter().all(|&b| b == 0));
  }

  #[test]
  fn oversized_images_are_capped() {
    let mut bare = STOCK_BL2_PREFIX.to_vec();
    bare.resize(BOOT_IMAGE_BYTES, 0);

    assert!(needs_info_sector(&bare));
    assert_eq!(to_boot_image(&bare).len(), BOOT_IMAGE_BYTES);
  }
}
