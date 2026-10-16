//! Bounded read-only host policy transport. Consumers validate the typed
//! binding and policy; this service never interprets a caller's owner claim.
use std::path::PathBuf;
use std::sync::Arc;

pub type AuthoritySource = Arc<dyn Fn() -> Result<Option<Vec<u8>>, String> + Send + Sync>;

/// Read the namespace the trusted process host selected. Native I/O stays in
/// the host service; the portable toolkit consumes only bounded bytes.
pub fn source(path: PathBuf, max_bytes: usize) -> AuthoritySource {
    #[cfg(feature = "native")]
    {
        Arc::new(move || read_bounded(&path, max_bytes))
    }
    #[cfg(not(feature = "native"))]
    {
        let _ = (path, max_bytes);
        Arc::new(|| Ok(None))
    }
}

#[cfg(feature = "native")]
fn read_bounded(path: &std::path::Path, max_bytes: usize) -> Result<Option<Vec<u8>>, String> {
    tilo_files::native::bounded::read(path, max_bytes).map_err(|error| match error.kind() {
        std::io::ErrorKind::FileTooLarge => "host action policy exceeds its byte bound".into(),
        std::io::ErrorKind::InvalidInput => "host action policy bound is invalid".into(),
        _ => "host action policy is unavailable".into(),
    })
}
