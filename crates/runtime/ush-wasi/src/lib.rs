//! Tier 2 of ush: a third-party program built for WASI (preview 1), run by the shell's own host
//! with no Linux underneath ([`ush-portable.md`], "Tier 2").
//!
//! The program is a WebAssembly module run under wasmi, the engine every host has, with the
//! limits every guest in Tilo has ([`tilo_wasm_host`]). Everything it reaches is a call on the
//! shell's [`Host`], the same ones Tilo's own commands make, so a tool is held exactly as `cat`
//! and `cp` are:
//!
//! - **Files** are the host's: the program sees one tree, `/` and its working directory (WASI's
//!   preopened directories), and every read is the host's read, asked of the environment's
//!   grants first. A path no grant covers is refused before anything is read.
//! - **Writes** are the environment's actions. A file the program writes is kept in memory
//!   while it is open and becomes one `write` (or `append`) when it is closed, through the
//!   host's [`Host::open_write`], as a redirect does. Making, removing and renaming are the file
//!   verbs (`mkdir`, `rm`, `rmdir`, `mv`, `ln -s`) through [`Host::file_action`]. All of them
//!   carry the shell's [`Run`], so one run of the program is one entry of History and one undo.
//! - **No network**: preview 1 has none to give, and the socket calls answer `ENOSYS`.
//! - **Stopping**: the program runs in slices of fuel, and between two slices the shell's stop
//!   (Ctrl-C, `timeout`, its job ending) ends it with status 130.
//!
//! What is not here: the browser runs these programs in its own engine, not wasmi, and a
//! program's threads, signals and sockets are not WASI preview 1's.
//!
//! [`ush-portable.md`]: ../../../docs/app-model/ush-portable.md

mod fs;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use tilo_wasm_host::wasmi::{Limited, Translation};
use tilo_wasm_host::{ImportPolicy, Limits};
use ush_shell::{Host, Program, Run, Stdio};
use wasmi::{Caller, Extern, Linker, Memory, ResourceLimiter, StoreLimits, TypedResumableCall};

/// The module WASI preview 1's calls are imported from.
const WASI: &str = "wasi_snapshot_preview1";

/// Fuel for one slice of a run: between two slices the run looks whether to stop. A few tens
/// of milliseconds of the interpreter's work.
const SLICE: u64 = 20_000_000;

/// The limits a tool runs under: one instance with one memory of at most 1 GiB, and no bound
/// on its time but the person's stop.
pub const LIMITS: Limits = Limits {
    fuel_per_call: SLICE,
    deadline: None,
    max_memory_bytes: 1 << 30,
    max_memories: 1,
    max_tables: 8,
    max_instances: 1,
};

/// The status of a run that was stopped, as a shell reports a program ended by Ctrl-C.
pub const STOPPED: i32 = 130;

/// The status of a module that is no WASI program, or one that trapped.
pub const FAILED: i32 = 1;

/// One run of a tool: its program, as the shell asked for it.
pub struct Invocation<'a> {
    pub argv: &'a [String],
    pub env: &'a [(String, String)],
    pub cwd: &'a Path,
    pub io: &'a Stdio,
    pub run: Option<&'a Run>,
    pub stop: &'a dyn Fn() -> bool,
}

/// Run the WASI program `module` as `call` asks, its files and writes `host`'s.
#[must_use]
pub fn run(module: &[u8], host: Arc<dyn Host>, call: &Invocation<'_>) -> Program {
    let name = call
        .argv
        .first()
        .map_or("program", String::as_str)
        .to_owned();
    match execute(module, host, call) {
        Ok(status) | Err(Ended::Exited(status)) => Program::Exited(status),
        Err(Ended::Stopped) => Program::Exited(STOPPED),
        Err(Ended::Failed(why)) => {
            let _ = call.io.stderr.write_str(&format!("{name}: {why}\n"));
            Program::Exited(FAILED)
        }
    }
}

enum Ended {
    /// It ended itself (`proc_exit`) with this status.
    Exited(i32),
    Stopped,
    Failed(String),
}

/// What a run's store holds: the limits its limiter reads and the program's WASI state.
struct State {
    limits: StoreLimits,
    wasi: Wasi,
}

impl Limited for State {
    fn limits(&mut self) -> &mut dyn ResourceLimiter {
        &mut self.limits
    }
}

/// The program's view of the world: its arguments and variables, its open files, the shell's
/// streams and the host its files are read and written through.
pub(crate) struct Wasi {
    args: Vec<Vec<u8>>,
    env: Vec<Vec<u8>>,
    io: Stdio,
    pub(crate) host: Arc<dyn Host>,
    pub(crate) run: Option<Run>,
    pub(crate) fds: fs::Table,
    started: Instant,
}

