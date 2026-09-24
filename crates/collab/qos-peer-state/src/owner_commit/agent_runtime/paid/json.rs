//! Shape inventory only. It never accepts/rejects JSON or replaces serde's error.
use super::*;
#[derive(Default)]
struct Shape {
    strings: usize,
    string_bytes: usize,
    cells: usize,
    containers: usize,
    numbers: usize,
    longest_string: usize,
    longest_number: usize,
}
fn scan(bytes: &[u8]) -> Result<Shape> {
    let mut s = Shape::default();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'"' => {
                            i += 1;
                            break;
                        }
                        b'\\' => i = i.saturating_add(2),
                        _ => i += 1,
                    }
                }
                let n = i.saturating_sub(start).min(bytes.len() - start);
                s.strings = sum(&[s.strings, 1])?;
                s.string_bytes = sum(&[s.string_bytes, n])?;
                s.longest_string = s.longest_string.max(n);
                s.cells = sum(&[s.cells, 1])?;
            }
            b'[' | b'{' => {
                s.containers = sum(&[s.containers, 1])?;
                s.cells = sum(&[s.cells, 1])?;
                i += 1
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < bytes.len()
                    && matches!(bytes[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    i += 1
                }
                s.numbers = sum(&[s.numbers, 1])?;
                s.longest_number = s.longest_number.max(i - start);
                s.cells = sum(&[s.cells, 1])?;
            }
            b' ' | b'\n' | b'\r' | b'\t' | b',' | b':' | b']' | b'}' => i += 1,
            _ => {
                s.cells = sum(&[s.cells, 1])?;
                i += 1
            }
        }
    }
    Ok(s)
}
// serde Content's largest owned variant is Vec<(Content,Content)>; the
// repr(C) wide discriminator plus payload bounds its private Rust enum layout.
// u64 also preserves the alignment of Content::U64/F64 on 32-bit targets;
// a pointer-sized discriminator would undercount those variants there.
#[repr(C)]
struct Content {
    discriminant: u64,
    payload: [usize; 3],
}
fn content(s: &Shape) -> Result<usize> {
    sum(&[
        s.string_bytes,
        growth(s.cells, size_of::<(Content, Content)>())?,
        mul(s.containers, 4 * size_of::<(Content, Content)>())?,
    ])
}
fn value(s: &Shape) -> Result<usize> {
    sum(&[
        s.string_bytes,
        nodes::<String, serde_json::Value>(s.strings)?,
        growth(s.cells, size_of::<serde_json::Value>())?,
        mul(s.containers, 4 * size_of::<serde_json::Value>())?,
    ])
}
fn parser(s: &Shape) -> Result<usize> {
    // Same full-finite lexical proof as reviewed Cloud parser: moderate powers
    // -350..309; bhcomp769 digits; conservative277 limbs incl shift/rounding.
    const SPAN: usize = 350 + 769 + 20;
    const LIMBS: usize = (4 * 769 + 3 * SPAN + (1075 + SPAN) + 128).div_ceil(32);
    let frames = (usize::BITS - (LIMBS - 1).leading_zeros()) as usize + 1;
    let floating = (8 * frames + 2) * 3 * (2 * LIMBS) * 8;
    sum(&[
        growth(sum(&[s.longest_string, 4])?, 1)?,
        mul(s.longest_number.checked_add(1).ok_or(Capacity)?.max(16), 3)?,
        floating,
        diagnostics(s.longest_string.max(s.longest_number))?,
    ])
}
pub(super) fn diagnostics(longest: usize) -> Result<usize> {
    // Fixed pieces cover every known schema's expected-field/variant list,
    // validator static message, and each wrapper prefix. These are summed,
    // although only one parser error uses one list. Position formatting has two
    // usize decimal fields. Debug escaping expands a single input byte by at
    // most six (`\\u00XX`); this is error spelling, not payload heap sizing.
    const SPELLINGS: &str = concat!(
        "version kind name task grant_set_id agent_id text cursor invocation_id ",
        "agent_capabilities agent_list agent_conversation agent_spawn agent_message agent_status ",
        "message_id task_id run_id launch_id runtime_epoch recipient_agent channel_lease ",
        "version task_id title run_id lease planning action_context ",
        "action_label app resource purpose group facets id kind label key value ",
        "invalid private task context: agent protocol JSON is invalid: ",
        "unknown variant missing field duplicate field unknown field expected one of invalid type invalid value at line column ",
        "private agent outcome is not backed by exact receipts private ledger retained/reserved budget exhausted ",
        "canonicalisation failed: postcard serialisation failed: device-cert signature did not verify ",
        "invalid character: expected an optional prefix of `urn:uuid:` followed by [0-9a-fA-F-], found "
    );
    let text = sum(&[
        SPELLINGS.len(),
        mul(longest, 6)?,
        2 * (usize::BITS as usize),
        2,
    ])?;
    // serde boxed error payload + its formatted String; protocol wraps/formats
    // that String once more; final validator map_err formats it again. Growth
    // includes old/new backing overlap for each simultaneously live string.
    sum(&[
        mul(growth(text, 1)?, 3)?,
        4 * (size_of::<Box<str>>() + size_of::<std::io::Error>() + 6 * size_of::<usize>()),
    ])
}
pub(super) fn string_bytes(bytes: &[u8]) -> Result<usize> {
    Ok(scan(bytes)?.string_bytes)
}
// zmij1.0.21 BUFFER_SIZE=24; allow two JSON delimiters per numeric token.
pub(super) fn request(bytes: &[u8]) -> Result<usize> {
    if bytes.len() > qos_agent_control_protocol::MAX_REQUEST_FRAME_BYTES {
        return diagnostics(0);
    }
    let s = scan(bytes)?;
    // Original Content plus owned DTO; normalize clones SpawnInput then replaces
    // two strings. canonical_request clones DTO, repeats normalization and writes
    // canonical output. Account each named owned string set, not frame bytes.
    let dto = s.string_bytes;
    let normalized = sum(&[dto, dto, dto])?;
    let canonical_copy = sum(&[dto, dto, dto])?;
    let output = growth(
        sum(&[mul(dto, 6)?, mul(s.numbers, 24 + 2)?, bytes.len()])?,
        1,
    )?;
    sum(&[
        parser(&s)?,
        content(&s)?,
        normalized,
        canonical_copy,
        output,
    ])
}
/// One exact child result: a single Unique Value parse, no typed decode.
pub(super) fn step_result(bytes: &[u8]) -> Result<usize> {
    if bytes.len() > MAX_AGENT_STEP_RESULT_BYTES {
        return diagnostics(0);
    }
    let s = scan(bytes)?;
    sum(&[parser(&s)?, value(&s)?])
}
pub(super) fn envelope(bytes: &[u8]) -> Result<usize> {
    if bytes.len() > 128 * 1024 {
        return diagnostics(0);
    }
    let s = scan(bytes)?;
    let tree = value(&s)?;
    // Raw Value, raw.clone consumed by typed Envelope, context clone consumed
    // by ActionContext, and reconstructed serde_json::to_value tree. Retaining
    // all four is conservative for the actual partially overlapping lifetimes.
    // Facets are the only typed sequence: each possible cell pays two Strings.
    let typed = sum(&[s.string_bytes, mul(s.cells, 2 * size_of::<String>())?])?;
    let output = growth(
        sum(&[
            bytes.len(),
            mul(s.string_bytes, 6)?,
            mul(s.numbers, 24 + 2)?,
        ])?,
        1,
    )?;
    sum(&[parser(&s)?, tree, tree, tree, tree, typed, typed, output])
}

