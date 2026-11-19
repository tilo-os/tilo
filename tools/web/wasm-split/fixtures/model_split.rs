//! A small combined model build for the splitter's tests (src/tests.rs,
//! `models`): two apps, `alpha` and `beta`, each with an entry the device
//! calls, as `tilo_model_guest::model!` declares one, and the model ABI's
//! import. Nothing here is a wasm-bindgen export: the module has no
//! `__wbindgen_start`.
//!
//! - `SHARED` and `shared_mix` are both apps': they go to the runtime;
//! - `ALPHA` and `alpha_own`, `BETA` and `beta_own`, are one app's each: they
//!   go to that app's module.

#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

#[link(wasm_import_module = "tilo_model")]
unsafe extern "C" {
    fn request_length() -> i32;
}

/// 64 words from `seed`, made at compile time.
const fn table(seed: u32) -> [u32; 64] {
    let mut out = [0u32; 64];
    let mut state = seed | 1;
    let mut i = 0;
    while i < 64 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        out[i] = state;
        i += 1;
    }
    out
}

static SHARED: [u32; 64] = table(0x1234_5678);
static ALPHA: [u32; 64] = table(0x0a1f_a000);
static BETA: [u32; 64] = table(0x0b37_a000);

#[inline(never)]
fn shared_mix(x: u32) -> u32 {
    let word = core::hint::black_box(&SHARED)[(x % 64) as usize];
    word.rotate_left(x % 31) ^ x
}

#[inline(never)]
fn alpha_own(x: u32) -> u32 {
    core::hint::black_box(&ALPHA)[(x % 64) as usize].wrapping_mul(3)
}

#[inline(never)]
fn beta_own(x: u32) -> u32 {
    core::hint::black_box(&BETA)[(x.wrapping_mul(7) % 64) as usize].wrapping_add(x)
}

#[unsafe(export_name = "tilo_model_apply_alpha")]
pub extern "C" fn alpha() -> i32 {
    // SAFETY: the device answers the import.
    let n = unsafe { request_length() } as u32;
    (alpha_own(n) ^ shared_mix(n)) as i32
}

#[unsafe(export_name = "tilo_model_apply_beta")]
pub extern "C" fn beta() -> i32 {
    // SAFETY: as for `alpha`.
    let n = unsafe { request_length() } as u32;
    beta_own(n).wrapping_add(shared_mix(n ^ 5)) as i32
}

#[unsafe(link_section = "tilo_split_exports")]
#[used]
static ENTRIES: [u8; 56] = *b"alpha tilo_model_apply_alpha\nbeta tilo_model_apply_beta\n";
