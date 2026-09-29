//! Handing a chosen result to the daemon over its native socket, for a host
//! search whose activation reaches another process of the same bundle.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::EntryId;

/// The `native` request the daemon answers by opening an entry.
pub const OPEN: &str = "open";

/// How long the daemon may take to answer.
const ANSWER: Duration = Duration::from_secs(5);

/// Ask the daemon serving `native_socket` to open `entry`, and wait for its
/// answer.
///
/// # Errors
///
/// Returns transport errors, or the daemon's refusal: the entry is unknown,
/// or no longer opens for the owner.
pub fn open(native_socket: &Path, entry: EntryId) -> io::Result<()> {
    let mut stream = UnixStream::connect(native_socket)?;
    stream.set_read_timeout(Some(ANSWER))?;
    stream.set_write_timeout(Some(ANSWER))?;
    stream.write_all(format!("{{\"native\":\"{OPEN}\",\"entry\":\"{entry}\"}}\n").as_bytes())?;
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