#[cfg(test)]
mod tests {
    use super::Content;
    use std::mem::{align_of, size_of};

    #[test]
    fn private_validation_paid_content_layout_bounds_pinned_serde() {
        type Actual = serde::__private228::de::Content<'static>;
        assert!(size_of::<Content>() >= size_of::<Actual>());
        assert!(align_of::<Content>() >= align_of::<Actual>());
        assert!(size_of::<(Content, Content)>() >= size_of::<(Actual, Actual)>());

        // Model both common 32-bit wide-scalar alignments without claiming a
        // target build. The owned Vec payload is three four-byte words.
        for wide_alignment in [4, 8] {
            let wide = std::alloc::Layout::from_size_align(8, wide_alignment).unwrap();
            let vector = std::alloc::Layout::from_size_align(12, 4).unwrap();
            let model = wide.extend(vector).unwrap().0.pad_to_align();
            let tag = std::alloc::Layout::from_size_align(4, 4).unwrap();
            let payload = std::alloc::Layout::from_size_align(12, wide_alignment)
                .unwrap()
                .pad_to_align();
            let actual_bound = tag.extend(payload).unwrap().0.pad_to_align();
            assert!(model.size() >= actual_bound.size());
            assert!(model.align() >= actual_bound.align());
        }
    }
}
