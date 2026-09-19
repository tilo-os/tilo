use super::*;

extern "C" {
    #[link_name = "libspa_rs_meta_check"]
    pub fn spa_meta_check(p: *const std::ffi::c_void, m: *const spa_meta) -> bool;

    #[link_name = "libspa_rs_meta_bitmap_is_valid"]
    pub fn spa_meta_bitmap_is_valid(m: *const spa_meta_bitmap) -> bool;

    #[link_name = "libspa_rs_meta_cursor_is_valid"]
    pub fn spa_meta_cursor_is_valid(m: *const spa_meta_cursor) -> bool;
}
