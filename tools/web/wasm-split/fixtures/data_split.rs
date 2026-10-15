//! A small program for the splitter's tests (src/tests.rs), which compiles
//! it for wasm32 the way the desktop bundle is linked (`--emit-relocs`, the
//! symbol table kept), splits it, and runs the pieces. The test writes the
//! `*.bin` blobs beside it first.
//!
//! Two split points, `reader` and `tabler`, reach static data in the ways a
//! split can get wrong:
//! - `READER_BLOB`, read only by `reader` at an offset computed at run
//!   time, and `READER_STATE`, a mutable static only `reader` changes;
//! - `TABLE`, a static table of slices into `TABLE_BLOB`, both only
//!   `tabler`'s: data reached through other data;
//! - `SHARED_BLOB`, which both split points read;
//! - `LISTED_BLOB`, which `reader` reads but the start module's
//!   `START_TABLE` points into;
//! - `START_BLOB`, the start module's alone.
//!
//! `reader` also hands a pointer into its own blob to `checksum`, a start
//! module function.

#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

const LEN: usize = 4096;

static READER_BLOB: [u8; LEN] = *include_bytes!("reader.bin");
static TABLE_BLOB: [u8; LEN] = *include_bytes!("table.bin");
static SHARED_BLOB: [u8; LEN] = *include_bytes!("shared.bin");
static LISTED_BLOB: [u8; LEN] = *include_bytes!("listed.bin");
static START_BLOB: [u8; LEN] = *include_bytes!("start.bin");

/// `blob[start..end]`, in a static's initializer.
const fn part(blob: &'static [u8; LEN], start: usize, end: usize) -> &'static [u8] {
    blob.as_slice().split_at(start).1.split_at(end - start).0
}

static TABLE: [&[u8]; 6] = [
    part(&TABLE_BLOB, 0, 100),
    part(&TABLE_BLOB, 100, 700),
    part(&TABLE_BLOB, 700, 1500),
    part(&TABLE_BLOB, 1500, 2048),
    part(&TABLE_BLOB, 2048, 3000),
    part(&TABLE_BLOB, 3000, LEN),
];

static START_TABLE: [&[u8]; 2] = [part(&LISTED_BLOB, 0, 64), part(&LISTED_BLOB, 64, LEN)];

static mut READER_STATE: [u32; 16] = [
    0x1111_1111,
    0x2222_2222,
    0x3333_3333,
    0x4444_4444,
    0x5555_5555,
    0x6666_6666,
    0x7777_7777,
    0x8888_8888,
    0x9999_9999,
    0xaaaa_aaaa,
    0xbbbb_bbbb,
    0xcccc_cccc,
    0xdddd_dddd,
    0xeeee_eeee,
    0x1234_5678,
    0x0fed_cba9,
];

/// Methods called through the function table, so the module has one.
static OPS: [fn(u32) -> u32; 2] = [double, triple];

#[inline(never)]
fn double(x: u32) -> u32 {
    x.wrapping_mul(2)
}

#[inline(never)]
fn triple(x: u32) -> u32 {
    x.wrapping_mul(3)
}

/// An offset into a blob that only the run time knows.
fn offset(i: u32) -> usize {
    (i.wrapping_mul(2_654_435_761) >> 7) as usize % LEN
}

/// A start module function the split modules hand their pointers to.
#[inline(never)]
fn checksum(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |sum, byte| sum.rotate_left(5) ^ u32::from(*byte))
}

#[link(wasm_import_module = "./__wasm_split.js")]
unsafe extern "C" {
    #[link_name = "__wasm_split_00___reader___00_import_00000000000000a1_read"]
    fn reader_import(i: u32) -> u32;
    #[link_name = "__wasm_split_00___tabler___00_import_00000000000000b2_pick"]
    fn tabler_import(i: u32) -> u32;
}

/// `reader`'s entry. `i` with the top bit set steps `READER_STATE` and
/// returns its new value; otherwise it reads its blobs at `offset(i)`.
#[unsafe(export_name = "__wasm_split_00___reader___00_export_00000000000000a1_read")]
pub extern "C" fn reader_export(i: u32) -> u32 {
    if i & 0x8000_0000 != 0 {
        let slot = (i as usize) % 16;
        // SAFETY: single-threaded; no reference to the static outlives this.
        unsafe {
            let state = &mut *core::ptr::addr_of_mut!(READER_STATE);
            state[slot] = state[slot].wrapping_add(1);
            return state[slot];
        }
    }
    let at = offset(i);
    let end = (at + 16).min(LEN);
    u32::from(READER_BLOB[at])
        | u32::from(SHARED_BLOB[at]) << 8
        | u32::from(LISTED_BLOB[at]) << 16
        | (checksum(&READER_BLOB[at..end]) & 0xff) << 24
}

/// `tabler`'s entry: a byte of `TABLE`'s slice `i % 6`, at a position
/// computed from `i`, and a byte of the shared blob.
#[unsafe(export_name = "__wasm_split_00___tabler___00_export_00000000000000b2_pick")]
pub extern "C" fn tabler_export(i: u32) -> u32 {
    let slice = TABLE[(i % 6) as usize];
    let at = offset(i) % slice.len();
    u32::from(slice[at]) | u32::from(SHARED_BLOB[offset(i)]) << 8
}

#[unsafe(no_mangle)]
pub extern "C" fn call_reader(i: u32) -> u32 {
    // SAFETY: the split point; the test loads its module first.
    unsafe { reader_import(i) }
}

#[unsafe(no_mangle)]
pub extern "C" fn call_tabler(i: u32) -> u32 {
    // SAFETY: as for `call_reader`.
    unsafe { tabler_import(i) }
}

/// The start module's own reads: its blob, and `LISTED_BLOB` through its
/// table, each with `checksum`.
#[unsafe(no_mangle)]
pub extern "C" fn start_read(i: u32) -> u32 {
    let at = offset(i);
    let listed = START_TABLE[(i % 2) as usize];
    let end = (at + 16).min(LEN);
    u32::from(START_BLOB[at])
        | u32::from(listed[at % listed.len()]) << 8
        | (checksum(&START_BLOB[at..end]) & 0xff) << 16
        | (core::hint::black_box(OPS[(i % 2) as usize])(i) & 0xff) << 24
}

/// The entry wasm-bindgen would export; the splitter anchors the start
/// module's call graph on it.
#[unsafe(no_mangle)]
pub extern "C" fn __wbindgen_start() {}