fn execute(module: &[u8], host: Arc<dyn Host>, call: &Invocation<'_>) -> Result<i32, Ended> {
    let engine = tilo_wasm_host::wasmi::engine(Translation::Lazy);
    let module = tilo_wasm_host::wasmi::compile(&engine, module, ImportPolicy::Any)
        .map_err(|failure| Ended::Failed(failure.to_string()))?;
    let wasi = Wasi {
        args: call
            .argv
            .iter()
            .map(|arg| nul_terminated(arg.as_bytes()))
            .collect(),
        env: call
            .env
            .iter()
            .map(|(name, value)| nul_terminated(format!("{name}={value}").as_bytes()))
            .collect(),
        io: call.io.clone(),
        fds: fs::Table::new(call.cwd),
        host,
        run: call.run.cloned(),
        started: Instant::now(),
    };
    let state = State {
        limits: tilo_wasm_host::wasmi::store_limits(&LIMITS),
        wasi,
    };
    let mut store = tilo_wasm_host::wasmi::store(&engine, state, &LIMITS)
        .map_err(|failure| Ended::Failed(failure.to_string()))?;
    let mut linker = Linker::<State>::new(&engine);
    link(&mut linker).map_err(|error| Ended::Failed(error.to_string()))?;
    let outcome = (|| {
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|error| exited_or(&error))?;
        let start = instance
            .get_typed_func::<(), ()>(&store, "_start")
            .map_err(|_| {
                Ended::Failed("not a WASI program: it has no `_start` to run".to_owned())
            })?;
        let mut call_state = start
            .call_resumable(&mut store, ())
            .map_err(|error| exited_or(&error))?;
        loop {
            match call_state {
                TypedResumableCall::Finished(()) => return Ok(0),
                TypedResumableCall::OutOfFuel(out_of_fuel) => {
                    if (call.stop)() {
                        return Err(Ended::Stopped);
                    }
                    store
                        .set_fuel(SLICE)
                        .map_err(|error| Ended::Failed(error.to_string()))?;
                    call_state = out_of_fuel
                        .resume(&mut store)
                        .map_err(|error| exited_or(&error))?;
                }
                // A host call that failed: the program's own `proc_exit` is how it ends.
                TypedResumableCall::HostTrap(trapped) => {
                    let error = trapped.host_error();
                    return Err(error
                        .i32_exit_status()
                        .map_or_else(|| Ended::Failed(error.to_string()), Ended::Exited));
                }
            }
        }
    })();
    // Whatever it left open is closed as it ended: each file it wrote is written now.
    let wasi = &mut store.data_mut().wasi;
    let (host, run) = (Arc::clone(&wasi.host), wasi.run.clone());
    wasi.fds.close_all(&host, run.as_ref());
    outcome
}

fn exited_or(error: &wasmi::Error) -> Ended {
    if let Some(status) = error.i32_exit_status() {
        return Ended::Exited(status);
    }
    Ended::Failed(tilo_wasm_host::wasmi::classify(error).to_string())
}

fn nul_terminated(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    out.push(0);
    out
}

// WASI's numbers, as preview 1 defines them.
pub(crate) const SUCCESS: i32 = 0;
pub(crate) const E2BIG: i32 = 1;
pub(crate) const EACCES: i32 = 2;
pub(crate) const EBADF: i32 = 8;
pub(crate) const EEXIST: i32 = 20;
pub(crate) const EFAULT: i32 = 21;
pub(crate) const EINVAL: i32 = 28;
pub(crate) const EIO: i32 = 29;
pub(crate) const EISDIR: i32 = 31;
pub(crate) const ENOENT: i32 = 44;
pub(crate) const ENOSYS: i32 = 52;
pub(crate) const ENOTDIR: i32 = 54;
pub(crate) const ENOTEMPTY: i32 = 55;
pub(crate) const ENOTSUP: i32 = 58;
pub(crate) const ESPIPE: i32 = 70;

/// The program's memory, as its `memory` export.
fn memory(caller: &Caller<'_, State>) -> Option<Memory> {
    caller.get_export("memory").and_then(Extern::into_memory)
}

/// Read `len` bytes of the program's memory at `at`.
pub(crate) fn read_bytes(caller: &Caller<'_, State>, at: i32, len: i32) -> Result<Vec<u8>, i32> {
    let memory = memory(caller).ok_or(EFAULT)?;
    let mut bytes = vec![0; usize::try_from(len).map_err(|_| EINVAL)?];
    memory
        .read(caller, usize::try_from(at).map_err(|_| EFAULT)?, &mut bytes)
        .map_err(|_| EFAULT)?;
    Ok(bytes)
}

