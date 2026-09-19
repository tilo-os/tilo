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
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[repr(C)]
pub struct ReadRequest {
    pub serial: u64,
    pub root_record: u64,
    pub root_creation: u64,
    pub object_record: u64,
    pub size: u64,
    pub offset: u64,
    pub length: u32,
    pub root_sequence: u16,
    pub object_sequence: u16,
    #[serde(default)]
    pub reserved: u64,
}
const _: () = assert!(std::mem::size_of::<ReadRequest>() == 64);
const _: () = assert!(std::mem::offset_of!(ReadRequest, length) == 48);
const _: () = assert!(std::mem::offset_of!(ReadRequest, root_sequence) == 52);
const _: () = assert!(std::mem::offset_of!(ReadRequest, reserved) == 56);
#[derive(Debug, Clone, Copy)]
pub enum NativeOperation {
    Probe { root: u64 },
    Manifest { root: u64 },
    ReadChunk { request: ReadRequest },
}
unsafe extern "C" {
    fn qos_ntfs_manifest(
        fd: c_int,
        capacity: u64,
        root: u64,
        read_limit: u64,
        emit: unsafe extern "C" fn(*mut c_void, *const c_char, usize) -> c_int,
        context: *mut c_void,
        stats: *mut Stats,
    ) -> c_int;
    fn qos_ntfs_read_chunk(
        fd: c_int,
        capacity: u64,
        request: *const ReadRequest,
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
pub(crate) fn inspect(
    file: &File,
    capacity: u64,
    operation: &NativeOperation,
    read_limit: u64,
) -> Report {
    let mut counters = Stats::default();
    let mut sink = Sink {
        facts: Vec::new(),
        bytes: 0,
    };
    let status = unsafe {
        match operation {
            NativeOperation::Probe { root } => qos_ntfs_probe(
                file.as_raw_fd(),
                capacity,
                *root,
                read_limit,
                collect,
                (&raw mut sink).cast::<c_void>(),
                &raw mut counters,
            ),
            NativeOperation::Manifest { root } => qos_ntfs_manifest(
                file.as_raw_fd(),
                capacity,
                *root,
                read_limit,
                collect,
                (&raw mut sink).cast::<c_void>(),
                &raw mut counters,
            ),
            NativeOperation::ReadChunk { request } => qos_ntfs_read_chunk(
                file.as_raw_fd(),
                capacity,
                request,
                read_limit,
                collect,
                (&raw mut sink).cast::<c_void>(),
                &raw mut counters,
            ),
        }
    };
    Report {
        schema: if matches!(operation, NativeOperation::Probe { .. }) {
            1
        } else {
            2
        },
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
