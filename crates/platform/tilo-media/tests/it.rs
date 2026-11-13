//! The decode tests that can share a process, as one test binary.
//!
//! Every binary that reads the generated clip encodes it once per run (about three
//! seconds: a test job starts with an empty fixture cache), scans the plugin registry
//! once and links the whole crate. A binary per file did each of those once per file.
//! `#[path]` keeps the files where they are, and the test runner starts each test in a
//! process of its own, as it did when they were separate binaries.
//!
//! Run in one process instead (`cargo test`), the tests that open an engine take
//! `common::registry_policy_lock`: an engine applies its decoder policy to the
//! process-wide registry, so one that opened while another autoplugged could change
//! which decoder that one gets.
//!
//! The binaries that stay apart need a process to themselves: `early_seek` loads the
//! CPUs it confines itself to, `no_audio_device`, `no_plugins` and `extension_registry`
//! set the environment `GStreamer` or its audio sink reads once, and `realtime` holds
//! the latency measurement that is run by its binary's name.

mod common;

#[path = "accurate_seek.rs"]
mod accurate_seek;
#[path = "hardware_decode.rs"]
mod hardware_decode;
#[path = "playback.rs"]
mod playback;
#[path = "realtime_stream_color.rs"]
mod realtime_stream_color;
#[path = "software_fallback.rs"]
mod software_fallback;