/// Write `bytes` into the program's memory at `at`.
pub(crate) fn write_bytes(
    caller: &mut Caller<'_, State>,
    at: i32,
    bytes: &[u8],
) -> Result<(), i32> {
    let memory = memory(caller).ok_or(EFAULT)?;
    memory
        .write(caller, usize::try_from(at).map_err(|_| EFAULT)?, bytes)
        .map_err(|_| EFAULT)
}

pub(crate) fn write_u32(caller: &mut Caller<'_, State>, at: i32, value: u32) -> Result<(), i32> {
    write_bytes(caller, at, &value.to_le_bytes())
}

pub(crate) fn write_u64(caller: &mut Caller<'_, State>, at: i32, value: u64) -> Result<(), i32> {
    write_bytes(caller, at, &value.to_le_bytes())
}

pub(crate) fn read_u32(caller: &Caller<'_, State>, at: i32) -> Result<u32, i32> {
    Ok(u32::from_le_bytes(field(&read_bytes(caller, at, 4)?, 0)?))
}

/// `bytes[at..at + N]`, or `EFAULT` where it runs past them.
pub(crate) fn field<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N], i32> {
    bytes
        .get(at..at.checked_add(N).ok_or(EFAULT)?)
        .and_then(|slice| slice.try_into().ok())
        .ok_or(EFAULT)
}

/// The address of entry `index` of `size` bytes each, from `base`.
pub(crate) fn offset(base: i32, index: impl TryInto<i32>, size: i32) -> Result<i32, i32> {
    index
        .try_into()
        .ok()
        .and_then(|index: i32| index.checked_mul(size))
        .and_then(|at| base.checked_add(at))
        .ok_or(EFAULT)
}

/// The buffers a vector of `(pointer, length)` pairs names.
pub(crate) fn iovecs(
    caller: &Caller<'_, State>,
    at: i32,
    count: i32,
) -> Result<Vec<(i32, i32)>, i32> {
    let mut out = Vec::new();
    for index in 0..count {
        let entry = offset(at, index, 8)?;
        let pointer = read_u32(caller, entry)?;
        let len = read_u32(caller, entry.checked_add(4).ok_or(EFAULT)?)?;
        out.push((
            i32::try_from(pointer).map_err(|_| EFAULT)?,
            i32::try_from(len).map_err(|_| EFAULT)?,
        ));
    }
    Ok(out)
}

/// A path the program names, as text.
pub(crate) fn path_of(caller: &Caller<'_, State>, at: i32, len: i32) -> Result<String, i32> {
    String::from_utf8(read_bytes(caller, at, len)?).map_err(|_| EINVAL)
}

fn errno(result: Result<(), i32>) -> i32 {
    result.err().unwrap_or(SUCCESS)
}

/// Write a list of NUL-terminated strings the way `args_get` and `environ_get` do.
fn strings(
    caller: &mut Caller<'_, State>,
    list: &[Vec<u8>],
    pointers: i32,
    buffer: i32,
) -> Result<(), i32> {
    let mut at = buffer;
    for (index, item) in list.iter().enumerate() {
        let pointer = offset(pointers, index, 4)?;
        write_u32(caller, pointer, u32::try_from(at).map_err(|_| EFAULT)?)?;
        write_bytes(caller, at, item)?;
        at = at
            .checked_add(i32::try_from(item.len()).map_err(|_| E2BIG)?)
            .ok_or(EFAULT)?;
    }
    Ok(())
}

fn sizes(
    caller: &mut Caller<'_, State>,
    list: &[Vec<u8>],
    count: i32,
    size: i32,
) -> Result<(), i32> {
    let total: usize = list.iter().map(Vec::len).sum();
    write_u32(caller, count, u32::try_from(list.len()).map_err(|_| E2BIG)?)?;
    write_u32(caller, size, u32::try_from(total).map_err(|_| E2BIG)?)
}

