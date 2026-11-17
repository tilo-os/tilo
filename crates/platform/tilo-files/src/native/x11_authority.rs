//! An X authority file: the cookie that lets a client onto one X display, in the format X
//! servers and Xlib read (`-auth`, `XAUTHORITY`).

use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

/// The only authorization protocol written.
pub const PROTOCOL: &[u8] = b"MIT-MAGIC-COOKIE-1";

/// Write a new authority file at `path`, readable by this user alone, granting `cookie` on the
/// display `:display` of any host. Fails when `path` exists.
///
/// # Errors
/// When the file cannot be created or written.
pub fn write(path: &Path, display: u32, cookie: &[u8; 16]) -> io::Result<()> {
    let mut contents = Vec::with_capacity(64);
    // FamilyWild: whichever address the client connects by.
    contents.extend_from_slice(&u16::MAX.to_be_bytes());
    for field in [
        &[][..],
        display.to_string().as_bytes(),
        PROTOCOL,
        &cookie[..],
    ] {
        let len = u16::try_from(field.len()).map_err(io::Error::other)?;
        contents.extend_from_slice(&len.to_be_bytes());
        contents.extend_from_slice(field);
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&contents)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_file_names_the_display_the_protocol_and_the_cookie() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("Xauthority");
        super::write(&path, 12, &[7; 16]).expect("written");
        let bytes = std::fs::read(&path).expect("read");
        let mut expected = vec![0xff, 0xff, 0, 0, 0, 2, b'1', b'2', 0, 18];
        expected.extend_from_slice(super::PROTOCOL);
        expected.extend_from_slice(&[0, 16]);
        expected.extend_from_slice(&[7; 16]);
        assert_eq!(bytes, expected);
        assert!(
            super::write(&path, 12, &[7; 16]).is_err(),
            "never over another"
        );
    }
}
