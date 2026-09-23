//! `EFI_LOAD_OPTION` encoding for the owned installer boot entry, shared by
//! both platforms so one tested encoder produces the bytes firmware sees.

/// `EFI_GLOBAL_VARIABLE` vendor GUID.
pub const GLOBAL_GUID: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
pub const LOAD_OPTION_ACTIVE: u32 = 0x0000_0001;
/// Owned entries are allocated at or above this number.
pub const FIRST_OWNED_BOOT_NUMBER: u16 = 0x0100;

/// Inputs for an HD(...)/File(...) load option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardDriveLoadOption {
    pub description: String,
    pub partition_number: u32,
    pub start_lba: u64,
    pub size_lba: u64,
    pub partition_guid: String,
    /// Backslash-separated, e.g. `\EFI\BOOT\BOOTX64.EFI`.
    pub file_path: String,
}

/// `Boot####` variable name for a boot number.
pub fn boot_var_name(number: u16) -> String {
    format!("Boot{number:04X}")
}

/// Encode an `EFI_LOAD_OPTION` (attributes ACTIVE, HD + File + End device path).
pub fn encode_load_option(option: &HardDriveLoadOption) -> Result<Vec<u8>, String> {
    let _ = option;
    Err("load option encoding not implemented".into())
}

/// Decode what [`encode_load_option`] produces (and firmware-written HD/File options).
pub fn decode_load_option(bytes: &[u8]) -> Result<HardDriveLoadOption, String> {
    let _ = bytes;
    Err("load option decoding not implemented".into())
}
