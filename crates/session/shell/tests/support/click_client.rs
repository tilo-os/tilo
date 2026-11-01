//! Where the `click_test_client` Wayland client is.
//!
//! It is its own package (`crates/dev/click-test-client`), so a shell edit
//! does not rebuild it, and cargo hands a test only its own package's binary
//! paths. `tools/ci/nextest.sh` builds it before the shell's tests and names
//! it in `$TILO_CLICK_TEST_CLIENT`; any other run asks
//! [`tilo_test_env::sibling_binary`] for it, which builds it with the profile
//! and in the target directory the test binary was built with.

use std::path::PathBuf;

/// The client's path, built for this run when it was not.
pub fn path() -> PathBuf {
    std::env::var_os("TILO_CLICK_TEST_CLIENT").map_or_else(
        || tilo_test_env::sibling_binary("click-test-client", "click_test_client"),
        PathBuf::from,
    )
}
