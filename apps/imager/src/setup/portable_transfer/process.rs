//! Fixed GNU tar invocation; paths are pinned directory descriptors, not archive input.
use super::*;
use std::{
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone, Copy)]
enum Mode {
    Extract,
    Create { sparse: bool },
}
#[cfg(test)]
pub(super) enum StreamEvent {
    Spawned(u32),
    Read(usize),
    Written(usize),
}
#[cfg(test)]
type Observer = Box<dyn FnMut(StreamEvent)>;
#[cfg(test)]
thread_local! {
    static STREAM_OBSERVER: std::cell::RefCell<Option<Observer>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(super) struct StreamObserver;
#[cfg(test)]
impl StreamObserver {
    pub(super) fn install(observer: impl FnMut(StreamEvent) + 'static) -> Self {
        STREAM_OBSERVER.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Box::new(observer));
        });
        Self
    }
}
#[cfg(test)]
impl Drop for StreamObserver {
    fn drop(&mut self) {
        STREAM_OBSERVER.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
#[cfg(test)]
fn observe(event: StreamEvent) {
    STREAM_OBSERVER.with(|slot| {
        if let Some(observer) = slot.borrow_mut().as_mut() {
            observer(event);
        }
    });
}
fn command(root: &PinnedDirectory, mode: Mode, exclusions: &[&Path]) -> Command {
    let extract = matches!(mode, Mode::Extract);
    let mut cmd = Command::new("/usr/bin/tar");
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .process_group(0)
        .args([
            "--file=-",
            "--numeric-owner",
            "--xattrs",
            "--xattrs-include=user.*",
        ])
        .arg("--directory=.")
        .stdout(if extract {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stderr(Stdio::null());
    // The capture service exposes only its independently signed, sealed helper closure.
    // Never inherit a caller-controlled loader path into archive execution.
    #[cfg(feature = "capture-worker")]
    cmd.env("LD_LIBRARY_PATH", "/run/tilo/migration/tools/lib");
    let directory_fd = root.file.as_raw_fd();
    // SAFETY: fchdir is async-signal-safe. The pinned descriptor remains alive
    // across spawn and in the forked child until exec; no ambient source path
    // or access to the non-dumpable parent's /proc descriptors is needed.
    unsafe {
        cmd.pre_exec(move || {
            if libc::fchdir(directory_fd) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    if extract {
        cmd.args([
            "--extract",
            "--acls",
            "--no-same-owner",
            "--same-permissions",
            "--keep-old-files",
            "--delay-directory-restore",
        ])
        .stdin(Stdio::piped());
    } else {
        cmd.args([
            "--create",
            "--format=pax",
            "--acls",
            "--atime-preserve=system",
        ])
        .stdin(Stdio::null());
        if matches!(mode, Mode::Create { sparse: true }) {
            cmd.args(["--sparse", "--sparse-version=0.1", "--hole-detection=seek"]);
        }
        for exclusion in exclusions {
            let mut literal = std::ffi::OsString::from("--exclude=./");
            literal.push(exclusion);
            cmd.args(["--anchored", "--no-wildcards"]).arg(literal);
        }
        cmd.arg(".");
    }
    cmd
}
struct Flow<T> {
    inner: T,
    progress: std::sync::Arc<Mutex<Instant>>,
}
impl<T: Read> Read for Flow<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(bytes)?;
        #[cfg(test)]
        observe(StreamEvent::Read(count));
        if count != 0 {
            *self
                .progress
                .lock()
                .map_err(|_| changed("Archive progress unavailable"))? = Instant::now();
        }
        Ok(count)
    }
}
impl<T: Write> Write for Flow<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        #[cfg(test)]
        observe(StreamEvent::Written(count));
        if count != 0 {
            *self
                .progress
                .lock()
                .map_err(|_| changed("Archive progress unavailable"))? = Instant::now();
        }
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn execute(
    root: &PinnedDirectory,
    mode: Mode,
    exclusions: &[&Path],
    cancel: &AtomicBool,
    stream: impl FnOnce(&mut std::process::Child, std::sync::Arc<Mutex<Instant>>) -> io::Result<()>,
) -> io::Result<()> {
    execute_with_idle_limit(
        root,
        mode,
        exclusions,
        cancel,
        Duration::from_secs(3600),
        stream,
    )
}
fn execute_with_idle_limit(
    root: &PinnedDirectory,
    mode: Mode,
    exclusions: &[&Path],
    cancel: &AtomicBool,
    idle_limit: Duration,
    stream: impl FnOnce(&mut std::process::Child, std::sync::Arc<Mutex<Instant>>) -> io::Result<()>,
) -> io::Result<()> {
    check_cancel(cancel)?;
    let mut child = command(root, mode, exclusions).spawn()?;
    let pid = child.id();
    #[cfg(test)]
    observe(StreamEvent::Spawned(pid));
    let done = AtomicBool::new(false);
    let progress = std::sync::Arc::new(Mutex::new(Instant::now()));
    std::thread::scope(|scope| {
        let watchdog = scope.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                let stalled = progress
                    .lock()
                    .map_or(true, |updated| updated.elapsed() > idle_limit);
                if cancel.load(Ordering::Relaxed) || stalled {
                    // SAFETY: child was placed in a new process group before exec.
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let result = stream(&mut child, progress.clone());
        if result.is_err() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let status = child.wait();
        done.store(true, Ordering::Relaxed);
        let _ = watchdog.join();
        result?;
        check_cancel(cancel)?;
        if !status?.success() {
            return Err(changed(
                "Portable archive transfer failed; source permissions or metadata may be unsupported",
            ));
        }
        Ok(())
    })
}
pub(super) fn create(
    root: &PinnedDirectory,
    inventory: &SourceInventory,
    writer: &mut impl Write,
    cancel: &AtomicBool,
    sparse: bool,
) -> io::Result<()> {
    execute(
        root,
        Mode::Create { sparse },
        &inventory.excluded_paths(),
        cancel,
        |child, progress| {
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| changed("Archive output missing"))?;
            io::copy(
                &mut Flow {
                    inner: stdout,
                    progress,
                },
                writer,
            )?;
            Ok(())
        },
    )
}
pub(super) fn extract(
    root: &PinnedDirectory,
    reader: &mut impl Read,
    cancel: &AtomicBool,
) -> io::Result<()> {
    execute(root, Mode::Extract, &[], cancel, |child, progress| {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| changed("Archive input missing"))?;
        let mut input = Flow {
            inner: stdin,
            progress,
        };
        io::copy(reader, &mut input)?;
        drop(input);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct SlowWriter(Vec<u8>);
    impl Write for SlowWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            std::thread::sleep(Duration::from_millis(5));
            let count = bytes.len().min(1024);
            self.0.extend_from_slice(
                bytes
                    .get(..count)
                    .ok_or_else(|| changed("Fixture stream missing"))?,
            );
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stalled_actual_tar_is_terminated_without_success() {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700)).unwrap();
        let contents = vec![37u8; 4 * 1024 * 1024];
        std::fs::write(source.join("document"), &contents).unwrap();
        let pin = PinnedDirectory::open_absolute(&source).unwrap();
        let mut partial = Vec::new();
        let result = execute_with_idle_limit(
            &pin,
            Mode::Create { sparse: false },
            &[],
            &AtomicBool::new(false),
            Duration::from_millis(100),
            |child, progress| {
                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| changed("Archive output missing"))?;
                let mut output = Flow {
                    inner: stdout,
                    progress,
                };
                let mut first = [0u8; 1024];
                output.read_exact(&mut first)?;
                partial.extend_from_slice(&first);
                // Backpressure leaves the actual GNU tar child blocked in its
                // pipe. The real watchdog must terminate this stalled stream.
                std::thread::sleep(Duration::from_millis(400));
                io::copy(&mut output, &mut partial)?;
                Ok(())
            },
        );
        assert!(
            result.is_err(),
            "stalled child must not report a complete archive"
        );
        assert!(partial.len() < contents.len());
        assert_eq!(std::fs::read(source.join("document")).unwrap(), contents);
    }

    #[test]
    fn actual_tar_progress_outlives_idle_bound_without_losing_contents() {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700)).unwrap();
        let contents = vec![37u8; 256 * 1024];
        std::fs::write(source.join("document"), &contents).unwrap();
        let pin = PinnedDirectory::open_absolute(&source).unwrap();
        let idle = Duration::from_millis(250);
        let began = Instant::now();
        let mut archive = SlowWriter(Vec::new());
        execute_with_idle_limit(
            &pin,
            Mode::Create { sparse: false },
            &[],
            &AtomicBool::new(false),
            idle,
            |child, progress| {
                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| changed("Archive output missing"))?;
                io::copy(
                    &mut Flow {
                        inner: stdout,
                        progress,
                    },
                    &mut archive,
                )?;
                Ok(())
            },
        )
        .unwrap();
        assert!(
            began.elapsed() > idle,
            "actual export must exceed the old total-duration bound"
        );
        let target = scratch.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let target = PinnedDirectory::open_absolute(&target).unwrap();
        extract(
            &target,
            &mut io::Cursor::new(archive.0),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(scratch.path().join("target/document")).unwrap(),
            contents
        );
        assert_eq!(std::fs::read(source.join("document")).unwrap(), contents);
    }
}
