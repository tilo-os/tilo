//! Participant-private native editing under the existing admitted guest session.
//! Kernel facts qualify the original producer; they grant no app/actor authority.
use getrandom::rand_core::TryRng as _;
use runtime_portal::{editor, process_binding};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const ASSET: &str = "usr/lib/tilo/ush/bash-editor.so";
pub(crate) const GUEST_DIR: &str = "/run/ush-editor";
const EXECUTABLE_LIMIT: u64 = 16 * 1024 * 1024;

/// Trusted launch material only. Never populated from an app-supplied path.
#[derive(Clone, Debug)]
pub struct GuestEditor {
    pub producer: Arc<editor::Socket>,
    pub module: PathBuf,
    /// What the shell's rc file runs before any of the person's files
    /// ([`crate::shell_start::bash_rc`]): loading the producer, or closing
    /// its descriptor when that fails.
    pub prelude: String,
}

pub(crate) struct Prepared {
    pub guest: GuestEditor,
    socket: Arc<editor::Socket>,
    qualified_bash: Vec<u8>,
}

fn bounded_binary(path: &Path) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    if size == 0 || size > EXECUTABLE_LIMIT {
        return Err(io::Error::other(
            "editor executable exceeds qualification bound",
        ));
    }
    let mut bytes = Vec::new();
    file.take(EXECUTABLE_LIMIT.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).ok() != Some(size) {
        return Err(io::Error::other(
            "editor executable changed during qualification",
        ));
    }
    Ok(bytes)
}

fn trusted_asset(path: &Path) -> bool {
    path.is_absolute()
        && path.ancestors().enumerate().all(|(index, ancestor)| {
            fs::symlink_metadata(ancestor).is_ok_and(|metadata| {
                (if index == 0 {
                    metadata.is_file()
                } else {
                    metadata.is_dir()
                }) && metadata.uid() == 0
                    && metadata.permissions().mode() & 0o022 == 0
            })
        })
}

impl Prepared {
    /// Absence or a different selected shell leaves ordinary Terminal unchanged.
    pub(crate) fn prepare(plan: &crate::guest::GuestPlan) -> io::Result<Option<Self>> {
        let module = plan.host_root.join(ASSET);
        let host_bash = plan.host_root.join("usr/bin/bash");
        if plan.shell != Path::new("/usr/bin/bash")
            || !trusted_asset(&module)
            || !trusted_asset(&host_bash)
        {
            return Ok(None);
        }
        // Read the bounded module before launch; a trusted path alone does not
        // qualify an unbounded or concurrently changing artifact.
        let _ = bounded_binary(&module)?;
        let qualified_bash = bounded_binary(&host_bash)?;
        let Some(guest_bash) = plan
            .images
            .iter()
            .map(|image| image.join("usr/bin/bash"))
            .find(|path| path.exists())
        else {
            return Ok(None);
        };
        // Different runtime builds require their own reviewed producer, not a guess.
        if bounded_binary(&guest_bash)? != qualified_bash {
            return Ok(None);
        }
        let (socket, producer) = editor::Socket::pair()?;
        process_binding::arm(&socket)?;
        socket.read_timeout(Some(Duration::from_secs(3)))?;
        let fd = producer.as_fd().as_raw_fd();
        // Initialization and CLOEXEC happen before any user's shell configuration.
        // Unsupported loading closes the capability before ordinary startup resumes.
        let prelude = format!(
            "if enable -f {GUEST_DIR}/producer.so tilo_editable; then\n  tilo_editable {fd} || exec {fd}<&-\nelse\n  exec {fd}<&-\nfi\n"
        );
        Ok(Some(Self {
            guest: GuestEditor {
                producer: Arc::new(producer),
                module,
                prelude,
            },
            socket: Arc::new(socket),
            qualified_bash,
        }))
    }

    /// Start after the owned guest launch exists, before exposing any field data.
    pub(crate) fn start(self, app: Arc<editor::Socket>, launch: u32) -> io::Result<Worker> {
        let start = launch_start(launch)?;
        let stop = Arc::clone(&self.socket);
        let worker = std::thread::Builder::new()
            .name("ush-native-editor".into())
            .spawn(move || {
                let socket = self.socket;
                let run = || -> io::Result<()> {
                    let mut challenge = [0; 32];
                    getrandom::SysRng
                        .try_fill_bytes(&mut challenge)
                        .map_err(io::Error::other)?;
                    socket.send(&process_binding::MAGIC)?;
                    socket.send(&challenge)?;
                    let peer = process_binding::receive(
                        &*socket,
                        &challenge,
                        rustix::process::geteuid().as_raw(),
                    )?;
                    if !peer.belongs_to(launch, start)
                        || bounded_binary(Path::new(&format!("/proc/{}/exe", peer.pid())))?
                            != self.qualified_bash
                    {
                        return Err(io::Error::other(
                            "native editor does not belong to this qualified launch",
                        ));
                    }
                    socket.send(&[1])?;
                    socket.read_timeout(None)?;
                    proxy(&socket, &app, &peer)
                };
                let _ = run(); // no private text or invocation payload in audit/logs
                socket.retire();
                app.retire();
            })?;
        Ok(Worker {
            stop,
            worker: Some(worker),
        })
    }
}

fn launch_start(pid: u32) -> io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    stat.rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .and_then(|start| start.parse().ok())
        .ok_or_else(|| io::Error::other("original editor launch is unavailable"))
}

fn proxy(
    socket: &editor::Socket,
    app: &editor::Socket,
    peer: &process_binding::ExecutingPeer,
) -> io::Result<()> {
    let mut bytes = [0; editor::FRAME_BYTES];
    loop {
        let mut descriptors = [
            libc::pollfd {
                fd: socket.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: app.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: peer.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: three initialized descriptors remain owned throughout the wait.
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), 3, -1) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let [producer, client, process] = descriptors;
        if process.revents != 0
            || (producer.revents | client.revents)
                & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)
                != 0
        {
            return Ok(());
        }
        if producer.revents & libc::POLLIN != 0 {
            let size = peer.receive_packet(socket, &mut bytes)?;
            let packet = bytes
                .get(..size)
                .ok_or_else(|| io::Error::other("native editor packet bound"))?;
            editor::decode(packet)?; // validates protected replies before crossing namespaces
            app.send(packet)?;
        }
        if client.revents & libc::POLLIN != 0 {
            let size = app.receive(&mut bytes)?;
            if size < editor::HEADER_BYTES {
                return Err(io::Error::other("invalid native editor request"));
            }
            socket.send(
                bytes
                    .get(..size)
                    .ok_or_else(|| io::Error::other("native editor packet bound"))?,
            )?;
        }
    }
}

pub(crate) struct Worker {
    stop: Arc<editor::Socket>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.retire();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
