//! The runtime's side of the native socket this crate speaks.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// The `native` request the runtime answers by opening an entry.
pub const OPEN: &str = "open";

/// How long the runtime may take to answer.
const ANSWER: Duration = Duration::from_secs(5);

/// Ask the runtime serving `native_socket` to open `entry`, and wait for its
/// answer.
///
/// # Errors
///
/// Returns transport errors, or the runtime's refusal: the entry is
/// unknown, or no longer opens for the runtime's owner.
pub fn open(native_socket: &Path, entry: &str) -> io::Result<()> {
    let mut request = request_line(entry)?;
    request.push('\n');
    let mut stream = UnixStream::connect(native_socket)?;
    stream.set_read_timeout(Some(ANSWER))?;
    stream.set_write_timeout(Some(ANSWER))?;
    stream.write_all(request.as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream).take(4096).read_line(&mut reply)?;
    if reply.trim() == r#"{"opened":true}"# {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "the runtime did not open the result: {}",
            reply.trim()
        )))
    }
}

/// The request line for `entry`. Entries are the runtime's own opaque ids;
/// anything that would need escaping is refused rather than escaped.
fn request_line(entry: &str) -> io::Result<String> {
    if entry.is_empty() || entry.len() > 64 || !entry.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a host search entry",
        ));
    }
    Ok(format!(r#"{{"native":"{OPEN}","entry":"{entry}"}}"#))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_opaque_entry_reaches_the_socket() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("native.sock");
        for entry in ["", "a\"b", "../x", &"f".repeat(65)] {
            let refused = open(&socket, entry).unwrap_err();
            assert_eq!(refused.kind(), io::ErrorKind::InvalidInput, "{entry:?}");
        }
        assert_eq!(
            request_line("00112233aabbccdd").unwrap(),
            r#"{"native":"open","entry":"00112233aabbccdd"}"#
        );
    }
}
