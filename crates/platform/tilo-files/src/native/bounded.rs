//! Bounded reads for host-owned path contracts and readers: never allocate an unbounded input.
//! Reads at most `max_bytes`, returning [`std::io::ErrorKind::NotFound`] for a missing file and
//! [`std::io::ErrorKind::FileTooLarge`] when the input exceeds the bound, never returning the bytes
//! of an input over the bound.

pub fn read(path: &std::path::Path, max_bytes: impl TryInto<u64>) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    read_from(file, max_bytes)
}

fn invalid_bound() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "file byte bound is invalid",
    )
}

/// Reads at most `max_bytes` from `reader`, returning [`std::io::ErrorKind::FileTooLarge`] when it
/// yields more. Never reads more than one byte past `max_bytes`, so a growing or unbounded source
/// is refused without loading it.
pub fn read_from(
    reader: impl std::io::Read,
    max_bytes: impl TryInto<u64>,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let max_bytes: u64 = max_bytes.try_into().map_err(|_| invalid_bound())?;
    let bound = max_bytes.saturating_add(1);
    let mut bytes = Vec::new();
    reader.take(bound).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "file exceeds its byte bound",
        ));
    }
    Ok(bytes)
}

/// [`read`], reporting a file over the bound as the caller's `too_large` error and other failures
/// (including a missing file) through its own.
pub fn read_or<E: From<std::io::Error>>(
    path: &std::path::Path,
    max_bytes: impl TryInto<u64>,
    too_large: E,
) -> Result<Vec<u8>, E> {
    read(path, max_bytes).map_err(|error| {
        if error.kind() == std::io::ErrorKind::FileTooLarge {
            too_large
        } else {
            error.into()
        }
    })
}
