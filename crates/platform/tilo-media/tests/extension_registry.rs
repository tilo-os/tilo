//! The registry follows system extensions, end to end, in one process.
//!
//! A codec-pack merge adds plugin files under `/usr` and an
//! `extension-release` file; an unmerge removes both. This test stands up
//! `GStreamer` on a private, initially empty plugin directory and registry
//! cache, then plays those events with real plugin files copied from the
//! system and checks what the registry offers after
//! [`tilo_media::extensions::refresh_registry_at`].
//!
//! It is its own test binary because the plugin paths must be in the
//! environment before `GStreamer` initialises, and it is one test because the
//! registry is process-global.
#![cfg(any(feature = "decode", feature = "encode", feature = "realtime"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gstreamer as gst;
use tilo_media::extensions::{decode_stamp, extension_stamp, refresh_registry_at};

fn system_plugin(name: &str) -> Option<PathBuf> {
    let from_pkg_config = std::process::Command::new("pkg-config")
        .args(["--variable=pluginsdir", "gstreamer-1.0"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()));
    from_pkg_config
        .into_iter()
        .chain(
            [
                "/usr/lib64/gstreamer-1.0",
                "/usr/lib/x86_64-linux-gnu/gstreamer-1.0",
                "/usr/lib/gstreamer-1.0",
            ]
            .map(PathBuf::from),
        )
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn announce(extensions: &Path, name: &str, modified: SystemTime) {
    let path = extensions.join(format!("extension-release.{name}"));
    std::fs::write(&path, "ID=fedora\nVERSION_ID=44\nSYSEXT_SCOPE=system\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

/// A private directory under Cargo's per-target scratch space, removed at the end.
#[test]
fn registry_follows_extension_merges() {
    let (Some(videotestsrc), Some(audiotestsrc)) = (
        system_plugin("libgstvideotestsrc.so"),
        system_plugin("libgstaudiotestsrc.so"),
    ) else {
        eprintln!("skipping: the system has no videotestsrc/audiotestsrc plugin files");
        return;
    };

    let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let plugins = root.path().join("plugins");
    let extensions = root.path().join("extension-release.d");
    let stamp = root.path().join("cache/tilo/media-extensions.stamp");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::create_dir_all(&extensions).unwrap();

    // GStreamer reads these once, at init, so they stay set for the whole test.
    let mut env = tilo_test_env::ScopedEnv::new();
    for key in ["GST_PLUGIN_SYSTEM_PATH_1_0", "GST_PLUGIN_SYSTEM_PATH"] {
        env.set(key, &plugins);
    }
    for key in ["GST_PLUGIN_PATH_1_0", "GST_PLUGIN_PATH"] {
        env.remove(key);
    }
    for key in ["GST_REGISTRY_1_0", "GST_REGISTRY"] {
        env.set(key, root.path().join("registry.bin"));
    }
    // The default-path refresh (encoder selection below) must not touch
    // the developer's own stamp.
    env.set("XDG_CACHE_HOME", root.path().join("cache"));
    env.remove("GST_REGISTRY_UPDATE");
    gst::init().unwrap();
    let registry = gst::Registry::get();

    // Nothing merged, nothing recorded: no rebuild, and no element.
    assert!(!refresh_registry_at(&extensions, Some(&stamp)));
    assert!(gst::ElementFactory::find("videotestsrc").is_none());

    // Merge: plugin files appear with their extension-release file.
    std::fs::copy(&videotestsrc, plugins.join("libgstvideotestsrc.so")).unwrap();
    std::fs::copy(&audiotestsrc, plugins.join("libgstaudiotestsrc.so")).unwrap();
    let t0 = SystemTime::now();
    announce(&extensions, "tilo-codecs", t0);
    assert!(refresh_registry_at(&extensions, Some(&stamp)));
    assert!(gst::ElementFactory::find("videotestsrc").is_some());
    assert!(gst::ElementFactory::find("audiotestsrc").is_some());
    assert_eq!(
        decode_stamp(&std::fs::read_to_string(&stamp).unwrap()),
        Some(extension_stamp(&extensions)),
        "the stamp the cache now reflects is recorded for later processes"
    );
    // Unchanged extensions: construction stays cheap.
    assert!(!refresh_registry_at(&extensions, Some(&stamp)));

    // What plain gst_update_registry does with an unchanged plugin file: it
    // keeps the cached plugin and its feature list. That is why a library
    // replaced beneath an unchanged plugin (libavcodec under libgstlibav)
    // needs more than an update.
    let cached = registry.find_plugin("videotestsrc").unwrap();
    assert!(!cached.is_loaded());
    gst::Registry::update().unwrap();
    assert_eq!(
        registry.find_plugin("videotestsrc").unwrap(),
        cached,
        "gst_update_registry keeps an unchanged plugin's cached entry"
    );

    // A rebuilt extension (new mtime) re-registers every plugin this process
    // has not loaded from the file on disk.
    announce(&extensions, "tilo-codecs", t0 + Duration::from_secs(60));
    assert!(refresh_registry_at(&extensions, Some(&stamp)));
    let rescanned = registry.find_plugin("videotestsrc").unwrap();
    assert_ne!(rescanned, cached, "the unloaded plugin was scanned again");
    assert!(gst::ElementFactory::find("videotestsrc").is_some());

    // A loaded plugin cannot be replaced in-process: it stays as it is.
    let element = gst::ElementFactory::make("videotestsrc").build().unwrap();
    let loaded = registry.find_plugin("videotestsrc").unwrap();
    assert!(loaded.is_loaded());
    announce(&extensions, "tilo-codecs", t0 + Duration::from_secs(120));
    assert!(refresh_registry_at(&extensions, Some(&stamp)));
    assert_eq!(registry.find_plugin("videotestsrc").unwrap(), loaded);
    drop(element);

    // Unmerge: the unloaded plugin's elements go away with its file.
    std::fs::remove_file(plugins.join("libgstaudiotestsrc.so")).unwrap();
    std::fs::remove_file(extensions.join("extension-release.tilo-codecs")).unwrap();
    assert!(refresh_registry_at(&extensions, Some(&stamp)));
    assert!(gst::ElementFactory::find("audiotestsrc").is_none());
    assert_eq!(
        decode_stamp(&std::fs::read_to_string(&stamp).unwrap()),
        Some(Vec::new())
    );

    // The public lookups the engines use go through the same refresh.
    assert!(tilo_media::encode::registry_has("videotestsrc"));
    assert!(!tilo_media::encode::registry_has("audiotestsrc"));
}
