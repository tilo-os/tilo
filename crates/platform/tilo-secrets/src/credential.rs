//! How a secret is laid out in the Windows Credential Manager, kept apart from the
//! system calls so every target tests it.
//!
//! One generic credential holds at most [`BLOB_LIMIT`] bytes, so a secret is stored as
//! a first credential, `tilo:<app>/<key>`, whose blob names the format and the number
//! of further parts, then as many `tilo:<app>/<key>#<n>` as it needs.

pub(crate) const BLOB_LIMIT: usize = 5 * 512;

const FORMAT: u8 = 1;

const HEADER: usize = 3;

const FIRST_ROOM: usize = BLOB_LIMIT - HEADER;

/// The credential that holds part `part` of `key` of `app`; part 0 is the first one,
/// The credential that holds part `part` of `key` of `app`; part 0 is the first one,
/// which a person finds in Credential Manager.
pub(crate) fn target_name(app: &str, key: &str, part: usize) -> String {
    if part == 0 {
        format!("tilo:{app}/{key}")
    } else {
        format!("tilo:{app}/{key}#{part}")
    }
}

pub(crate) fn split(value: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let (first, rest) = value.split_at(value.len().min(FIRST_ROOM));
    let rest: Vec<Vec<u8>> = rest.chunks(BLOB_LIMIT).map(<[u8]>::to_vec).collect();
    let count = u16::try_from(rest.len())
        .map_err(|_| format!("a secret of {} bytes is too large to store", value.len()))?;
    let mut head = Vec::with_capacity(BLOB_LIMIT);
    head.push(FORMAT);
    head.extend_from_slice(&count.to_le_bytes());
    head.extend_from_slice(first);
    let mut parts = vec![head];
    parts.extend(rest);
    Ok(parts)
}

pub(crate) fn further_parts(first: &[u8]) -> Result<usize, String> {
    match first {
        [FORMAT, low, high, ..] => Ok(usize::from(u16::from_le_bytes([*low, *high]))),
        _ => Err("the stored credential is not a Tilo secret".to_owned()),
    }
}

pub(crate) fn join(first: &[u8], rest: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let expected = further_parts(first)?;
    if rest.len() != expected {
        return Err(format!(
            "the stored secret has {} of its {expected} further parts",
            rest.len()
        ));
    }
    let mut value = first.get(HEADER..).unwrap_or_default().to_vec();
    for part in rest {
        value.extend_from_slice(part);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_secret_is_one_credential() {
        let parts = split(b"\x00seed\xff").unwrap();
        assert_eq!(parts.len(), 1);
        assert!(parts[0].len() <= BLOB_LIMIT);
        assert_eq!(join(&parts[0], &[]).unwrap(), b"\x00seed\xff");
        assert_eq!(split(b"").unwrap(), [vec![FORMAT, 0, 0]]);
        assert_eq!(join(&split(b"").unwrap()[0], &[]).unwrap(), b"");
    }

    #[test]
    fn a_large_secret_spans_credentials_that_each_fit() {
        for len in [FIRST_ROOM, FIRST_ROOM + 1, BLOB_LIMIT * 3, 40_000] {
            let value: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
            let parts = split(&value).unwrap();
            assert!(parts.iter().all(|part| part.len() <= BLOB_LIMIT), "{len}");
            assert_eq!(further_parts(&parts[0]).unwrap(), parts.len() - 1);
            assert_eq!(join(&parts[0], &parts[1..]).unwrap(), value, "{len}");
        }
    }

    #[test]
    fn foreign_or_incomplete_credentials_are_refused() {
        assert!(join(b"plain password", &[]).is_err());
        assert!(join(&[], &[]).is_err());
        let parts = split(&vec![7; BLOB_LIMIT * 2]).unwrap();
        assert!(join(&parts[0], &parts[1..2]).is_err());
    }

    #[test]
    fn parts_are_named_under_the_app_and_key() {
        assert_eq!(target_name("so.tilo.x", "k", 0), "tilo:so.tilo.x/k");
        assert_eq!(target_name("so.tilo.x", "k", 2), "tilo:so.tilo.x/k#2");
    }
}
