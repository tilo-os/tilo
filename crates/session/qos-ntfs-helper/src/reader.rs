use crate::protocol::{Report, Stats};
use std::{
    ffi::{c_char, c_int, c_void},
    fs::File,
    os::fd::AsRawFd,
};
unsafe extern "C" {
    fn qos_ntfs_probe(
        fd: c_int,
        capacity: u64,
        root: u64,
        read_limit: u64,
        emit: unsafe extern "C" fn(*mut c_void, *const c_char, usize) -> c_int,
        context: *mut c_void,
        stats: *mut Stats,
    ) -> c_int;
}
struct Sink {
    facts: Vec<serde_json::Value>,
    bytes: usize,
}
unsafe extern "C" fn collect(context: *mut c_void, data: *const c_char, size: usize) -> c_int {
    // The C reader owns the callback buffer for this call and supplies this exact Sink.
    let sink = unsafe { &mut *context.cast::<Sink>() };
    if size > 65_536 || sink.bytes.saturating_add(size) > 4 * 1024 * 1024 {
        return -1;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) };
    let Ok(value) = serde_json::from_slice(bytes) else {
        return -1;
    };
    sink.bytes = sink.bytes.saturating_add(size);
    sink.facts.push(value);
    0
}
/// Reads native diagnostic facts from an already readonly descriptor.
/// Call only in a bounded child; this function grants no production authority.
#[must_use]
pub(crate) fn inspect(file: &File, capacity: u64, root: u64, read_limit: u64) -> Report {
    let mut counters = Stats::default();
    let mut sink = Sink {
        facts: Vec::new(),
        bytes: 0,
    };
    let status = unsafe {
        qos_ntfs_probe(
            file.as_raw_fd(),
            capacity,
            root,
            read_limit,
            collect,
            (&raw mut sink).cast::<c_void>(),
            &raw mut counters,
        )
    };
    Report {
        schema: 1,
        source_pin: "d327833ec1d5eb1358b6f2c37139f10a3460944d".into(),
        diagnostic_only: true,
        memory_limit_enforced: false,
        native_identity: None,
        metadata_complete: false,
        status,
        facts: sink.facts,
        stats: counters,
    }
}
