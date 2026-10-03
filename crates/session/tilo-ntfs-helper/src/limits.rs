//! Process isolation for a single-threaded command entry point.
pub use crate::reader::{NativeOperation, ReadRequest};
use crate::{
    protocol::{Report, Stats, identity},
    reader::inspect,
};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::{Duration, Instant},
};
pub(crate) static CANCELLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
pub const READ_BYTES: u64 = 256 * 1024 * 1024;
pub const WALL_SECONDS: u64 = 20;
const OUTPUT_BYTES: usize = 4 * 1024 * 1024;
/// Child-to-parent reports: a little-endian `u32` length, then one JSON value.
const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(65_536);
unsafe extern "C" {
    fn tilo_child_prepare(
        input: i32,
        output: i32,
        uid: u32,
        gid: u32,
        production: i32,
        expected_parent: i32,
    ) -> i32;
}
fn retry_interrupted<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match operation() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}
struct Child(libc::pid_t);
impl Drop for Child {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0, libc::SIGKILL);
            let _ = retry_interrupted(|| {
                if libc::waitpid(self.0, std::ptr::null_mut(), 0) < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
}
fn frame(output: &mut impl Write, value: &serde_json::Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    Ok(FRAMING.write(output, &bytes)?)
}
fn child_output(capacity: u64, operation: &NativeOperation, read_limit: u64) -> io::Result<()> {
    // prepare() retained only input fd 3 and output fd 4; _exit avoids stale inherited File destructors.
    let input = unsafe { File::from_raw_fd(3) };
    let mut output = unsafe { File::from_raw_fd(4) };
    let _ = input.as_raw_fd();
    let report = inspect(&input, capacity, operation, read_limit);
    for fact in report.facts {
        frame(&mut output, &serde_json::json!({"fact":fact}))?;
    }
    frame(
        &mut output,
        &serde_json::json!({"done":{"schema":report.schema,"status":report.status,"stats":report.stats}}),
    )?;
    std::mem::forget(input);
    std::mem::forget(output);
    Ok(())
}
/// Run only from a single-threaded CLI before starting threads.
/// No successful result survives timeout, truncation, child failure or protocol failure.
///
/// # Errors
/// Returns refusal when the OS boundary or native child fails.
pub fn run(
    file: &File,
    capacity: u64,
    root: u64,
    read_limit: u64,
    credentials: Option<(u32, u32)>,
    deadline: Instant,
) -> io::Result<Report> {
    run_operation(
        file,
        capacity,
        NativeOperation::Probe { root },
        read_limit,
        credentials,
        deadline,
    )
}
/// Executes one bounded native operation; only the broker owns production claims.
/// # Errors
/// Refuses process, transport, native identity or resource failures.
pub fn run_operation(
    file: &File,
    capacity: u64,
    operation: NativeOperation,
    read_limit: u64,
    credentials: Option<(u32, u32)>,
    deadline: Instant,
) -> io::Result<Report> {
    run_cancellable(
        file,
        capacity,
        operation,
        read_limit,
        credentials,
        deadline,
        None,
    )
}
pub(crate) fn run_cancellable(
    file: &File,
    capacity: u64,
    operation: NativeOperation,
    read_limit: u64,
    credentials: Option<(u32, u32)>,
    deadline: Instant,
    peer: Option<i32>,
) -> io::Result<Report> {
    check_peer(peer)?;
    let mut descriptors = [-1; 2];
    if unsafe { libc::pipe(descriptors.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let [read_fd, write_fd] = descriptors;
    let read_fd = unsafe { OwnedFd::from_raw_fd(read_fd) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(write_fd) };
    let parent = unsafe { libc::getpid() };
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        let (uid, gid) = credentials.unwrap_or((0, 0));
        if unsafe {
            tilo_child_prepare(
                file.as_raw_fd(),
                write_fd.as_raw_fd(),
                uid,
                gid,
                i32::from(credentials.is_some()),
                parent,
            )
        } != 0
        {
            unsafe { libc::_exit(120) }
        }
        let status = if child_output(capacity, &operation, read_limit).is_ok() {
            0
        } else {
            121
        };
        unsafe { libc::_exit(status) }
    }
    drop(write_fd);
    let child = Child(pid);
    let bytes = read_output(File::from(read_fd), deadline, peer)?;
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
        if result == pid {
            break;
        }
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native child exit deadline",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    std::mem::forget(child);
    if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
        return Err(io::Error::other(format!("native child failed: {status}")));
    }
    if CANCELLED.load(std::sync::atomic::Ordering::Acquire) {
        return Err(io::Error::other("cancelled native completion"));
    }
    check_peer(peer)?;
    let mut report = decode(&bytes)?;
    if report.status == 0 {
        match operation {
            NativeOperation::Probe { root } | NativeOperation::Manifest { root } => {
                report.native_identity = Some(identity(&report, root)?);
            }
            NativeOperation::ReadChunk { ref request } => {
                decode_chunk(&report, request.length)?;
            }
        }
    }
    let schema = if matches!(operation, NativeOperation::Probe { .. }) {
        1
    } else {
        2
    };
    if report.schema != schema {
        return Err(io::Error::other("operation schema mismatch"));
    }
    Ok(report)
}
fn check_peer(peer: Option<i32>) -> io::Result<()> {
    if let Some(fd) = peer {
        let mut state = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&raw mut state, 1, 0) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if state.revents != 0 {
            return Err(io::Error::other("peer invalidated native transaction"));
        }
    }
    Ok(())
}
fn read_output(mut input: File, deadline: Instant, peer: Option<i32>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        if CANCELLED.load(std::sync::atomic::Ordering::Acquire) {
            return Err(io::Error::other("cancelled native operation"));
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "native wall deadline"))?;
        let millis = i32::try_from(remaining.as_millis().min(1000)).map_err(io::Error::other)?;
        let fd = libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let mut fds = [
            fd,
            libc::pollfd {
                fd: peer.unwrap_or(-1),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let result = unsafe { libc::poll(fds.as_mut_ptr(), 2, millis) };
        let [_, peer_state] = fds;
        if peer_state.revents != 0 {
            return Err(io::Error::other(
                "peer cancelled, disconnected or pipelined during native operation",
            ));
        }
        if result < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if result == 0 {
            continue;
        }
        let mut chunk = [0_u8; 8192];
        let n = input.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        if bytes.len().saturating_add(n) > OUTPUT_BYTES {
            return Err(io::Error::other("native output limit"));
        }
        bytes.extend_from_slice(
            chunk
                .get(..n)
                .ok_or_else(|| io::Error::other("read size"))?,
        );
    }
    Ok(bytes)
}
fn decode(mut bytes: &[u8]) -> io::Result<Report> {
    let mut facts = Vec::new();
    let mut done = None;
    let mut payload = Vec::new();
    while FRAMING.read_into(&mut bytes, &mut payload)? {
        if done.is_some() {
            return Err(io::Error::other("trailing child output"));
        }
        let frame: serde_json::Value = serde_json::from_slice(&payload)?;
        if let Some(fact) = frame.get("fact") {
            facts.push(fact.clone());
        } else if let Some(summary) = frame.get("done") {
            done = Some(summary.clone());
        } else {
            return Err(io::Error::other("unknown frame"));
        }
    }
    let done = done.ok_or_else(|| io::Error::other("missing completion frame"))?;
    let status = i32::try_from(
        done.get("status")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| io::Error::other("missing status"))?,
    )
    .map_err(io::Error::other)?;
    let counters: Stats = serde_json::from_value(
        done.get("stats")
            .ok_or_else(|| io::Error::other("missing counters"))?
            .clone(),
    )?;
    if done
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .is_none_or(|s| s != 1 && s != 2)
        || !(0..=7).contains(&status)
        || counters.writes != 0
        || counters.bytes > READ_BYTES
        || counters.calls > 65_536
    {
        return Err(io::Error::other("invalid native evidence"));
    }
    Ok(Report {
        schema: u32::try_from(
            done.get("schema")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| io::Error::other("schema"))?,
        )
        .map_err(io::Error::other)?,
        source_pin: "d327833ec1d5eb1358b6f2c37139f10a3460944d".into(),
        diagnostic_only: true,
        memory_limit_enforced: cfg!(target_os = "linux"),
        native_identity: None,
        metadata_complete: false,
        status,
        facts,
        stats: counters,
    })
}

