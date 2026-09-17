//! Process isolation for a single-threaded command entry point.
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
pub const READ_BYTES: u64 = 256 * 1024 * 1024;
pub const WALL_SECONDS: u64 = 20;
const OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const FRAME_BYTES: usize = 65_536;
unsafe extern "C" {
    fn qos_child_prepare(
        input: i32,
        output: i32,
        uid: u32,
        gid: u32,
        production: i32,
        expected_parent: i32,
    ) -> i32;
}
struct Child(libc::pid_t);
impl Drop for Child {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0, libc::SIGKILL);
            libc::waitpid(self.0, std::ptr::null_mut(), 0);
        }
    }
}
fn frame(output: &mut File, value: &serde_json::Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > FRAME_BYTES {
        return Err(io::Error::other("output frame limit"));
    }
    let len = u32::try_from(bytes.len()).map_err(io::Error::other)?;
    output.write_all(&len.to_le_bytes())?;
    output.write_all(&bytes)
}
fn child_output(capacity: u64, root: u64, read_limit: u64) -> io::Result<()> {
    // prepare() retained only input fd 3 and output fd 4; _exit avoids stale inherited File destructors.
    let input = unsafe { File::from_raw_fd(3) };
    let mut output = unsafe { File::from_raw_fd(4) };
    let _ = input.as_raw_fd();
    let report = inspect(&input, capacity, root, read_limit);
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
            qos_child_prepare(
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
        let status = if child_output(capacity, root, read_limit).is_ok() {
            0
        } else {
            121
        };
        unsafe { libc::_exit(status) }
    }
    drop(write_fd);
    let child = Child(pid);
    let mut input = File::from(read_fd);
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "native wall deadline"))?;
        let millis = i32::try_from(remaining.as_millis().min(1000)).map_err(io::Error::other)?;
        let mut fd = libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&raw mut fd, 1, millis) };
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
    let mut report = decode(&bytes)?;
    if report.status == 0 {
        report.native_identity = Some(identity(&report, root)?);
    }
    Ok(report)
}
fn decode(mut bytes: &[u8]) -> io::Result<Report> {
    let mut facts = Vec::new();
    let mut done = None;
    while !bytes.is_empty() {
        if done.is_some() {
            return Err(io::Error::other("trailing child output"));
        }
        let length = bytes
            .get(..4)
            .ok_or_else(|| io::Error::other("truncated frame"))?;
        let length = u32::from_le_bytes(length.try_into().map_err(io::Error::other)?);
        let length = usize::try_from(length).map_err(io::Error::other)?;
        if length > FRAME_BYTES {
            return Err(io::Error::other("oversized frame"));
        }
        bytes = bytes
            .get(4..)
            .ok_or_else(|| io::Error::other("frame header"))?;
        let frame: serde_json::Value = serde_json::from_slice(
            bytes
                .get(..length)
                .ok_or_else(|| io::Error::other("truncated body"))?,
        )?;
        bytes = bytes
            .get(length..)
            .ok_or_else(|| io::Error::other("frame body"))?;
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
    if done.get("schema").and_then(serde_json::Value::as_u64) != Some(1)
        || !(0..=7).contains(&status)
        || counters.writes != 0
        || counters.bytes > READ_BYTES
        || counters.calls > 65536
    {
        return Err(io::Error::other("invalid native evidence"));
    }
    Ok(Report {
        schema: 1,
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
