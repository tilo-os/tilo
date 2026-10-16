//! Explicit native qualification, never an installed editor or permission grant.
//! Pass a privately compiled `bash_editor.c` module and a new scratch directory.
//! Synthetic Bash runs without home, display, microphone or external network.

#[cfg(target_os = "linux")]
mod linux {
    use getrandom::rand_core::TryRng as _;
    use runtime_portal::{editor, process_binding};
    use std::fs::{self, File};
    use std::io::{self, Write};
    use std::os::fd::{AsFd, AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    struct Journey {
        child: Child,
        master: File,
        socket: editor::Socket,
        peer: process_binding::ExecutingPeer,
        producer_fd: i32,
        next: u64,
    }
    impl Drop for Journey {
        fn drop(&mut self) {
            self.socket.retire();
            // SAFETY: this owned child starts its own process group below.
            kill_owned(&mut self.child);
            let _ = self.child.wait();
        }
    }
    fn error(reason: &str) -> io::Error {
        io::Error::other(reason)
    }
    fn start_time(pid: u32) -> io::Result<u64> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
        stat.rsplit_once(") ")
            .and_then(|(_, tail)| tail.split_whitespace().nth(19))
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| error("cannot observe original launch"))
    }
    fn kill_owned(child: &mut Child) {
        if let Some(group) = i32::try_from(child.id()).ok().and_then(i32::checked_neg) {
            // SAFETY: only the process group of this owned launch is addressed.
            unsafe { libc::kill(group, libc::SIGKILL) };
        } else {
            let _ = child.kill();
        }
    }
    fn pty() -> io::Result<(File, File)> {
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: initialized descriptor out parameters, default dimensions.
        if unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful openpty gives unique ownership of both descriptors.
        Ok(unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) })
    }
    fn spawn(module: &Path, scratch: &Path) -> io::Result<Journey> {
        let module = module.canonicalize()?;
        if !module.is_file() || module.metadata()?.len() > 1024 * 1024 {
            return Err(error("missing or oversized qualified module"));
        }
        fs::create_dir(scratch)?;
        let scratch = scratch.canonicalize()?;
        let rc = scratch.join("synthetic-rc");
        let (socket, producer) = editor::Socket::pair()?;
        process_binding::arm(&socket)?;
        socket.read_timeout(Some(Duration::from_secs(3)))?;
        let fd = producer.as_fd().as_raw_fd();
        fs::write(
            &rc,
            format!(
                "enable -f /review/editor.so tilo_editable || exit 91\ntilo_editable {fd} || exit 92\nPS1='PROOF> '; PS2='CONT> '; HISTFILE=/dev/null\nset -o emacs\nbind 'set enable-active-region on'\nread -e -p 'STARTUP_PRIVATE> ' startupsecret\n"
            ),
        )?;
        let (master, slave) = pty()?;
        let mut command = Command::new("bwrap");
        command.args(["--unshare-all", "--die-with-parent", "--ro-bind", "/usr", "/usr", "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib64", "/lib64", "--symlink", "usr/bin", "/bin", "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--dir", "/review", "--ro-bind"])
            .arg(module).arg("/review/editor.so").arg("--ro-bind").arg(&rc).arg("/review/rc")
            .args(["--chdir", "/tmp", "--clearenv", "--setenv", "PATH", "/usr/bin:/bin", "--setenv", "HOME", "/tmp", "--setenv", "LANG", "C.UTF-8", "--setenv", "TERM", "xterm", "/usr/bin/python3", "-c", "import os,fcntl,termios; os.setsid(); fcntl.ioctl(0,termios.TIOCSCTTY,0); os.execv('/usr/bin/bash', ['bash','--noprofile','--rcfile','/review/rc','-i'])"])
            .stdin(Stdio::from(slave.try_clone()?)).stdout(Stdio::from(slave.try_clone()?)).stderr(Stdio::from(slave));
        // SAFETY: only async-signal-safe calls on the child's owned descriptor.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 || libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        drop(producer);
        let admit = || -> io::Result<process_binding::ExecutingPeer> {
            let start = start_time(child.id())?;
            let mut nonce = [0; 32];
            getrandom::SysRng
                .try_fill_bytes(&mut nonce)
                .map_err(io::Error::other)?;
            socket.send(&process_binding::MAGIC)?;
            socket.send(&nonce)?;
            let peer =
                process_binding::receive(&socket, &nonce, rustix::process::geteuid().as_raw())?;
            if !peer.belongs_to(child.id(), start)
                || fs::read(format!("/proc/{}/exe", peer.pid()))? != fs::read("/usr/bin/bash")?
            {
                return Err(error("producer is not this launch's qualified Bash"));
            }
            socket.send(&[1])?;
            Ok(peer)
        };
        let peer = match admit() {
            Ok(peer) => peer,
            Err(problem) => {
                // SAFETY: cleanup only this owned launch's process group.
                kill_owned(&mut child);
                let _ = child.wait();
                return Err(problem);
            }
        };
        Ok(Journey {
            child,
            master,
            socket,
            peer,
            producer_fd: fd,
            next: 1,
        })
    }
    impl Journey {
        fn receive(&self) -> io::Result<editor::Reply> {
            let mut frame = [0; editor::FRAME_BYTES];
            let size = self.peer.receive_packet(&self.socket, &mut frame)?;
            editor::decode(frame.get(..size).ok_or_else(|| error("packet length"))?)
        }
        fn call(&mut self, operation: &editor::Request) -> io::Result<editor::Reply> {
            let id = self.next;
            self.next = self
                .next
                .checked_add(1)
                .ok_or_else(|| error("request overflow"))?;
            self.socket.send(&editor::encode(id, operation)?)?;
            let deadline = Instant::now()
                .checked_add(Duration::from_secs(3))
                .ok_or_else(|| error("deadline overflow"))?;
            for _ in 0..128 {
                let reply = self.receive()?;
                if reply.request == id {
                    return Ok(reply);
                }
                if reply.request != 0 || Instant::now() >= deadline {
                    return Err(error("wrong or expired operation acknowledgement"));
                }
            }
            Err(error("field announcement bound"))
        }
        fn key(&mut self, text: &[u8]) -> io::Result<()> {
            self.master.write_all(text)?;
            // Bounded fixture scheduling only; production producer polls without timers.
            std::thread::sleep(Duration::from_millis(100));
            Ok(())
        }
        fn ready(&mut self, text: &str) -> io::Result<editor::Reply> {
            let reply = self.call(&editor::Request::Inspect)?;
            if reply.status != editor::Status::Ready || reply.text != text || reply.lease.is_none()
            {
                return Err(error("actual public buffer or lease did not match"));
            }
            Ok(reply)
        }
        fn mutate(
            &mut self,
            operation: &editor::Request,
            status: editor::Status,
        ) -> io::Result<()> {
            let reply = self.call(operation)?;
            if reply.status != status || reply.lease.is_some() || !reply.text.is_empty() {
                return Err(error("native operation outcome did not match"));
            }
            Ok(())
        }
    }
    fn startup_privacy(journey: &mut Journey) -> io::Result<()> {
        let unavailable = journey.call(&editor::Request::Inspect)?;
        if unavailable.status != editor::Status::Unavailable || !unavailable.text.is_empty() {
            return Err(error("startup read -e must expose no field"));
        }
        journey.key(b"synthetic startup secret\n")?;
        journey.ready("")?;
        println!("PASS startup protected input");
        Ok(())
    }
    fn selection_and_receipts(journey: &mut Journey) -> io::Result<()> {
        journey.key("café ☕".as_bytes())?;
        journey.key(b"\x02\x00\x01\x18\x18")?;
        let lease = journey
            .ready("café ☕")?
            .lease
            .ok_or_else(|| error("missing selection lease"))?;
        if lease.anchor != 0 || lease.focus != 6 {
            return Err(error("actual Unicode selection missing"));
        }
        let mut extended = lease.clone();
        extended.expires_ns = extended.expires_ns.saturating_add(1);
        journey.mutate(
            &editor::Request::Replace {
                lease: extended,
                transaction: 60,
                text: "wrong ".into(),
            },
            editor::Status::Stale,
        )?;
        let request = journey.next;
        let operation = editor::Request::Replace {
            lease: lease.clone(),
            transaction: 61,
            text: "salut ".into(),
        };
        journey.mutate(&operation, editor::Status::Applied)?;
        journey.ready("salut ☕")?;
        // Repeat the exact original request ID and bytes; observe its receipt, never reapply.
        journey.socket.send(&editor::encode(request, &operation)?)?;
        let repeated = journey.receive()?;
        if repeated.request != request || repeated.status != editor::Status::Applied {
            return Err(error("idempotent receipt missing"));
        }
        journey.ready("salut ☕")?;
        let queried = journey.call(&editor::Request::Query {
            lease: lease.clone(),
            transaction: request,
        })?;
        if queried.status != editor::Status::Applied || !queried.text.is_empty() {
            return Err(error("original private receipt query failed"));
        }
        let current = journey
            .ready("salut ☕")?
            .lease
            .ok_or_else(|| error("missing history lease"))?;
        journey.mutate(
            &editor::Request::Undo {
                lease: current,
                transaction: 61,
            },
            editor::Status::Undone,
        )?;
        let restored = journey
            .ready("café ☕")?
            .lease
            .ok_or_else(|| error("missing restored lease"))?;
        if restored.anchor != lease.anchor || restored.focus != lease.focus {
            return Err(error("native selection was not restored"));
        }
        journey.key(b"\x05\x15")?;
        journey.ready("")?;
        println!("PASS Unicode selection restoration, exact expiry and idempotent queried receipt");
        Ok(())
    }
    fn open_journey() -> io::Result<Journey> {
        let args: Vec<_> = std::env::args_os().collect();
        let [_, module, scratch] = args.as_slice() else {
            return Err(error("usage: bash_editor_journey MODULE NEW_SCRATCH"));
        };
        spawn(Path::new(module), Path::new(scratch))
    }
    pub fn run() -> io::Result<()> {
        let mut journey = open_journey()?;
        startup_privacy(&mut journey)?;
        selection_and_receipts(&mut journey)?;
        journey.key("alpha café".as_bytes())?;
        let initial = journey
            .ready("alpha café")?
            .lease
            .ok_or_else(|| error("missing editor lease"))?;
        journey.mutate(
            &editor::Request::Replace {
                lease: initial.clone(),
                transaction: 71,
                text: " 世界".into(),
            },
            editor::Status::Applied,
        )?;
        let applied = journey.ready("alpha café 世界")?;
        if !applied.can_undo || applied.can_redo {
            return Err(error("Undo checkpoint missing"));
        }
        println!("PASS literal Unicode insertion");
        journey.mutate(
            &editor::Request::Replace {
                lease: initial,
                transaction: 72,
                text: " stale".into(),
            },
            editor::Status::Stale,
        )?;
        let lease = applied.lease.ok_or_else(|| error("missing editor lease"))?;
        journey.mutate(
            &editor::Request::Undo {
                lease,
                transaction: 71,
            },
            editor::Status::Undone,
        )?;
        let undone = journey.ready("alpha café")?;
        if undone.can_undo || !undone.can_redo {
            return Err(error("Redo checkpoint missing"));
        }
        journey.mutate(
            &editor::Request::Redo {
                lease: undone.lease.ok_or_else(|| error("missing editor lease"))?,
                transaction: 71,
            },
            editor::Status::Redone,
        )?;
        let redone = journey.ready("alpha café 世界")?;
        journey.mutate(
            &editor::Request::Undo {
                lease: redone.lease.ok_or_else(|| error("missing editor lease"))?,
                transaction: 71,
            },
            editor::Status::Undone,
        )?;
        journey.ready("alpha café")?;
        println!("PASS native Undo Redo Undo and stale original lease");
        let original = journey
            .ready("alpha café")?
            .lease
            .ok_or_else(|| error("missing editor lease"))?;
        journey.key(b"\x02\x06")?;
        journey.mutate(
            &editor::Request::Replace {
                lease: original,
                transaction: 73,
                text: " wrong".into(),
            },
            editor::Status::Stale,
        )?;
        let original = journey
            .ready("alpha café")?
            .lease
            .ok_or_else(|| error("missing editor lease"))?;
        journey.mutate(&editor::Request::Revoke, editor::Status::Revoked)?;
        journey.mutate(
            &editor::Request::Replace {
                lease: original,
                transaction: 74,
                text: " wrong".into(),
            },
            editor::Status::Stale,
        )?;
        println!("PASS net-zero caret motion and cancellation revoke");
        journey.key(b"\x15read -e -p 'PRIVATE> ' private\n")?;
        let unavailable = journey.call(&editor::Request::Inspect)?;
        if unavailable.status != editor::Status::Unavailable || !unavailable.text.is_empty() {
            return Err(error("read -e privacy refusal"));
        }
        journey.key(b"synthetic private value\n")?;
        journey.ready("")?;
        println!("PASS protected prompt and repeated editor generation");
        journey.key(format!("/usr/bin/python3 -c 'import os; assert not os.path.exists(\"/proc/self/fd/{}\")' || exit 93\n", journey.producer_fd).as_bytes())?;
        journey.ready("")?;
        println!("PASS bounded native protocol journey");
        Ok(())
    }
}

fn main() -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    return linux::run();
    #[cfg(not(target_os = "linux"))]
    Err(std::io::Error::other(
        "native Bash qualification requires Linux",
    ))
}
