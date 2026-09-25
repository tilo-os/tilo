//! Allocation-free token sizing before constructing the pinned JSON decoder.
use super::super::ResourceError;

pub(super) fn bytes(input: &[u8]) -> Result<usize, ResourceError> {
    // Match the existing parser's recursion ceiling; schema traversal also
    // bounds collection work. This scan is not a second JSON grammar.
    let mut depth = 0usize;
    let mut longest_string = 0usize;
    let mut longest_number = 0usize;
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < input.len() {
                    match input[i] {
                        b'"' => {
                            i += 1;
                            break;
                        }
                        b'\\' => i = i.saturating_add(2),
                        _ => i += 1,
                    }
                }
                longest_string = longest_string.max(i.saturating_sub(start));
            }
            b'[' | b'{' => {
                depth += 1;
                if depth > 128 {
                    return Err(ResourceError::Capacity);
                }
                i += 1
            }
            b']' | b'}' => {
                depth = depth.saturating_sub(1);
                i += 1
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < input.len()
                    && matches!(input[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    i += 1;
                }
                longest_number = longest_number.max(i - start);
            }
            _ => i += 1,
        }
    }
    // SliceRead's escaped scratch can reserve4 beyond source length; numeric
    // scratch starts at16. Three requested sizes include old/new growth overlap.
    let string = longest_string
        .checked_add(4)
        .and_then(|n| n.max(8).checked_mul(3));
    let number = longest_number
        .checked_add(1)
        .and_then(|n| n.max(16).checked_mul(3));
    let diagnostic = longest_string
        .max(longest_number)
        .checked_mul(10)
        .and_then(|n| n.checked_add(128))
        .and_then(|n| n.checked_mul(3));
    let signature = 3 * ("expected 64-byte signature, got ".len() + usize::BITS as usize);
    // Full finite-number range, not governance's exp64/digit20 policy.
    // Pinned serde_json lexical moderate powers are -350..309; outside that
    // range the moderate path returns zero/inf before bhcomp. bhcomp retains
    // <=769 significant digits. A short mantissa has<=20 decimal digits.
    // Conservative pow5/shift span=350+769+20; 4 bits/decimal digit and3
    // bits/power5 plus1075 binary exponent/rounding and128 carry bits bound
    // each result to277 32-bit limbs. Account64-bit limb storage too.
    const SPAN: usize = 350 + 769 + 20;
    const LIMBS: usize = (4 * 769 + 3 * SPAN + (1075 + SPAN) + 128).div_ceil(32);
    // Karatsuba halves the larger operand until cutoff32; even its sum carry
    // cannot require more than ceil(log2(LIMBS))+1 frames. Each frame can hold
    // sumx,sumy,z0,z1,z2,result/growth plus the long-multiply z/zi pair.
    // Eight vectors/frame,2 top-level Bigints,2*LIMBS result slots and3x
    // geometric old/new overlap cover that pinned recursive workspace.
    let frames = (usize::BITS - (LIMBS - 1).leading_zeros()) as usize + 1;
    let floating = (8 * frames + 2) * 3 * (2 * LIMBS) * 8;
    string
        .and_then(|n| n.checked_add(number?))
        .and_then(|n| n.checked_add(diagnostic?))
        .and_then(|n| n.checked_add(signature))
        .and_then(|n| n.checked_add(floating))
        .and_then(|n| n.checked_add(super::error_bytes()))
        .ok_or(ResourceError::Capacity)
}
