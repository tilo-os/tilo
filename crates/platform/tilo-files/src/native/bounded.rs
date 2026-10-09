//! Bounded reads for host-owned path contracts; never allocate an unbounded file.
/// Read at most `max_bytes`, returning `None` for a missing file.
///
/// # Errors
/// Returns an I/O error, an invalid bound or refusal when the file exceeds
/// the supplied bound. The oversized bytes are never returned.
pub fn read(path: &std::path::Path, max_bytes: usize) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read as _;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let bound = u64::try_from(max_bytes)
        .ok()
        .and_then(|bytes| bytes.checked_add(1))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "file byte bound is invalid",
            )
        })?;
    let mut bytes = Vec::new();
    file.take(bound).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds its byte bound",
        ));
    }
    Ok(Some(bytes))
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
