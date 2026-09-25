//! Checked costs for the closed hosted protocol and its pinned dependencies.
use super::{Error, Result, identity};
use automerge::resource_limits::{NativeAllowance, NativeReservation};
use serde::Serialize;

pub(in super::super) fn sum(parts: &[usize]) -> Result<usize> {
    parts.iter().try_fold(0usize, |n, part| {
        n.checked_add(*part).ok_or(Error::Capacity)
    })
}
fn times(n: usize, count: usize) -> Result<usize> {
    n.checked_mul(count).ok_or(Error::Capacity)
}
pub(in super::super) fn reserve(
    budget: &NativeAllowance,
    bytes: usize,
) -> Result<NativeReservation> {
    budget.reserve(bytes).map_err(|_| Error::Capacity)
}

// Pinned Vec<u8>/String initial capacity 8, then geometric growth. For a
// final requested length n, old + new during realloc is less than 3*max(n,8).
pub(in super::super) fn growing_bytes(n: usize) -> Result<usize> {
    times(n.max(8), 3)
}
pub(in super::super) fn error_boxes() -> usize {
    // serde_json1.0.151 ErrorImpl: ErrorCode + two positions; its largest
    // code payload includes Box<str>/io::Error. fix_position overlaps two.
    2 * (size_of::<Box<str>>() + size_of::<std::io::Error>() + 6 * size_of::<usize>())
}
pub(in super::super) fn signature_diagnostic() -> usize {
    // Identity/join bytes64 visitors format this prefix and decimal usize
    // before Error::custom. Count String growth as well as retained text.
    3 * ("expected 64-byte signature, got ".len() + usize::BITS as usize)
}

// Fixed closed error chains: postcard1.1's longest Display, CanonicalError's
// wrapper, DeviceCertError's wrapper, and the error.to_string conversion in
// Cloud enrollment/join. Each can overlap its source string. Dalek2 InternalError
// is at most its ArrayLength arm (three static str/usize pairs + discriminant),
// boxed by signature2 on std, even though this path never invokes batch verify.
pub(in super::super) fn helper_errors(budget: &NativeAllowance) -> Result<NativeReservation> {
    const POSTCARD: usize =
        "Found a varint that didn't terminate. Is the usize too big for this platform?".len();
    const CANONICAL: usize = "postcard serialisation failed: ".len() + POSTCARD;
    const CERT: usize = "canonicalisation failed: ".len() + CANONICAL;
    const SHARED: usize = "membership proof identity does not match device certificate".len();
    const DALEK: usize = "signature error: Verification equation was not satisfied".len();
    let boxed = 3 * (size_of::<&str>() + size_of::<usize>()) + size_of::<usize>();
    reserve(
        budget,
        sum(&[
            growing_bytes(POSTCARD)?,
            growing_bytes(CANONICAL)?,
            growing_bytes(CERT)?,
            growing_bytes(SHARED)?,
            growing_bytes(DALEK)?,
            boxed,
        ])?,
    )
}
pub(in super::super) fn transcript<T: Serialize>(
    value: &T,
    budget: &NativeAllowance,
) -> Result<NativeReservation> {
    // serialized_size uses only the counting flavor. The known views have
    // no custom fallible/allocating serializers; errors map directly inline.
    let size = postcard::experimental::serialized_size(value).map_err(|_| Error::Capacity)?;
    reserve(budget, growing_bytes(size)?)
}
pub(in super::super) fn enrollment_transcript(
    request: &identity::cloud_enrollment::EnrollmentRequest,
    budget: &NativeAllowance,
) -> Result<NativeReservation> {
    transcript(
        &(
            request.version,
            &request.challenge,
            &request.browser_certificate,
            &request.host_certificate,
        ),
        budget,
    )
}
pub(in super::super) fn enrollment_backing(
    challenge: &identity::cloud_enrollment::Challenge,
    host: &identity::DeviceCert,
    browser: &identity::DeviceCert,
    budget: &NativeAllowance,
) -> Result<NativeReservation> {
    // Challenge's three Strings and nested host label (absent for a first
    // claim); direct host and browser certificate labels; two exact 64-byte
    // Vec signatures. Clone allocates exact source length; arrays/capabilities
    // /keys are inline.
    let nested = challenge
        .host_certificate
        .as_ref()
        .map_or(0, |host| host.device_label.len());
    reserve(
        budget,
        sum(&[
            challenge.machine_id.len(),
            challenge.room.len(),
            challenge.workspace_id.len(),
            nested,
            host.device_label.len(),
            browser.device_label.len(),
            64,
            64,
        ])?,
    )
}

// Scan refusal/resource bounds only. Serde still checks actual JSON grammar,
// Unicode, field types and trailing data. Measuring tokens avoids charging a
// megabyte of whitespace as a megabyte of escaped-string scratch.
pub(in super::super) fn parser(
    bytes: &[u8],
    budget: &NativeAllowance,
) -> Result<NativeReservation> {
    let (mut i, mut depth, mut quoted, mut numeric) = (0usize, 0usize, 0usize, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'"' => {
                            i += 1;
                            break;
                        }
                        b'\\' => i = i.saturating_add(2).min(bytes.len()),
                        _ => i += 1,
                    }
                }
                quoted = quoted.max(i - start);
            }
            b'{' | b'[' => {
                depth += 1;
                if depth > 64 {
                    return Err(Error::Capacity);
                }
                i += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                let mut digits = 0;
                while i < bytes.len() && matches!(bytes[i], b'-' | b'.' | b'0'..=b'9') {
                    if bytes[i].is_ascii_digit() {
                        digits += 1;
                        if digits > 20 {
                            return Err(Error::Capacity);
                        }
                    }
                    i += 1;
                }
                if i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
                    i += 1;
                    if i < bytes.len() && matches!(bytes[i], b'+' | b'-') {
                        i += 1;
                    }
                    let mut exponent = 0u32;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        exponent = exponent * 10 + u32::from(bytes[i] - b'0');
                        if exponent > 64 {
                            return Err(Error::Capacity);
                        }
                        i += 1;
                    }
                }
                numeric = numeric.max(i - start);
            }
            _ => i += 1,
        }
    }
    // SliceRead WTF8 requests at most raw quoted span +4. Numeric String
    // initial capacity16, at most token+1. Both include realloc overlap.
    // The same parser scratch also stores ignore_value's one-byte enclosing
    // frames; the lexical depth limit bounds those at64 even for tiny strings.
    let scratch = growing_bytes(quoted.checked_add(4).ok_or(Error::Capacity)?.max(64))?;
    let number = times(numeric.checked_add(1).ok_or(Error::Capacity)?.max(16), 3)?;
    // Unexpected string Debug <=10 bytes per raw byte plus fixed framing.
    // Fixed GuardError prevents arbitrary schema/field-list formatting.
    let diagnostic = growing_bytes(sum(&[times(quoted, 10)?, 128])?)?;
    // Pinned lexical conversion, <=20 significand digits and exponent64:
    // pow5 uses single-limb multiplication; later pow2 reaches at most55
    // limbs. Two vectors, eight bytes/limb, old/new growth overlap3.
    let floating = 2 * 3 * 55 * 8;
    reserve(
        budget,
        sum(&[
            scratch,
            number,
            diagnostic,
            signature_diagnostic(),
            floating,
            error_boxes(),
        ])?,
    )
}