/// Nanoseconds by the clock `id`: the place's wall clock, or the run's own monotonic one.
fn now_ns(wasi: &Wasi, id: i32) -> Result<u64, i32> {
    match id {
        0 => {
            let ms = u64::try_from(wasi.host.now_ms()).unwrap_or(0);
            Ok(ms.saturating_mul(1_000_000))
        }
        1..=3 => Ok(u64::try_from(wasi.started.elapsed().as_nanos()).unwrap_or(u64::MAX)),
        _ => Err(EINVAL),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one entry per call of WASI preview 1, in its order"
)]
fn link(linker: &mut Linker<State>) -> Result<(), wasmi::Error> {
    linker.func_wrap(
        WASI,
        "args_get",
        |mut c: Caller<'_, State>, argv: i32, buf: i32| {
            let args = c.data().wasi.args.clone();
            errno(strings(&mut c, &args, argv, buf))
        },
    )?;
    linker.func_wrap(
        WASI,
        "args_sizes_get",
        |mut c: Caller<'_, State>, count: i32, size: i32| {
            let args = c.data().wasi.args.clone();
            errno(sizes(&mut c, &args, count, size))
        },
    )?;
    linker.func_wrap(
        WASI,
        "environ_get",
        |mut c: Caller<'_, State>, env: i32, buf: i32| {
            let vars = c.data().wasi.env.clone();
            errno(strings(&mut c, &vars, env, buf))
        },
    )?;
    linker.func_wrap(
        WASI,
        "environ_sizes_get",
        |mut c: Caller<'_, State>, count: i32, size: i32| {
            let vars = c.data().wasi.env.clone();
            errno(sizes(&mut c, &vars, count, size))
        },
    )?;
    linker.func_wrap(
        WASI,
        "clock_res_get",
        |mut c: Caller<'_, State>, id: i32, at: i32| {
            let resolution = match id {
                0 => 1_000_000,
                1..=3 => 1,
                _ => return EINVAL,
            };
            errno(write_u64(&mut c, at, resolution))
        },
    )?;
    linker.func_wrap(
        WASI,
        "clock_time_get",
        |mut c: Caller<'_, State>, id: i32, _precision: i64, at: i32| {
            let now = now_ns(&c.data().wasi, id);
            errno(now.and_then(|now| write_u64(&mut c, at, now)))
        },
    )?;
    linker.func_wrap(
        WASI,
        "random_get",
        |mut c: Caller<'_, State>, at: i32, len: i32| {
            let mut bytes = vec![0; usize::try_from(len).unwrap_or(0)];
            if getrandom::fill(&mut bytes).is_err() {
                return EIO;
            }
            errno(write_bytes(&mut c, at, &bytes))
        },
    )?;
    linker.func_wrap(
        WASI,
        "proc_exit",
        |_: Caller<'_, State>, status: i32| -> Result<(), wasmi::Error> {
            Err(wasmi::Error::i32_exit(status))
        },
    )?;
    linker.func_wrap(WASI, "proc_raise", |_: Caller<'_, State>, _signal: i32| {
        ENOSYS
    })?;
    linker.func_wrap(WASI, "sched_yield", |_: Caller<'_, State>| SUCCESS)?;
    linker.func_wrap(
        WASI,
        "poll_oneoff",
        |mut c: Caller<'_, State>, subs: i32, events: i32, count: i32, done: i32| {
            errno(poll(&mut c, subs, events, count, done))
        },
    )?;
    linker.func_wrap(
        WASI,
        "fd_write",
        |mut c: Caller<'_, State>, fd: i32, iovs: i32, count: i32, written: i32| {
            errno(fd_write(&mut c, fd, iovs, count, None, written))
        },
    )?;
    linker.func_wrap(
        WASI,
        "fd_pwrite",
        |mut c: Caller<'_, State>, fd: i32, iovs: i32, count: i32, at: i64, written: i32| {
            errno(fd_write(&mut c, fd, iovs, count, Some(at), written))
        },
    )?;
    linker.func_wrap(
        WASI,
        "fd_read",
        |mut c: Caller<'_, State>, fd: i32, iovs: i32, count: i32, read: i32| {
            errno(fd_read(&mut c, fd, iovs, count, None, read))
        },
    )?;
    linker.func_wrap(
        WASI,
        "fd_pread",
        |mut c: Caller<'_, State>, fd: i32, iovs: i32, count: i32, at: i64, read: i32| {
            errno(fd_read(&mut c, fd, iovs, count, Some(at), read))
        },
    )?;
    fs::link(linker)?;
    // Sockets are not preview 1's to give: a tool has no network.
    linker.func_wrap(
        WASI,
        "sock_accept",
        |_: Caller<'_, State>, _: i32, _: i32, _: i32| ENOSYS,
    )?;
    linker.func_wrap(
        WASI,
        "sock_recv",
        |_: Caller<'_, State>, _: i32, _: i32, _: i32, _: i32, _: i32, _: i32| ENOSYS,
    )?;
    linker.func_wrap(
        WASI,
        "sock_send",
        |_: Caller<'_, State>, _: i32, _: i32, _: i32, _: i32, _: i32| ENOSYS,
    )?;
    linker.func_wrap(
        WASI,
        "sock_shutdown",
        |_: Caller<'_, State>, _: i32, _: i32| ENOSYS,
    )?;
    Ok(())
}

