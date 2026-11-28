//! GPT partition-type and EFI vendor GUIDs shared by the imager, the installer and the staging
//! helper.

/// Type GUID of an EFI System Partition.
pub const EFI_SYSTEM: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// Type GUID of a plain Linux filesystem.
pub const LINUX_FILESYSTEM: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
/// Type GUID of the Tilo root filesystem.
pub const TILO_ROOT: &str = "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab";
/// Type GUID of the temporary installer root; only the installer UKI resolves it.
pub const TILO_STAGING_ROOT: &str = "d4c4daf6-f72c-43ee-b8e3-5ba32550311d";
/// `EFI_GLOBAL_VARIABLE`, the vendor GUID of the firmware's boot variables.
pub const EFI_GLOBAL_VARIABLE: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
/// The systemd boot loader vendor GUID.
pub const SYSTEMD_LOADER: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