/// Validates exact ordered native payload coverage. Partial native failure is never returned as bytes.
/// # Errors
/// Refuses malformed, partial, overlapping or oversized native content.
pub fn decode_chunk(report: &Report, length: u32) -> io::Result<Vec<u8>> {
    if report.schema != 2 || report.status != 0 || length == 0 || length > 1_048_576 {
        return Err(io::Error::other("invalid chunk result"));
    }
    let completions: Vec<_> = report
        .facts
        .iter()
        .filter(|f| f.get("kind").and_then(serde_json::Value::as_str) == Some("attributes_end"))
        .collect();
    let [before, after] = completions.as_slice() else {
        return Err(io::Error::other("missing chunk metadata completion"));
    };
    if before != after {
        return Err(io::Error::other("changed chunk object"));
    }
    let record = before
        .get("record")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| io::Error::other("chunk object missing"))?;
    crate::manifest::metadata_digest(&report.facts, record)?;
    let mut bytes = Vec::with_capacity(length as usize);
    for f in &report.facts {
        if f["kind"] != "data" {
            continue;
        }
        if f["offset"].as_u64() != Some(bytes.len() as u64) {
            return Err(io::Error::other("chunk coverage"));
        }
        let hex = f["hex"]
            .as_str()
            .ok_or_else(|| io::Error::other("data encoding"))?;
        if hex.is_empty() || hex.len() > 8192 || !hex.len().is_multiple_of(2) {
            return Err(io::Error::other("data length"));
        }
        for pair in hex.as_bytes().chunks_exact(2) {
            bytes.push(
                u8::from_str_radix(std::str::from_utf8(pair).map_err(io::Error::other)?, 16)
                    .map_err(io::Error::other)?,
            );
        }
        if bytes.len() > length as usize {
            return Err(io::Error::other("excess content"));
        }
    }
    if bytes.len() != length as usize {
        return Err(io::Error::other("short content"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn child_limits_survive_cancellation_and_framing_refuses_bad_results() {
        // Phase `cancellation_signal_cannot_interrupt_child_reaping`.
        {
            let mut attempts = 0;
            let result = retry_interrupted(|| {
                attempts += 1;
                if attempts < 3 {
                    Err(io::Error::from(io::ErrorKind::Interrupted))
                } else {
                    Ok("reaped")
                }
            });
            assert_eq!(result.unwrap(), "reaped");
            assert_eq!(attempts, 3);
            let mut attempts = 0;
            let error = retry_interrupted(|| {
                attempts += 1;
                Err::<(), _>(io::Error::from_raw_os_error(libc::ECHILD))
            })
            .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
            assert_eq!(attempts, 1);
        }
        // Phase `parent_peer_monitor_refuses_cancel_and_disconnect`.
        {
            use std::os::unix::net::UnixStream;
            let (parent, mut peer) = UnixStream::pair().unwrap();
            check_peer(Some(parent.as_raw_fd())).unwrap();
            peer.write_all(b"cancel").unwrap();
            assert!(check_peer(Some(parent.as_raw_fd())).is_err());
            let (parent, peer) = UnixStream::pair().unwrap();
            drop(peer);
            assert!(check_peer(Some(parent.as_raw_fd())).is_err());
        }
        // Phase `child_report_bytes_are_pinned`.
        // The child's report bytes: a little-endian `u32` length, then JSON.
        {
            let mut output = Vec::new();
            frame(&mut output, &serde_json::json!({"fact": 1})).unwrap();
            assert_eq!(output, b"\x0a\0\0\0{\"fact\":1}");
        }
        // Phase `child_framing_refuses_trailing_unknown_and_oversized_results`.
        {
            let valid = serde_json::json!({"done":{"schema":2,"status":0,"stats":{"bytes":0,"calls":0,"writes":0}}});
            let encode = |value: &serde_json::Value| {
                let payload = serde_json::to_vec(value).unwrap();
                let mut bytes = u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
                bytes.extend(payload);
                bytes
            };
            assert!(decode(&encode(&valid)).is_ok());
            let mut trailing = encode(&valid);
            trailing.extend(encode(&valid));
            assert!(decode(&trailing).is_err());
            let mut unknown = valid;
            unknown["done"]["status"] = serde_json::json!(42);
            assert!(decode(&encode(&unknown)).is_err());
            assert!(decode(&65_537_u32.to_le_bytes()).is_err());
            assert!(decode(&[1, 0, 0, 0]).is_err());
        }
    }
}
