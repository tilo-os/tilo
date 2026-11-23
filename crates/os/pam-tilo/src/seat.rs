//! Telling this computer's daemon who signed in: when a session opens, the account's uid, the
//! person it is and that person's local person proof (their signature naming the account and the
//! device's key), both from the account's identity directory; when it closes, that the account
//! left. The daemon checks the proof, so an account cannot name someone else's person, and acts
//! for that account as that person until it left.
//!
//! Both are the daemon's actions `seats#signed-in` and `seats#left`, called on its native
//! endpoint as any action is, and answered for root alone. A daemon that is not running, or that
//! refuses, never holds up a sign-in: the session goes on, and the account is answered as an
//! unknown one until it signs in again.

use std::ffi::CString;
use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where an account keeps its local person proof, below its home.
const PROOF: &str = ".config/tilo/identity/local-person.sig";
/// Where an account keeps the public key of the person it is, below its home.
const PERSON: &str = ".config/tilo/identity/user-identity/pubkey.bin";
/// How long the daemon has to answer.
const ANSWER: Duration = Duration::from_secs(2);

/// Say that `user` signed in.
///
/// # Errors
/// The account is unknown, has no proof, or the daemon refused.
pub fn signed_in(socket: &Path, user: &str) -> Result<(), String> {
    let (uid, home) = account(user)?;
    let hex = |what: &str, path: &str| {
        std::fs::read(home.join(path))
            .map(|bytes| {
                bytes.iter().fold(String::new(), |mut hex, byte| {
                    let _ = write!(hex, "{byte:02x}");
                    hex
                })
            })
            .map_err(|error| format!("{user} has no {what}: {error}"))
    };
    let person = hex("person", PERSON)?;
    let proof = hex("local person proof", PROOF)?;
    act(
        socket,
        "seats#signed-in",
        &format!(r#"{{"person":"{person}","uid":{uid},"proof":"{proof}"}}"#),
    )
}

/// Say that `user`'s session ended.
///
/// # Errors
/// The account is unknown, or the daemon refused.
pub fn left(socket: &Path, user: &str) -> Result<(), String> {
    let (uid, _) = account(user)?;
    act(socket, "seats#left", &format!(r#"{{"uid":{uid}}}"#))
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
/// `socket`: a refusal it answers is returned.
fn act(socket: &Path, path: &str, args: &str) -> Result<(), String> {
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
    let mut answer = String::new();
    BufReader::new(stream)
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    if answer.is_empty() || answer.contains(r#""error""#) || answer.contains(r#""isError":true"#) {
        return Err(format!("Tilo's daemon refused: {}", answer.trim()));
    }
    Ok(())
}