fn fd_write(
    c: &mut Caller<'_, State>,
    fd: i32,
    iovs: i32,
    count: i32,
    at: Option<i64>,
    written: i32,
) -> Result<(), i32> {
    let mut bytes = Vec::new();
    for (pointer, len) in iovecs(c, iovs, count)? {
        bytes.extend(read_bytes(c, pointer, len)?);
    }
    let wrote = match fd {
        1 | 2 if at.is_some() => return Err(ESPIPE),
        1 => c
            .data()
            .wasi
            .io
            .stdout
            .write_all(&bytes)
            .map_err(|_| EIO)
            .map(|()| bytes.len())?,
        2 => c
            .data()
            .wasi
            .io
            .stderr
            .write_all(&bytes)
            .map_err(|_| EIO)
            .map(|()| bytes.len())?,
        _ => {
            let state = &mut c.data_mut().wasi;
            let (host, run) = (Arc::clone(&state.host), state.run.clone());
            state.fds.write(fd, &bytes, at, &host, run.as_ref())?
        }
    };
    write_u32(c, written, u32::try_from(wrote).map_err(|_| EIO)?)
}

fn fd_read(
    c: &mut Caller<'_, State>,
    fd: i32,
    iovs: i32,
    count: i32,
    at: Option<i64>,
    read: i32,
) -> Result<(), i32> {
    let buffers = iovecs(c, iovs, count)?;
    let mut total = 0_usize;
    for (pointer, len) in buffers {
        let mut bytes = vec![0; usize::try_from(len).map_err(|_| EFAULT)?];
        let got = if fd == 0 {
            if at.is_some() {
                return Err(ESPIPE);
            }
            c.data().wasi.io.stdin.read(&mut bytes).map_err(|_| EIO)?
        } else {
            let from = at.map(|at| {
                u64::try_from(at)
                    .unwrap_or(0)
                    .saturating_add(u64::try_from(total).unwrap_or(u64::MAX))
            });
            c.data_mut().wasi.fds.read(fd, &mut bytes, from)?
        };
        write_bytes(c, pointer, bytes.get(..got).ok_or(EIO)?)?;
        total = total.saturating_add(got);
        if got < bytes.len() {
            break;
        }
    }
    write_u32(c, read, u32::try_from(total).map_err(|_| EIO)?)
}

/// `poll_oneoff`: a clock waits on the place's clock; a file is always ready.
fn poll(
    c: &mut Caller<'_, State>,
    subs: i32,
    events: i32,
    count: i32,
    done: i32,
) -> Result<(), i32> {
    let mut wait_ns: Option<u64> = None;
    let mut ready = Vec::new();
    for index in 0..count {
        let sub = read_bytes(c, offset(subs, index, 48)?, 48)?;
        let userdata = u64::from_le_bytes(field(&sub, 0)?);
        let [tag] = field(&sub, 8)?;
        if tag == 0 {
            let timeout = u64::from_le_bytes(field(&sub, 24)?);
            let absolute = u16::from_le_bytes(field(&sub, 40)?) & 1 == 1;
            let id = i32::from_le_bytes(field(&sub, 16)?);
            let relative = if absolute {
                timeout.saturating_sub(now_ns(&c.data().wasi, id)?)
            } else {
                timeout
            };
            wait_ns = Some(wait_ns.map_or(relative, |wait| wait.min(relative)));
            ready.push((userdata, tag, true));
        } else {
            ready.push((userdata, tag, false));
        }
    }
    let files_ready = ready.iter().any(|(_, _, clock)| !clock);
    if !files_ready && let Some(wait) = wait_ns {
        c.data().wasi.host.sleep(wait / 1_000_000);
    }
    let mut written = 0_u32;
    for (userdata, tag, clock) in &ready {
        if files_ready && *clock {
            continue;
        }
        // userdata, then no error, then the event's type; the rest of its 32 bytes are zero.
        let mut event = Vec::with_capacity(32);
        event.extend_from_slice(&userdata.to_le_bytes());
        event.extend_from_slice(&[0, 0, *tag]);
        event.resize(32, 0);
        write_bytes(c, offset(events, written, 32)?, &event)?;
        written = written.saturating_add(1);
    }
    write_u32(c, done, written)
}

#[cfg(test)]
mod tests;
