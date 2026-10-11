//! Register only the actual private-control peer's original signed launch.
use super::{Frame, Path, UnixStream, Value, invalid, json, read_frame};
use std::io::{BufReader, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::sync::Arc;
use tilo_action_broker::services::LauncherServices;

pub(super) struct Caller {
    pid: u32,
    pidfd: OwnedFd,
}
impl Caller {
    pub(super) fn from_socket(socket: &UnixStream) -> Option<Self> {
        let mut credentials = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut size = std::mem::size_of::<libc::ucred>().try_into().ok()?;
        // SAFETY: initialized output buffers have the exact socket-option sizes.
        if unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut credentials).cast(),
                &raw mut size,
            )
        } != 0
            || usize::try_from(size).ok() != Some(std::mem::size_of::<libc::ucred>())
            || credentials.uid != unsafe { libc::geteuid() }
        {
            return None;
        }
        let pid = u32::try_from(credentials.pid).ok().filter(|pid| *pid > 0)?;
        let mut fd = -1;
        size = std::mem::size_of::<libc::c_int>().try_into().ok()?;
        // SAFETY: SO_PEERPIDFD creates one owned kernel handle for this socket's creator.
        if unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERPIDFD,
                (&raw mut fd).cast(),
                &raw mut size,
            )
        } != 0
            || fd < 0
        {
            return None;
        }
        // SAFETY: successful SO_PEERPIDFD transferred this new owned descriptor.
        let pidfd = unsafe { OwnedFd::from_raw_fd(fd) };
        if usize::try_from(size).ok() != Some(std::mem::size_of::<libc::c_int>()) {
            return None;
        }
        let caller = Self { pid, pidfd };
        caller.current().then_some(caller)
    }

    fn current(&self) -> bool {
        let mut poll = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: a single initialized pollfd references our retained kernel handle.
        unsafe { libc::poll(&raw mut poll, 1, 0) == 0 }
    }

    pub(super) fn answer(
        &self,
        launcher: Option<&Arc<dyn LauncherServices>>,
        request: &Value,
    ) -> Value {
        let answer = || -> Result<(), String> {
            if !self.current() {
                return Err("native registering process ended".into());
            }
            let app = request
                .get("app_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= 4096)
                .ok_or("native registration needs an application identity")?;
            let room = request
                .get("room")
                .and_then(Value::as_str)
                .filter(|value| uuid::Uuid::parse_str(value).is_ok())
                .ok_or("native registration needs an exact workspace")?;
            let executable = request
                .get("executable")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= 4096)
                .ok_or("native registration needs an executable source")?;
            launcher
                .ok_or("native source registration is unavailable")?
                .register_independent_launch(self.pid, app, room, Path::new(executable))?;
            if !self.current() {
                return Err("native registering process ended".into());
            }
            Ok(())
        };
        match answer() {
            Ok(()) => json!({"registered": true}),
            Err(error) => json!({"error": error}),
        }
    }
}

pub(super) fn register(
    profile: &Path,
    app: &str,
    room: &str,
    executable: &Path,
) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(profile.join("native.sock"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(10)))?;
    let executable = executable
        .to_str()
        .ok_or_else(|| invalid("native executable source path must be UTF-8"))?;
    let request =
        json!({"native": "register-launch", "app_id": app, "room": room, "executable": executable});
    let bytes = serde_json::to_vec(&request).map_err(std::io::Error::other)?;
    stream.write_all(&bytes)?;
    stream.write_all(b"\n")?;
    let Frame::Line(bytes) = read_frame(&mut BufReader::new(stream))? else {
        return Err(invalid(
            "native source registration returned no bounded reply",
        ));
    };
    let reply: Value = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    if reply.get("registered").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    Err(std::io::Error::other(
        reply
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("native source registration refused"),
    ))
}
