//! Exact compiler-input key. Kept dependency-free for focused production tests.

use alloc::vec::Vec;

pub(super) fn encode(flags: u8, parts: [&[u8]; 5]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.push(flags);
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
        bytes.extend_from_slice(part);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_compiler_input_changes_the_key() {
        let parts: [&[u8]; 5] = [
            b"float4 main() { return 1; }",
            b"main",
            b"cs",
            b"6_0",
            b"test.wgsl",
        ];
        let baseline = encode(0, parts);
        for flags in [1, 2, 3, 4] {
            assert_ne!(baseline, encode(flags, parts));
        }
        for index in 0..5 {
            let mut changed = parts;
            changed[index] = b"different compiler input";
            assert_ne!(baseline, encode(0, changed));
        }
        assert_ne!(
            encode(0, [b"ab", b"c", b"cs", b"6_0", b""]),
            encode(0, [b"a", b"bc", b"cs", b"6_0", b""])
        );
    }
}
