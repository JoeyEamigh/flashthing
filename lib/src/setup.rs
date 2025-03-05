use std::{fs, path::PathBuf, process::Command};

use crate::{Result, PRODUCT_ID, PRODUCT_ID_BOOTED, VENDOR_ID, VENDOR_ID_BOOTED};

#[cfg(target_os = "linux")]
pub fn setup_host_linux() -> Result<()> {
  let rules_path = PathBuf::from("/etc/udev/rules.d/98-superbird.rules");

  let username = whoami::username()?;
  let rules_content = format!(
      "SUBSYSTEM==\"usb\", ATTRS{{idVendor}}==\"{:04x}\", ATTRS{{idProduct}}==\"{:04x}\", OWNER=\"{}\", MODE=\"0666\"\n\
       SUBSYSTEM==\"usb\", ATTRS{{idVendor}}==\"{:04x}\", ATTRS{{idProduct}}==\"{:04x}\", OWNER=\"{}\", MODE=\"0666\"\n",
      VENDOR_ID, PRODUCT_ID, username,
      VENDOR_ID_BOOTED, PRODUCT_ID_BOOTED, username
    );

  let temp_dir = std::env::temp_dir();
  let temp_file_path = temp_dir.join("98-superbird.rules");
  fs::write(&temp_file_path, &rules_content)?;
  tracing::debug!("created temporary rules file at: {}", temp_file_path.display());

  let pkexec_result = Command::new("pkexec")
    .args(["cp", &temp_file_path.to_string_lossy(), &rules_path.to_string_lossy()])
    .status();

  if let Ok(status) = pkexec_result {
    if status.success() {
      tracing::debug!("successfully installed udev rules using polkit");
      let reload_result = Command::new("pkexec")
        .args(["udevadm", "control", "--reload-rules"])
        .status();

      if let Ok(status) = reload_result {
        if status.success() {
          let _ = Command::new("pkexec").args(["udevadm", "trigger"]).status()?;

          tracing::info!("successfully activated udev rules. Device should now be accessible.");
          let _ = fs::remove_file(&temp_file_path);
          return Ok(());
        }
      }

      tracing::warn!("installed rules but failed to reload automatically. please run:");
      tracing::warn!("  sudo udevadm control --reload-rules && sudo udevadm trigger");
    } else {
      tracing::warn!("polkit authentication failed or was canceled");
    }
  } else {
    tracing::warn!("failed to execute pkexec - polkit might not be available");
  }

  tracing::info!("to install the rules manually, run the following commands:");
  tracing::info!("  sudo cp {} /etc/udev/rules.d/", temp_file_path.display());
  tracing::info!("  sudo udevadm control --reload-rules && sudo udevadm trigger");

  Ok(())
}
