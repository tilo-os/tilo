//! Bounded read-only host policy transport. Consumers validate the typed
//! binding and policy; this service never interprets a caller's owner claim.
use std::path::PathBuf;
use std::sync::Arc;

pub type AuthoritySource = Arc<dyn Fn() -> Result<Option<Vec<u8>>, String> + Send + Sync>;

/// Read the namespace the trusted process host selected. Native I/O stays in
/// the host service; the portable toolkit consumes only bounded bytes. A
/// policy the host no longer holds (it exited, or could not replace it) is
/// read as absent: its admission ends with the host, without a lifetime.
pub fn source(path: PathBuf, max_bytes: usize) -> AuthoritySource {
    #[cfg(feature = "native")]
    {
        Arc::new(move || read_held(&path, max_bytes))
    }
    #[cfg(not(feature = "native"))]
    {
        let _ = (path, max_bytes);
        Arc::new(|| Ok(None))
    }
}

#[cfg(feature = "native")]
fn read_held(path: &std::path::Path, max_bytes: usize) -> Result<Option<Vec<u8>>, String> {
    tilo_files::native::held::read(path, max_bytes).map_err(|error| match error.kind() {
        std::io::ErrorKind::FileTooLarge => "host action policy exceeds its byte bound".into(),
        std::io::ErrorKind::InvalidInput => "host action policy bound is invalid".into(),
        _ => "host action policy is unavailable".into(),
    })
}

#[cfg(all(test, feature = "native"))]
mod tests {
    #[test]
    fn a_policy_is_read_only_while_the_host_holds_it() {
        let unique = tilo_clock::try_unix_now_ns().unwrap();
        let root = std::env::temp_dir().join(format!(
            "tilo-host-authority-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("current.json");
        let source = super::source(path.clone(), 64);
        assert_eq!(source().unwrap(), None);
        let held = tilo_files::native::held::publish(&path, b"policy", 0o600).unwrap();
        assert_eq!(source().unwrap().as_deref(), Some(&b"policy"[..]));
        // Nothing renews it: it stays current until the host lets go.
        assert_eq!(source().unwrap().as_deref(), Some(&b"policy"[..]));
        drop(held);
        assert_eq!(
            source().unwrap(),
            None,
            "a host that let go vouches for nothing"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
