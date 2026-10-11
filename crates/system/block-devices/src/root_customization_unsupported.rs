//! A setup machine without Unix ownership and links cannot give a written
//! disk's account its SSH login or home files (see the Unix
//! `installed_root`), nor label with the image's policy.

use std::io;
use std::path::Path;

/// The app chooses its staged request paths and validation policy.
pub struct Requests {
    pub ssh_key: &'static str,
    pub home_files: &'static str,
    pub validate_keys: fn(&str) -> io::Result<()>,
    pub valid_home_name: fn(&str) -> bool,
}

#[derive(Debug, Default)]
pub struct Placed;

impl Placed {
    pub fn before_copy(_staged: &Path, _etc: &Path, _var: &Path) -> io::Result<Self> {
        Ok(Self)
    }
}

pub fn finish(
    etc: &Path,
    _var: &Path,
    staged: &Path,
    _placed: Placed,
    requests: Requests,
) -> io::Result<()> {
    let request = etc.join(
        requests
            .ssh_key
            .strip_prefix("etc/")
            .unwrap_or(requests.ssh_key),
    );
    if request.exists() || staged.join(requests.home_files).exists() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "giving a written disk's account SSH keys or home files requires a Unix setup machine",
        ));
    }
    Ok(())
}
