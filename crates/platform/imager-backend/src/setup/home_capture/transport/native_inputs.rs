//! Real authenticated processes and SCM_RIGHTS; no synthetic ExecutingPeer.
use super::*;
use runtime_portal::{fd_transport, process_binding};
use std::os::fd::AsFd;
use std::{
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::UnixStream,
            process::CommandExt,
        },
    },
    path::Path,
    process::{Child, Command},
    time::Duration,
};
const SELECTOR: &str = "setup::home_capture::transport::native_inputs::mounted_input_fixture_child";
struct Owned(Child);
impl Owned {
    fn finish(&mut self) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            if std::time::Instant::now() >= deadline {
                self.0.kill().unwrap();
                self.0.wait().unwrap();
                assert!(
                    std::time::Instant::now() < deadline,
                    "Owned native input child timeout"
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        self.0.wait().unwrap();
    }
}
fn spawn(role: &str, stream: &UnixStream, control: &UnixStream, source: &Path, uid: u32) -> Owned {
    let executable = File::open(std::env::current_exe().unwrap()).unwrap();
    let descriptors = [
        executable.as_raw_fd(),
        stream.as_raw_fd(),
        control.as_raw_fd(),
    ];
    let mut command = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
    command
        .args(["--exact", SELECTOR, "--nocapture"])
        .env("TILO_MOUNTED_INPUT_ROLE", role)
        .env("TILO_MOUNTED_INPUT_FD", stream.as_raw_fd().to_string())
        .env(
            "TILO_MOUNTED_INPUT_CONTROL",
            control.as_raw_fd().to_string(),
        )
        .env("TILO_MOUNTED_INPUT_SOURCE", source);
    unsafe {
        command.pre_exec(move || {
            for fd in descriptors {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if uid != 0 {
                let groups = [1002];
                if libc::setgroups(groups.len(), groups.as_ptr()) != 0
                    || libc::setgid(1001) != 0
                    || libc::setuid(uid) != 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Owned(command.spawn().unwrap())
}
fn fixture() -> tempfile::TempDir {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "Native credential-drop fixture requires isolated root"
    );
    let temp = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o711))
        .tempdir()
        .unwrap();
    let file = temp.path().join("selected");
    std::fs::write(&file, b"selected original bytes").unwrap();
    let path = std::ffi::CString::new(file.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(path.as_ptr(), 1000, 1001) }, 0);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let directory = temp.path().join("selected-directory");
    std::fs::create_dir(&directory).unwrap();
    let named = std::ffi::CString::new(directory.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(named.as_ptr(), 1000, 1001) }, 0);
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    temp
}
#[test]
fn mounted_input_fixture_child() {
    let Ok(role) = std::env::var("TILO_MOUNTED_INPUT_ROLE") else {
        return;
    };
    let mut stream = unsafe {
        UnixStream::from_raw_fd(
            std::env::var("TILO_MOUNTED_INPUT_FD")
                .unwrap()
                .parse()
                .unwrap(),
        )
    };
    let mut control = unsafe {
        UnixStream::from_raw_fd(
            std::env::var("TILO_MOUNTED_INPUT_CONTROL")
                .unwrap()
                .parse()
                .unwrap(),
        )
    };
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    control
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let source = std::path::PathBuf::from(std::env::var_os("TILO_MOUNTED_INPUT_SOURCE").unwrap());
    if role == "late-broker" {
        assert_eq!(rustix::process::geteuid().as_raw(), 0);
        let mut pid = [0; 4];
        control.read_exact(&mut pid).unwrap();
        let mut channel = HostChannel::authenticate(stream, u32::from_ne_bytes(pid), 1000).unwrap();
        channel.authenticate_as_root().unwrap();
        let (bytes, descriptor) = fd_transport::recv_with_fd(channel.stream().as_fd()).unwrap();
        assert_eq!(bytes, [3]);
        let file = File::from(descriptor.unwrap());
        assert!(file.metadata().unwrap().is_file());
        channel.validate_peer().unwrap();
        control.write_all(b"R").unwrap();
        return; // Real admitted root process retires before any descriptor ACK.
    }
    assert_eq!(rustix::process::geteuid().as_raw(), 1000);
    assert_eq!(rustix::process::getegid().as_raw(), 1001);
    if role == "late-sender" {
        let mut pid = [0; 4];
        control.read_exact(&mut pid).unwrap();
        let mut channel = WorkerChannel::authenticate(stream).unwrap();
        channel.authenticate_root(u32::from_ne_bytes(pid)).unwrap();
        let file = File::open(&source).unwrap();
        assert!(
            channel.send(3, &file).is_err(),
            "ACK after actual root pidfd retirement must refuse"
        );
        control.write_all(b"F").unwrap();
        return;
    }
    process_binding::complete(&mut stream).unwrap();
    if role == "missing" {
        stream.write_all(&[3]).unwrap();
    } else {
        let file = if role == "kind" {
            File::open(source.with_file_name("selected-directory")).unwrap()
        } else {
            std::fs::OpenOptions::new()
                .read(true)
                .write(role == "writable")
                .custom_flags(libc::O_NOFOLLOW)
                .open(&source)
                .unwrap()
        };
        fd_transport::send_with_fd(
            stream.as_fd(),
            &[if role == "tag" { 2 } else { 3 }],
            file.as_fd(),
        )
        .unwrap();
    }
    control.write_all(b"S").unwrap();
    if role == "retired" {
        control.write_all(b"F").unwrap();
        return;
    }
    let mut ack = [0];
    assert_eq!(
        stream.read(&mut ack).unwrap(),
        0,
        "Rejected input must not receive ACK"
    );
    control.write_all(b"F").unwrap();
}
#[test]
fn actual_mounted_inputs_refuse_wrong_tag_writable_missing_kind_and_retired_peer() {
    let temp = fixture();
    let source = temp.path().join("selected");
    for role in ["tag", "writable", "missing", "kind", "retired"] {
        let (host, app) = UnixStream::pair().unwrap();
        let (mut control, child_control) = UnixStream::pair().unwrap();
        control
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let mut child = spawn(role, &app, &child_control, &source, 1000);
        drop((app, child_control));
        let mut channel = HostChannel::authenticate(host, child.0.id(), 1000).unwrap();
        let mut sent = [0];
        control.read_exact(&mut sent).unwrap();
        assert_eq!(sent, *b"S");
        if role == "retired" {
            child.finish();
        }
        assert!(channel.receive_mounted_input(3, false).is_err(), "{role}");
        drop(channel);
        let mut complete = [0];
        control.read_exact(&mut complete).unwrap();
        assert_eq!(complete, *b"F");
        child.finish();
        assert_eq!(std::fs::read(&source).unwrap(), b"selected original bytes");
    }
}
#[test]
fn actual_late_descriptor_ack_after_root_retirement_refuses_sender_success() {
    let temp = fixture();
    let source = temp.path().join("selected");
    let (mut host, app) = UnixStream::pair().unwrap();
    let (mut broker_control, broker_child_control) = UnixStream::pair().unwrap();
    let (mut sender_control, sender_child_control) = UnixStream::pair().unwrap();
    broker_control
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    sender_control
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut sender = spawn("late-sender", &app, &sender_child_control, &source, 1000);
    let mut broker = spawn("late-broker", &host, &broker_child_control, &source, 0);
    drop((app, broker_child_control, sender_child_control));
    broker_control
        .write_all(&sender.0.id().to_ne_bytes())
        .unwrap();
    sender_control
        .write_all(&broker.0.id().to_ne_bytes())
        .unwrap();
    let mut ready = [0];
    broker_control.read_exact(&mut ready).unwrap();
    assert_eq!(ready, *b"R");
    broker.finish(); // Exact process retirement, not a mirrored test boolean.
    host.write_all(&[3]).unwrap(); // Retained socket can still supply old ACK bytes.
    let mut refused = [0];
    sender_control.read_exact(&mut refused).unwrap();
    assert_eq!(refused, *b"F");
    sender.finish();
    assert_eq!(std::fs::read(source).unwrap(), b"selected original bytes");
}
