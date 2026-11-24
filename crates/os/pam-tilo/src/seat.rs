//! Telling this computer's daemon who signed in: when a session opens, the account's uid, the
//! person its identity directory holds (when it holds one), and whether the session is to hand
//! over the workspaces its desktop kept, which then are the person's here; when it closes, that
//! the account left. The daemon serves the account as the device's person only when it recorded
//! that account as theirs, so naming a person proves nothing, and acts for that account as that
//! person until it left.
//!
//! Both are the daemon's actions `seats#signed-in` and `seats#left`, called on its native
//! endpoint as any action is, and answered for root alone. A daemon that is not running, or that
//! refuses, never holds up a sign-in: the session goes on, and the account is answered as an
//! unknown one until it signs in again.
//!
//! The daemon answers a sign-in with the link the person's shell is admitted by, a descriptor
//! that comes with the answer's line: it is the session's to hand to its shell, and no file or
//! token any other process of the account could read.

use std::ffi::CString;
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where an account keeps the public key of the person it is, below its home.
const PERSON: &str = ".config/tilo/identity/user-identity/pubkey.bin";
/// How long the daemon has to answer.
const ANSWER: Duration = Duration::from_secs(2);

/// Say that `user` signed in, their session's `$XDG_STATE_HOME` being `state_home` when the
/// stack set it: the link the person's shell is admitted by, when the daemon made one.
///
/// # Errors
/// The account is unknown, or the daemon refused.
pub fn signed_in(
    socket: &Path,
    user: &str,
    state_home: Option<&str>,
) -> Result<Option<OwnedFd>, String> {
    let (uid, home) = account(user)?;
    // The person the account holds, which the daemon serves it as a key of when it is not the
    // device's own.
    let person = std::fs::read(home.join(PERSON))
        .map(|bytes| {
            let hex = bytes.iter().fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });
            format!(r#""person":"{hex}","#)
        })
        .unwrap_or_default();
    // A desktop that kept workspaces with a service of its own hands them over from its session.
    let importing = tilo_paths::xdg::Base::State
        .resolve_from(state_home.map(std::ffi::OsStr::new), Some(home.as_os_str()))
        .is_some_and(|state| {
            tilo_paths::desktop_service::kept(&tilo_paths::desktop_service::dir(&state))
        });
    act(
        socket,
        "seats#signed-in",
        &format!(r#"{{{person}"uid":{uid},"importing":{importing}}}"#),
    )
}

/// Say that `user`'s session ended.
///
/// # Errors
/// The account is unknown, or the daemon refused.
pub fn left(socket: &Path, user: &str, _: Option<&str>) -> Result<Option<OwnedFd>, String> {
    let (uid, _) = account(user)?;
    act(socket, "seats#left", &format!(r#"{{"uid":{uid}}}"#)).map(|_| None)
}

/// The account named `user`: its uid and home.
fn account(user: &str) -> Result<(u32, PathBuf), String> {
    use std::os::unix::ffi::OsStrExt as _;
    let name = CString::new(user).map_err(|_| "an account name with a NUL".to_owned())?;
    // SAFETY: an all-zero passwd is a valid out-buffer for getpwnam_r.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0_u8; 16 * 1024];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: a NUL-terminated name, and out-pointers to the entry, its string buffer of the
    // length given, and the result, all live for the call.
    let rc = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            &raw mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut found,
        )
    };
    if rc != 0 || found.is_null() || entry.pw_dir.is_null() {
        return Err(format!("{user} is no account of this computer"));
    }
    // SAFETY: getpwnam_r found the entry, whose home is a NUL-terminated string in `buffer`.
    let home = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
    Ok((
        entry.pw_uid,
        PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes())),
    ))
}

/// Call the daemon's action `path` with `args` (a JSON object) on its native endpoint at
/// `socket`: the descriptor its answer carries, if any. A refusal it answers is returned.
fn act(socket: &Path, path: &str, args: &str) -> Result<Option<OwnedFd>, String> {
    let line = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"tilo_do","arguments":{{"path":"{path}","args":{args}}}}}}}"#
    );
    let mut stream = tilo_socket::connect(socket).map_err(|error| {
        format!(
            "Tilo's daemon is not answering at {}: {error}",
            socket.display()
        )
    })?;
    stream
        .set_read_timeout(Some(ANSWER))
        .and_then(|()| stream.set_write_timeout(Some(ANSWER)))
        .and_then(|()| writeln!(stream, "{line}"))
        .map_err(|error| error.to_string())?;
    let (answer, handed) = answer(&stream).map_err(|error| error.to_string())?;
    let answer = String::from_utf8_lossy(&answer);
    if answer.is_empty() || answer.contains(r#""error""#) || answer.contains(r#""isError":true"#) {
        return Err(format!("Tilo's daemon refused: {}", answer.trim()));
    }
    Ok(handed)
}

