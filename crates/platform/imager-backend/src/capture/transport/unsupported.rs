//! Portable transport surface. Native descriptor channels cannot be constructed
//! here; operations report the same typed unsupported error as capture admission.
use crate::capture::{original_caller::OriginalCallerFacts, process::OriginalCaller};
use std::{fs::File, io};

pub const CHANNEL_FD: i32 = 70;

pub struct HostChannel {
    never: std::convert::Infallible,
}
pub struct WorkerChannel {
    never: std::convert::Infallible,
}
pub struct RetainedHomeInput {
    never: std::convert::Infallible,
}

fn unsupported<T>() -> io::Result<T> {
    Err(crate::capture::Error::Unsupported.into())
}

impl HostChannel {
    pub fn original_peer(&self) -> OriginalCaller {
        match self.never {}
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn original_caller_facts(&self) -> io::Result<OriginalCallerFacts> {
        unsupported()
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn validate_peer(&self) -> io::Result<()> {
        unsupported()
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn send(&mut self, _tag: u8, _file: &File) -> io::Result<()> {
        unsupported()
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn send_inventory_scratch(&mut self, _file: &File, _quota: u64) -> io::Result<()> {
        unsupported()
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn authenticate_as_root(&mut self) -> io::Result<()> {
        unsupported()
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn receive_mounted_input(&mut self, _tag: u8, _directory: bool) -> io::Result<File> {
        unsupported()
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn receive_retained_home(&mut self) -> io::Result<RetainedHomeInput> {
        unsupported()
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn receive(&mut self, _tag: u8) -> io::Result<File> {
        unsupported()
    }
    #[cfg(unix)]
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn authenticate(
        _stream: std::os::unix::net::UnixStream,
        _child_pid: u32,
        _uid: u32,
    ) -> io::Result<Self> {
        unsupported()
    }
    #[cfg(unix)]
    pub fn stream(&mut self) -> &mut std::os::unix::net::UnixStream {
        match self.never {}
    }
}

impl WorkerChannel {
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn authenticate_root(&mut self, _expected_pid: u32) -> io::Result<()> {
        unsupported()
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn send(&mut self, _tag: u8, _file: &File) -> io::Result<()> {
        unsupported()
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn receive(&mut self, _tag: u8, _directory: bool) -> io::Result<File> {
        unsupported()
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn receive_inventory_scratch(&mut self) -> io::Result<(File, u64)> {
        unsupported()
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn validate_root(&self) -> io::Result<()> {
        unsupported()
    }
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn receive_read_only_media(&mut self) -> io::Result<File> {
        unsupported()
    }
    #[cfg(unix)]
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn authenticate(_stream: std::os::unix::net::UnixStream) -> io::Result<Self> {
        unsupported()
    }
    #[cfg(unix)]
    pub fn stream(&mut self) -> &mut std::os::unix::net::UnixStream {
        match self.never {}
    }
}

impl RetainedHomeInput {
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn validate(&self, _channel: &HostChannel, _named: &File) -> io::Result<()> {
        unsupported()
    }
}

/// # Errors
/// Always returns the capture unsupported-platform error.
pub fn validate_read_only_block_device(_file: &File) -> io::Result<()> {
    unsupported()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_descriptor_validation_is_typed_unsupported() {
        let file = tempfile::tempfile().unwrap();
        let error = validate_read_only_block_device(&file).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(matches!(
            error
                .get_ref()
                .unwrap()
                .downcast_ref::<crate::capture::Error>(),
            Some(crate::capture::Error::Unsupported)
        ));
    }
}
