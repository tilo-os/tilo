//! A small image filter, built for `wasm32-unknown-unknown`: the kind of library an app ships
//! as wasm, here so `tilolang bench --library` can time each engine on code compiled from Rust.
//!
//! The library owns its image: [`fill`] makes a deterministic RGBA test image and [`blur`]
//! runs passes of a 3x3 box blur over it, returning a checksum of the result, so every engine's
//! output can be compared.

#![no_std]

use core::ptr::addr_of_mut;

/// The largest image, in bytes: 1024 x 1024 RGBA.
const MAX_BYTES: usize = 1024 * 1024 * 4;

static mut FRONT: [u8; MAX_BYTES] = [0; MAX_BYTES];
static mut BACK: [u8; MAX_BYTES] = [0; MAX_BYTES];
static mut SIZE: (usize, usize) = (0, 0);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    core::arch::wasm32::unreachable()
}

/// The two image buffers and the image's size. The module runs one call at a time, so nothing
/// else holds them.
fn image() -> (&'static mut [u8], &'static mut [u8], usize, usize) {
    // SAFETY: wasm32 here has one thread and the exports never re-enter, so these are the only
    // references to the statics while a call runs.
    unsafe {
        let (width, height) = *addr_of_mut!(SIZE);
        (
            &mut *addr_of_mut!(FRONT),
            &mut *addr_of_mut!(BACK),
            width,
            height,
        )
    }
}

/// Make a `width` x `height` RGBA image from `seed`. Returns 0, or -1 when it does not fit.
#[unsafe(no_mangle)]
pub extern "C" fn fill(width: i32, height: i32, seed: i32) -> i32 {
    let (Ok(width), Ok(height)) = (usize::try_from(width), usize::try_from(height)) else {
        return -1;
    };
    if width < 3 || height < 3 || width.saturating_mul(height).saturating_mul(4) > MAX_BYTES {
        return -1;
    }
    // SAFETY: as in `image`.
    unsafe { *addr_of_mut!(SIZE) = (width, height) };
    let (front, _, _, _) = image();
    let mut state = u32::from_ne_bytes(seed.to_ne_bytes()) | 1;
    for byte in &mut front[..width * height * 4] {
        // xorshift32
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *byte = state.to_le_bytes()[0];
    }
    0
}

/// Run `passes` of a 3x3 box blur over the image, each channel apart, the border kept, and
/// return a checksum of the result.
#[unsafe(no_mangle)]
pub extern "C" fn blur(passes: i32) -> i64 {
    let (mut src, mut dst, width, height) = image();
    let stride = width * 4;
    let len = stride * height;
    for _ in 0..passes {
        dst[..len].copy_from_slice(&src[..len]);
        for y in 1..height - 1 {
            for x in 1..width - 1 {
                for c in 0..4 {
                    let at = y * stride + x * 4 + c;
                    let mut sum = 0u32;
                    for row in [at - stride, at, at + stride] {
                        sum +=
                            u32::from(src[row - 4]) + u32::from(src[row]) + u32::from(src[row + 4]);
                    }
                    dst[at] = u8::try_from(sum / 9).unwrap_or(u8::MAX);
                }
            }
        }
        core::mem::swap(&mut src, &mut dst);
    }
    let checksum = src[..len].iter().fold(0u32, |acc, &b| {
        acc.wrapping_mul(31).wrapping_add(u32::from(b))
    });
    if passes % 2 == 1 {
        // The result is in BACK; leave it in FRONT for the next call.
        dst[..len].copy_from_slice(&src[..len]);
    }
    i64::from(checksum)
}