/// One line of `stream`, and the descriptor that came with it: one, and the first of any more.
fn answer(stream: &UnixStream) -> std::io::Result<(Vec<u8>, Option<OwnedFd>)> {
    let mut line = Vec::new();
    let mut handed = None;
    // Room for the one descriptor an answer carries, and a few more that are closed.
    let mut control = [0_u64; 8];
    let mut buffer = [0_u8; 4096];
    while !line.contains(&b'\n') {
        let mut part = libc::iovec {
            iov_base: buffer.as_mut_ptr().cast(),
            iov_len: buffer.len(),
        };
        // SAFETY: an all-zero msghdr is a valid empty one.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &raw mut part;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of_val(&control);
        // SAFETY: the message names the buffer and control space above, live for the call.
        let read =
            unsafe { libc::recvmsg(stream.as_raw_fd(), &raw mut message, libc::MSG_CMSG_CLOEXEC) };
        let read = usize::try_from(read).map_err(|_| std::io::Error::last_os_error())?;
        for fd in received(&message) {
            if handed.is_none() {
                handed = Some(fd);
            }
        }
        if read == 0 {
            break;
        }
        line.extend_from_slice(buffer.get(..read).unwrap_or_default());
    }
    Ok((line, handed))
}

/// The descriptors `message` received, each owned once.
fn received(message: &libc::msghdr) -> Vec<OwnedFd> {
    let mut fds = Vec::new();
    // SAFETY: the message recvmsg filled; each header lies inside its control space.
    let mut header = unsafe { libc::CMSG_FIRSTHDR(message) };
    while let Some(current) = std::ptr::NonNull::new(header) {
        // SAFETY: a header CMSG_FIRSTHDR or CMSG_NXTHDR gave.
        let cmsg = unsafe { current.as_ref() };
        if cmsg.cmsg_level == libc::SOL_SOCKET && cmsg.cmsg_type == libc::SCM_RIGHTS {
            // SAFETY: the header's own length, as the kernel wrote it.
            let start = unsafe { libc::CMSG_LEN(0) } as usize;
            let width = std::mem::size_of::<i32>();
            let bytes = cmsg.cmsg_len.saturating_sub(start);
            // SAFETY: SCM_RIGHTS data is that many bytes of descriptors.
            let data = unsafe { libc::CMSG_DATA(header) };
            for offset in (0..bytes).step_by(width) {
                // SAFETY: inside the data, read as bytes at any alignment.
                let fd = i32::from_ne_bytes(unsafe {
                    data.add(offset).cast::<[u8; 4]>().read_unaligned()
                });
                // SAFETY: each descriptor the kernel installed here is owned once.
                fds.push(unsafe { OwnedFd::from_raw_fd(fd) });
            }
        }
        // SAFETY: the message and a header in it.
        header = unsafe { libc::CMSG_NXTHDR(message, header) };
    }
    fds
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a failed unwrap in a test is its assertion"
    )]
    use super::*;
    use std::io::Read as _;

    /// `line` written on `stream` with `fd` beside it, as the daemon answers.
    fn send_with(stream: &UnixStream, line: &[u8], fd: i32) {
        let mut control = [0_u64; 4];
        let mut part = libc::iovec {
            iov_base: line.as_ptr().cast_mut().cast(),
            iov_len: line.len(),
        };
        // SAFETY: an all-zero msghdr is a valid empty one, filled below with live buffers.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &raw mut part;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        // SAFETY: the space one descriptor takes.
        message.msg_controllen = unsafe { libc::CMSG_SPACE(4) } as usize;
        // SAFETY: the control space holds one header and its descriptor.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&raw const message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(4) as usize;
            libc::CMSG_DATA(header).cast::<i32>().write_unaligned(fd);
        }
        // SAFETY: the message built above.
        let sent = unsafe { libc::sendmsg(stream.as_raw_fd(), &raw const message, 0) };
        assert_eq!(usize::try_from(sent).unwrap(), line.len());
    }

    /// The link a sign-in's answer carries comes back with its line, even when the line arrives
    /// in parts, and is the daemon's end of a working connection.
    #[test]
    fn the_answer_to_a_sign_in_brings_the_shells_link() {
        let (daemon, pam) = UnixStream::pair().unwrap();
        let (mut ours, theirs) = UnixStream::pair().unwrap();
        send_with(&daemon, br#"{"result":"#, theirs.as_raw_fd());
        drop(theirs);
        (&daemon).write_all(b"{}}\n").unwrap();
        let (line, handed) = answer(&pam).unwrap();
        assert_eq!(line, b"{\"result\":{}}\n");
        let mut shell = UnixStream::from(handed.expect("the answer carries the link"));
        shell.write_all(b"shell").unwrap();
        let mut read = [0; 5];
        ours.read_exact(&mut read).unwrap();
        assert_eq!(&read, b"shell");

        // An answer without one brings none.
        (&daemon).write_all(b"{\"result\":{}}\n").unwrap();
        assert!(answer(&pam).unwrap().1.is_none());
    }
}
