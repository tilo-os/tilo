//! Compressed twins: the transport form of content-addressed objects.
//!
//! A `.tapp` at URL `u`, or a payload object at `<base>/blobs/<hash>`, may
//! have a twin at `u.zst`: the same bytes as one zstd frame. Rust binaries
//! are most of what Tilo ships and shrink to about a third, so a client asks
//! for the twin first and falls back to the raw object when there is none.
//!
//! A twin is transport only. Nothing names, hashes or signs the compressed
//! bytes: what a reader trusts is still the hash and length the signed
//! registry entry, the signed collection or the chunk list gives for the
//! raw object. A reader therefore decompresses with [`decompress_bounded`],
//! which refuses output past that length, and then checks the decompressed
//! bytes exactly as it checks a raw download. A small twin cannot expand
//! into more than the object it stands for, and the compressed bytes
//! themselves are bounded by [`compressed_bound`].
//!
//! Twins are written once, when the object is published, at [`LEVEL`] with
//! a window of at most 2^[`WINDOW_LOG_MAX`] bytes, so decompressing one
//! never needs more memory than that, whatever its frame header claims.

use std::io::{self, Read, Write};

use zstd::stream::raw::CParameter;

/// What a twin's URL or file name adds to its object's.
pub const SUFFIX: &str = ".zst";

/// Media type a server labels a twin with. Twins are never sent as
/// `Content-Encoding: zstd`: a client or proxy that decoded transparently
/// would hand over bytes nobody asked it to decode, and caches would key the
/// one URL on two bodies.
pub const MEDIA_TYPE: &str = "application/zstd";

/// zstd level twins are written at. Measured on real packages, level 19
/// cuts app `.tapp`s to 34–42 % and the runtime's libraries to about a third
/// (TIL-600); a higher level gains bytes in the thousandths.
pub const LEVEL: i32 = 19;

/// Largest window, as a power of two, a twin is compressed with and a reader
/// accepts: 8 MiB, what level 19 picks for any object larger than that.
pub const WINDOW_LOG_MAX: u32 = 23;

/// The compressed twin of `bytes`.
///
/// # Errors
///
/// Returns an error if zstd refuses the parameters or the input.
pub fn compress(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut compressor = zstd::bulk::Compressor::new(LEVEL)?;
    compressor.set_parameter(CParameter::WindowLog(WINDOW_LOG_MAX))?;
    compressor.include_contentsize(true)?;
    compressor.include_checksum(false)?;
    compressor.compress(bytes)
}

/// The largest twin a reader accepts for an object of at most `limit`
/// bytes: zstd's worst-case expansion of incompressible input, rounded up.
#[must_use]
pub const fn compressed_bound(limit: u64) -> u64 {
    limit.saturating_add(limit / 128).saturating_add(4096)
}

/// Decompress the twin `compressed` into `out`, refusing to produce more
/// than `limit` bytes, and return how many it produced. `out` may already
/// hold up to `limit + 1` bytes when this fails; callers discard it.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidData`] when the twin is not zstd, needs
/// a larger window than [`WINDOW_LOG_MAX`], or decompresses past `limit`,
/// and any error reading `compressed` or writing `out`.
pub fn decompress_bounded<R: Read, W: Write + ?Sized>(
    compressed: R,
    limit: u64,
    out: &mut W,
) -> io::Result<u64> {
    let mut decoder = zstd::stream::read::Decoder::new(compressed)?;
    decoder.window_log_max(WINDOW_LOG_MAX)?;
    let produced = io::copy(&mut decoder.take(limit.saturating_add(1)), out)?;
    if produced > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("compressed object expands past its {limit}-byte bound"),
        ));
    }
    Ok(produced)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        (0..200_000u32)
            .flat_map(|n| (n % 251).to_le_bytes())
            .collect()
    }

    #[test]
    fn a_twin_round_trips_within_its_bound() {
        let raw = sample();
        let twin = compress(&raw).unwrap();
        assert!(
            twin.len() < raw.len() / 4,
            "{} of {}",
            twin.len(),
            raw.len()
        );
        let mut out = Vec::new();
        let len = decompress_bounded(twin.as_slice(), raw.len() as u64, &mut out).unwrap();
        assert_eq!(len, raw.len() as u64);
        assert_eq!(out, raw);
    }

    #[test]
    fn a_twin_that_expands_past_the_signed_length_is_refused() {
        // 64 MiB of zeros compresses to a few kilobytes: a bomb against a
        // reader that trusted the frame instead of the signed length.
        let bomb = compress(&vec![0u8; 64 << 20]).unwrap();
        assert!(bomb.len() < 64 * 1024);
        let mut out = Vec::new();
        let error = decompress_bounded(bomb.as_slice(), 1 << 20, &mut out).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(out.len() as u64 <= (1 << 20) + 1, "stopped at the bound");

        // One byte over is as refused as a megabyte over.
        let raw = sample();
        let twin = compress(&raw).unwrap();
        let mut out = Vec::new();
        assert!(decompress_bounded(twin.as_slice(), raw.len() as u64 - 1, &mut out).is_err());
    }

    #[test]
    fn a_frame_asking_for_a_larger_window_is_refused() {
        let raw = vec![7u8; 1 << 20];
        // A stream of unknown length keeps the window it was given, so the
        // frame header asks the decoder for 128 MiB.
        let mut wide = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        wide.set_parameter(CParameter::WindowLog(WINDOW_LOG_MAX + 4))
            .unwrap();
        wide.write_all(&raw).unwrap();
        let frame = wide.finish().unwrap();
        // A valid frame: zstd's own default limit (2^27) takes it.
        let mut plain = Vec::new();
        zstd::stream::copy_decode(frame.as_slice(), &mut plain).unwrap();
        assert_eq!(plain, raw);
        let mut out = Vec::new();
        assert!(decompress_bounded(frame.as_slice(), raw.len() as u64, &mut out).is_err());
    }

    #[test]
    fn bytes_that_are_not_zstd_are_refused() {
        let mut out = Vec::new();
        let error = decompress_bounded(&b"not a zstd frame"[..], 1024, &mut out).unwrap_err();
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn the_bound_admits_any_twin_of_an_incompressible_object() {
        let noise: Vec<u8> = (0..1u32 << 18)
            .flat_map(|n| n.wrapping_mul(2_654_435_761).rotate_right(13).to_le_bytes())
            .collect();
        let twin = compress(&noise).unwrap();
        assert!(twin.len() as u64 <= compressed_bound(noise.len() as u64));
    }
}
