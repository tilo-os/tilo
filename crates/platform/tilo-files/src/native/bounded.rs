//! Bounded reads for host-owned path contracts; never allocate an unbounded file.
/// Read at most `max_bytes`, returning `None` for a missing file.
///
/// # Errors
/// Returns an I/O error, an invalid bound or refusal when the file exceeds
/// the supplied bound. The oversized bytes are never returned.
pub fn read(
    path: &std::path::Path,
    max_bytes: impl TryInto<u64>,
) -> std::io::Result<Option<Vec<u8>>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let max_bytes: u64 = max_bytes.try_into().map_err(|_| invalid_bound())?;
    read_from(&file, max_bytes).map(Some)
}

fn invalid_bound() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "file byte bound is invalid",
    )
}

/// Read at most `max_bytes` of an open `file`, as [`read`] does.
pub(super) fn read_from(file: &std::fs::File, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let bound = max_bytes.checked_add(1).ok_or_else(invalid_bound)?;
    let mut bytes = Vec::new();
    file.take(bound).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "file exceeds its byte bound",
        ));
    }
    Ok(bytes)
}

/// [`read`], reporting a file over the bound as the caller's `too_large`
/// error and other failures through its `From<io::Error>`.
pub fn read_or<E: From<std::io::Error>>(
    path: &std::path::Path,
    max_bytes: impl TryInto<u64>,
    too_large: E,
) -> Result<Option<Vec<u8>>, E> {
    read(path, max_bytes).map_err(|error| {
        if error.kind() == std::io::ErrorKind::FileTooLarge {
            too_large
        } else {
            error.into()
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn action_authority_source_observes_atomic_replacement_deletion_and_byte_bound() {
        let unique = tilo_clock::try_unix_now_ns().unwrap();
        let root =
            std::env::temp_dir().join(format!("tilo-authority-{}-{unique}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("current.json");
        let read = || super::read(&path, 4);
        assert_eq!(read().unwrap(), None);
        std::fs::write(&path, b"old").unwrap();
        assert_eq!(read().unwrap(), Some(b"old".to_vec()));
        let next = root.join("next");
        std::fs::write(&next, b"new!").unwrap();
        std::fs::rename(&next, &path).unwrap();
        assert_eq!(read().unwrap(), Some(b"new!".to_vec()));
        std::fs::write(&path, b"secret").unwrap();
        let error = read().unwrap_err().to_string();
        assert_eq!(error, "file exceeds its byte bound");
        assert!(!error.contains("secret"));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(read().unwrap(), None);
        std::fs::create_dir(&path).unwrap();
        assert!(read().is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
