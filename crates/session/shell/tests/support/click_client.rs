//! Where the `click_test_client` Wayland client is.
//!
//! It is its own package (`crates/dev/click-test-client`), so a shell edit
//! does not rebuild it, and cargo hands a test only its own package's binary
//! paths. `tools/ci/nextest.sh` builds it before the shell's tests and names
//! it in `$TILO_CLICK_TEST_CLIENT`; without that the client is looked up in the
//! target directory the test binary was built into.

use std::path::PathBuf;

/// The client's path, panicking with the build command when it is missing.
pub fn path() -> PathBuf {
    let path = std::env::var_os("TILO_CLICK_TEST_CLIENT").map_or_else(
        || {
            // <target>/<profile>/deps/<test binary> -> <target>/<profile>/click_test_client
            let exe = std::env::current_exe().expect("the test binary's path");
            exe.parent()
                .and_then(std::path::Path::parent)
                .expect("the test binary lives in <target>/<profile>/deps")
                .join("click_test_client")
        },
        PathBuf::from,
    );
    assert!(
        path.is_file(),
        "no click_test_client at {}: build it with `cargo build -p click-test-client` \
         or name it in TILO_CLICK_TEST_CLIENT",
        path.display()
    );
    path
}
