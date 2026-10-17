//! Isolated host discovery inputs shared by native and frontend scenario tests.
use std::{
    path::Path,
    sync::{Mutex, OnceLock},
};
static PLATFORM: OnceLock<Mutex<Option<std::path::PathBuf>>> = OnceLock::new();

pub fn fixture_mount_root() -> Option<std::path::PathBuf> {
    PLATFORM
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone()
}
pub struct PlatformFixture;
pub fn source_fixture(root: &Path) -> impl Drop {
    PlatformFixture::install(root)
}
impl PlatformFixture {
    pub fn install(root: &Path) -> Self {
        let mut selected = PLATFORM.get_or_init(|| Mutex::new(None)).lock().unwrap();
        assert!(selected.is_none());
        *selected = Some(root.to_owned());
        Self
    }
}
impl Drop for PlatformFixture {
    fn drop(&mut self) {
        *PLATFORM.get_or_init(|| Mutex::new(None)).lock().unwrap() = None;
    }
}
pub fn fixture_source_facts() -> Option<(Vec<u8>, serde_json::Value)> {
    // Native journeys discover the same GPT the standalone helper will resolve.
    if std::env::var_os("TILO_NATIVE_SHARED_FILES_ACTOR_ROOT").is_some() {
        return None;
    }
    let root = fixture_mount_root()?;
    let mount = format!(
        "24 1 8:1 / {} rw,relatime - ext4 /dev/storydisk1 rw\n",
        root.display()
    );
    let devices = serde_json::json!({"blockdevices":[{
        "name":"/dev/storydisk","type":"disk","size":512u64<<30,
        "pttype":"gpt","ptuuid":"11111111-2222-4333-8444-555555555555","log-sec":512,
        "children":[{"name":"/dev/storydisk1","type":"part","size":(128u64<<30)-1048576,
        "start":2048,"fstype":"ext4","uuid":"33333333-3333-4333-8333-333333333333",
        "partuuid":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee","maj:min":"8:1"}]
    }]});
    Some((mount.into_bytes(), devices))
}
