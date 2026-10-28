//! Bounded read-only host policy transport. Consumers validate the typed
//! binding and policy; this service never interprets a caller's owner claim.
use std::path::{Path, PathBuf};
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

/// A watch on what a [`source`] reads; dropping it ends the calls.
pub type AuthorityWatch = Box<dyn std::any::Any + Send>;

/// Call `changed` whenever what [`source`] reads at `path` may have changed:
/// the host published another policy over it, removed it, or let go of it.
/// Letting go needs no help from the host: when its hold (the writable
/// descriptor the file was published through) closes, on exit or a crash
/// as much as on a withdrawal, the kernel reports that close.
///
/// # Errors
/// When the directory holding `path` cannot be watched.
pub fn watch(path: &Path, changed: impl Fn() + Send + 'static) -> Result<AuthorityWatch, String> {
    #[cfg(feature = "native")]
    {
        let directory = path.parent().ok_or("host action policy has no directory")?;
        let name = path.to_owned();
        tilo_files::native::watch_changes(directory, move |entry| entry == name, changed)
            .map(|watch| Box::new(watch) as AuthorityWatch)
            .map_err(|error| format!("host action policy cannot be watched: {error}"))
    }
    #[cfg(not(feature = "native"))]
    {
        let _ = (path, changed);
        Ok(Box::new(()))
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

    #[test]
    fn a_watch_hears_each_publication_and_the_host_letting_go() {
        use std::sync::mpsc;
        use std::time::Duration;
        let unique = tilo_clock::try_unix_now_ns().unwrap();
        let root = std::env::temp_dir().join(format!(
            "tilo-host-authority-watch-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("current.json");
        let (told, heard) = mpsc::channel();
        let _watch = super::watch(&path, move || {
            let _ = told.send(());
        })
        .unwrap();
        let source = super::source(path.clone(), 64);
        // A loaded test machine may take this long to report a change.
        let report = Duration::from_secs(10);
        let settle = |heard: &mpsc::Receiver<()>| {
            heard.recv_timeout(report).expect("the change was reported");
            while heard.recv_timeout(Duration::from_millis(100)).is_ok() {}
        };
        let first = tilo_files::native::held::publish(&path, b"first", 0o600).unwrap();
        settle(&heard);
        let second = tilo_files::native::held::publish(&path, b"second", 0o600).unwrap();
        drop(first);
        settle(&heard);
        assert_eq!(source().unwrap().as_deref(), Some(&b"second"[..]));
        // Reading the policy is no change.
        assert!(heard.recv_timeout(Duration::from_millis(300)).is_err());
        drop(second);
        settle(&heard);
        assert_eq!(source().unwrap(), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
