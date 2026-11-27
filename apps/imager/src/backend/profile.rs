//! The installation-media profile codec, shared with the standalone media tool.
//!
//! Everything the codec does lives in `install_profile_model::profile`; the
//! imager reaches it through this module.

pub use install_profile_model::profile::{
    PROFILE_BLOCK_SIZE, PROFILE_MAGIC, encode_profile_block, encode_profile_block_bytes,
    encode_profile_payload, write_encoded_profile_block, write_profile_block,
    write_profile_block_bound, write_profile_block_to_file,
};
