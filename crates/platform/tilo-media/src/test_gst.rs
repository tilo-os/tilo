//! What the crate's `GStreamer` tests share.

/// Keep `GStreamer`'s plugin registry cache where every test process of the run
/// finds it.
///
/// `GStreamer` caches what its plugin scan found under the cache home, and the test
/// environment gives each test process a fresh one, so every process that initialised
/// `GStreamer` scanned every installed plugin again: about 0.37 s of a 0.45 s test.
/// Called before a test's first `gst::init`, this names a cache file in the run's
/// fixture directory instead, so the first process scans and the rest read what it
/// wrote. The plugins scanned and the registry they give are the same either way.
///
/// A registry file the environment or the test already named is kept, and a run with
/// no fixture directory keeps the cache home's.
///
/// So does a machine with a system extension merged. A codec pack swaps libraries
/// beneath plugin files whose size and time the cache goes by, and the stamp that makes
/// a process rescan after a merge or an unmerge (`crate::extensions`) is kept in the
/// cache home, which is fresh in every test process. A cache written while a pack was
/// merged would list the pack's elements to a run after the unmerge, where a fresh scan
/// never did; one only ever written without a pack is right whenever none is merged.
pub(crate) fn shared_registry() {
    static NAMED: std::sync::Once = std::sync::Once::new();
    NAMED.call_once(|| {
        let _env = tilo_test_env::lock();
        if ["GST_REGISTRY_1_0", "GST_REGISTRY"]
            .iter()
            .any(|key| std::env::var_os(key).is_some())
        {
            return;
        }
        #[cfg(any(gst_decode, gst_encode, gst_realtime))]
        {
            let merged = crate::extensions::extension_stamp(std::path::Path::new(
                crate::extensions::EXTENSION_RELEASE_DIR,
            ));
            if !merged.is_empty() {
                return;
            }
        }
        let Some(cache) = std::env::var_os("TILO_TEST_CACHE_DIR").filter(|dir| !dir.is_empty())
        else {
            return;
        };
        let cache = std::path::PathBuf::from(cache).join("tilo-media-test-fixtures");
        if std::fs::create_dir_all(&cache).is_ok() {
            tilo_test_env::pin(
                "GST_REGISTRY_1_0",
                cache.join(format!("gstreamer-registry-{}.bin", std::env::consts::ARCH)),
            );
        }
    });
}
