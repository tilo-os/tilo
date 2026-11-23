//! A captured browser profile, as data only: hosts without the capture backend never
//! construct or read one, and no source file is opened.

/// One fixed file captured from a reviewed profile.
pub struct CapturedFile {
    name: &'static str,
    bytes: Vec<u8>,
}
impl CapturedFile {
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A stable bounded snapshot. It contains no live source paths or write grants.
pub struct Snapshot {
    files: Vec<CapturedFile>,
    fingerprint: [u8; 32],
}
impl Snapshot {
    #[must_use]
    pub fn files(&self) -> &[CapturedFile] {
        &self.files
    }
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes.len() as u64).sum()
    }
}
