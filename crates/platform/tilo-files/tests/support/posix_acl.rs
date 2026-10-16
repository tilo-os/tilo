//! `setfacl -m` for the native Linux ACL tests, written straight to the
//! `system.posix_acl_{access,default}` extended attributes.
//!
//! CI's builder image does not ship the `acl` tools, and the kernel interface
//! is what the production checks observe anyway. Only numeric qualifiers are
//! accepted. As with `setfacl -m`, entries replace those with the same tag and
//! qualifier, a default ACL starts from the access ACL's base entries, and the
//! mask is recalculated unless the specification sets it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    reason = "test fixture: a malformed specification or refused syscall fails the test"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const USER_OBJ: u16 = 0x01;
const USER: u16 = 0x02;
const GROUP_OBJ: u16 = 0x04;
const GROUP: u16 = 0x08;
const MASK: u16 = 0x10;
const OTHER: u16 = 0x20;
const UNDEFINED_ID: u32 = u32::MAX;
const ACCESS: &str = "system.posix_acl_access";
const DEFAULT: &str = "system.posix_acl_default";

type Entry = (u16, u32, u16);

/// Apply a `setfacl -m` specification such as `u:65534:rwx,m::---` or
/// `d:u::rwx,d:g::rwx,d:o::rwx` to `path`.
pub fn modify(path: &Path, spec: &str) {
    let mut access = Vec::new();
    let mut default = Vec::new();
    for item in spec.split(',') {
        let (is_default, item) = item
            .strip_prefix("d:")
            .or_else(|| item.strip_prefix("default:"))
            .map_or((false, item), |rest| (true, rest));
        let mut fields = item.split(':');
        let (Some(tag), Some(qualifier), Some(perms), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            panic!("unsupported ACL entry {item:?}");
        };
        let named = !qualifier.is_empty();
        let tag = match (tag, named) {
            ("u" | "user", false) => USER_OBJ,
            ("u" | "user", true) => USER,
            ("g" | "group", false) => GROUP_OBJ,
            ("g" | "group", true) => GROUP,
            ("m" | "mask", false) => MASK,
            ("o" | "other", false) => OTHER,
            _ => panic!("unsupported ACL entry {item:?}"),
        };
        let id = if named {
            qualifier.parse().expect("numeric ACL qualifier")
        } else {
            UNDEFINED_ID
        };
        let entry = (tag, id, permissions(perms));
        if is_default {
            default.push(entry);
        } else {
            access.push(entry);
        }
    }
    let current = read(path, ACCESS).unwrap_or_else(|| {
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        let bits = |shift: u32| u16::try_from((mode >> shift) & 0o7).unwrap();
        vec![
            (USER_OBJ, UNDEFINED_ID, bits(6)),
            (GROUP_OBJ, UNDEFINED_ID, bits(3)),
            (OTHER, UNDEFINED_ID, bits(0)),
        ]
    });
    if !default.is_empty() {
        let base = read(path, DEFAULT).unwrap_or_else(|| {
            current
                .iter()
                .copied()
                .filter(|(tag, _, _)| matches!(*tag, USER_OBJ | GROUP_OBJ | OTHER))
                .collect()
        });
        write(path, DEFAULT, merge(base, &default));
    }
    if !access.is_empty() {
        write(path, ACCESS, merge(current, &access));
    }
}

fn permissions(text: &str) -> u16 {
    let bytes = text.as_bytes();
    assert_eq!(bytes.len(), 3, "ACL permissions {text:?}");
    [(b'r', 4), (b'w', 2), (b'x', 1)]
        .iter()
        .zip(bytes)
        .map(|(&(set, bit), &actual)| match actual {
            b'-' => 0,
            _ if actual == set => bit,
            _ => panic!("ACL permissions {text:?}"),
        })
        .sum()
}

fn merge(mut acl: Vec<Entry>, changes: &[Entry]) -> Vec<Entry> {
    for &(tag, id, perm) in changes {
        acl.retain(|&(t, i, _)| (t, i) != (tag, id));
        acl.push((tag, id, perm));
    }
    let explicit_mask = changes.iter().any(|&(tag, _, _)| tag == MASK);
    let extended = acl.iter().any(|&(tag, _, _)| matches!(tag, USER | GROUP));
    if !explicit_mask && (extended || acl.iter().any(|&(tag, _, _)| tag == MASK)) {
        let union = acl
            .iter()
            .filter(|&&(tag, _, _)| matches!(tag, USER | GROUP_OBJ | GROUP))
            .fold(0, |bits, &(_, _, perm)| bits | perm);
        acl.retain(|&(tag, _, _)| tag != MASK);
        acl.push((MASK, UNDEFINED_ID, union));
    }
    acl.sort_unstable_by_key(|&(tag, id, _)| (tag, id));
    acl
}

fn read(path: &Path, name: &str) -> Option<Vec<Entry>> {
    let mut buffer = vec![0_u8; 4096];
    let length = match rustix::fs::getxattr(path, name, buffer.as_mut_slice()) {
        Ok(length) => length,
        Err(rustix::io::Errno::NODATA) => return None,
        Err(error) => panic!("read {name} of {}: {error}", path.display()),
    };
    buffer.truncate(length);
    let (version, entries) = buffer.split_at_checked(4).expect("POSIX ACL header");
    assert_eq!(version, 2_u32.to_le_bytes(), "POSIX ACL version");
    Some(
        entries
            .chunks_exact(8)
            .map(|entry| {
                let &[t0, t1, p0, p1, i0, i1, i2, i3] = entry else {
                    unreachable!("chunks_exact yields 8 bytes")
                };
                (
                    u16::from_le_bytes([t0, t1]),
                    u32::from_le_bytes([i0, i1, i2, i3]),
                    u16::from_le_bytes([p0, p1]),
                )
            })
            .collect(),
    )
}

fn write(path: &Path, name: &str, acl: Vec<Entry>) {
    let mut bytes = 2_u32.to_le_bytes().to_vec();
    for (tag, id, perm) in acl {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&perm.to_le_bytes());
        bytes.extend_from_slice(&id.to_le_bytes());
    }
    rustix::fs::setxattr(path, name, &bytes, rustix::fs::XattrFlags::empty())
        .unwrap_or_else(|error| panic!("write {name} of {}: {error}", path.display()));
}
