//! Compatibility facade for the native staging backend.

pub use imager_backend::efi::{
    FIRST_OWNED_BOOT_NUMBER, GLOBAL_GUID, HardDriveLoadOption, LOAD_OPTION_ACTIVE,
    allocate_boot_number, boot_var_name, decode_load_option, encode_load_option, encode_u16,
    format_boot_number, guid_bytes, guid_text, load_option_attributes, parse_boot_number,
    parse_u16_list,
};
