//! `tilo-legacy hosted …`: the hosted-VM service and what operators ask it.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use legacy_vm::hosted::control::{self, Reply, Request};
use legacy_vm::hosted::files::{self, Served};
use legacy_vm::hosted::firecracker::Machine;
use legacy_vm::hosted::guest;
use legacy_vm::hosted::jail;
use legacy_vm::hosted::service::{Service, ServiceConfig};
use legacy_vm::hosted::vm::{Settings, system_clock};

pub const USAGE: &str = "\
usage: tilo-legacy hosted <command>

  serve --state DIR --run DIR --kernel VMLINUX [--initramfs FILE] [--cmdline TEXT]
        [--disk-image RAW] [--firecracker PATH] [--plans FILE] [--idle-suspend SECONDS]
        (--jail-helper PATH [--jail-config FILE] | --unjailed)
                         own this host's workspace VMs; listens on RUN/hosted.sock.
                         Each VM runs jailed, with a network, through the
                         privileged tilo-hosted-jail helper and its root-owned
                         configuration (default /etc/tilo/hosted-jail.json);
                         --unjailed is for development only: no network
  status --run DIR       every VM, its phase and its last wake
  fetch-guest --images DIR [--ref REF]
                         fetch the pinned guest image into DIR and print the
                         kernel's and the root filesystem's paths
  usage --run DIR --account ID [--from MS] [--to MS]
                         the account's usage records and totals, as JSON
  suspend --run DIR WORKSPACE ENVIRONMENT
  stop --run DIR WORKSPACE ENVIRONMENT
";

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

struct Flags {
    values: Vec<(String, String)>,
    switches: Vec<String>,
    rest: Vec<String>,
}

impl Flags {
    fn parse(args: &[String], switches: &[&str]) -> Result<Self, String> {
        let mut flags = Self {
            values: Vec::new(),
            switches: Vec::new(),
            rest: Vec::new(),
        };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if switches.contains(&arg.as_str()) {
                flags.switches.push(arg.clone());
            } else if arg.starts_with("--") {
                let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
                flags.values.push((arg.clone(), value.clone()));
            } else {
                flags.rest.push(arg.clone());
            }
        }
        Ok(flags)
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(flag, _)| flag == name)
            .map(|(_, value)| value.as_str())
    }

    fn need(&self, name: &str) -> Result<&str, String> {
        self.get(name)
            .ok_or_else(|| format!("{name} is required\n{USAGE}"))
    }

    fn path(&self, name: &str) -> Result<PathBuf, String> {
        self.need(name).map(PathBuf::from)
    }

    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|switch| switch == name)
    }
}

pub fn main(args: &[String]) -> Result<ExitCode, String> {
    let (command, rest) = args.split_first().ok_or(USAGE)?;
    match command.as_str() {
        "serve" => serve(&Flags::parse(rest, &["--unjailed"])?),
        "files-serve" => files_serve(&Flags::parse(rest, &["--read-only"])?),
        "fetch-guest" => fetch_guest(&Flags::parse(rest, &[])?),
        "status" => ask(&Flags::parse(rest, &[])?, &Request::Status),
        "usage" => {
            let flags = Flags::parse(rest, &[])?;
            let number = |name: &str, default: u64| {
                flags.get(name).map_or(Ok(default), |value| {
                    value
                        .parse()
                        .map_err(|_| format!("{name} takes milliseconds"))
                })
            };
            let request = Request::Usage {
                account: flags.need("--account")?.to_string(),
                from_ms: number("--from", 0)?,
                to_ms: number("--to", u64::MAX)?,
            };
            ask(&flags, &request)
        }
        "suspend" | "stop" => {
            let flags = Flags::parse(rest, &[])?;
            let [workspace, environment] = flags.rest.as_slice() else {
                return Err(format!("{command}: WORKSPACE ENVIRONMENT"));
            };
            let (workspace, environment) = (workspace.clone(), environment.clone());
            let request = if command == "suspend" {
                Request::Suspend {
                    workspace,
                    environment,
                }
            } else {
                Request::Stop {
                    workspace,
                    environment,
                }
            };
            ask(&flags, &request)
        }
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown hosted command {other:?}\n{USAGE}")),
    }
}

/// Fetch the pinned guest image and say what a VM boots: the kernel's path and
/// the root filesystem's path, one per line.
fn fetch_guest(flags: &Flags) -> Result<ExitCode, String> {
    let images = flags.path("--images")?;
    let reference = match flags.get("--ref") {
        Some(reference) if guest::valid_pinned_ref(reference) => reference.to_string(),
        Some(reference) => {
            return Err(format!(
                "{reference} is not a digest-pinned hosted guest reference"
            ));
        }
        None => guest::current_ref().ok_or_else(|| guest::GuestError::NotPublished.to_string())?,
    };
    let guest = guest::ensure_guest(&images, &reference).map_err(|e| e.to_string())?;
    println!("kernel {}", guest.kernel.display());
    println!("rootfs {}", guest.rootfs.display());
    Ok(ExitCode::SUCCESS)
}

fn serve(flags: &Flags) -> Result<ExitCode, String> {
    let idle = flags
        .get("--idle-suspend")
        .map_or(Ok(30), str::parse::<u64>)
        .map_err(|_| "--idle-suspend takes seconds".to_string())?;
    let run = flags.path("--run")?;
    let jail = match (flags.get("--jail-helper"), flags.has("--unjailed")) {
        (Some(helper), false) => {
            let config = flags.get("--jail-config").unwrap_or(jail::DEFAULT_CONFIG);
            Some(Arc::new(jail::Jail::new(jail::Helper::open(
                PathBuf::from(helper),
                PathBuf::from(config),
            )?)))
        }
        (None, true) => None,
        _ => {
            return Err(format!(
                "pass --jail-helper (production) or --unjailed (development, no network)\n{USAGE}"
            ));
        }
    };
    let config = ServiceConfig {
        state: flags.path("--state")?,
        plans: flags.get("--plans").map_or_else(
            || flags.path("--state").map(|state| state.join("plans.json")),
            |plans| Ok(PathBuf::from(plans)),
        )?,
        run,
        settings: Settings {
            machine: Machine {
                firecracker: PathBuf::from(flags.get("--firecracker").unwrap_or("firecracker")),
                kernel: flags.path("--kernel")?,
                initramfs: flags.get("--initramfs").map(PathBuf::from),
                cmdline: flags
                    .get("--cmdline")
                    .unwrap_or(Machine::DEFAULT_CMDLINE)
                    .to_string(),
                base_disk: flags.get("--disk-image").map(PathBuf::from),
            },
            idle_suspend: Duration::from_secs(idle.max(1)),
            self_exe: std::env::current_exe().map_err(|e| e.to_string())?,
            jail,
        },
    };
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        let handler = on_signal as extern "C" fn(libc::c_int) as *const () as libc::sighandler_t;
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
    let service = Arc::new(Service::open(config, system_clock()).map_err(|e| e.to_string())?);
    eprintln!(
        "tilo-legacy hosted: serving {}",
        service.config().socket().display()
    );
    service.serve(&STOP).map_err(|e| e.to_string())?;
    Ok(ExitCode::SUCCESS)
}

/// One share's server, confined by `confine-fs` before it got here: serve
/// every guest connection on the inherited listener.
fn files_serve(flags: &Flags) -> Result<ExitCode, String> {
    use std::os::fd::FromRawFd as _;
    let fd: i32 = flags
        .need("--listen-fd")?
        .parse()
        .map_err(|_| "--listen-fd takes a descriptor".to_string())?;
    let shares = vec![Served {
        path: flags.path("--share")?,
        read_only: flags.has("--read-only"),
    }];
    // SAFETY: the parent bound this listener and passed it as `fd`; nothing
    // else in this process owns it.
    let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd) };
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) if error.raw_os_error() == Some(libc::EINTR) => continue,
            // No listener to serve: the VM is gone or the descriptor was
            // never one.
            Err(error) => return Err(format!("accept: {error}")),
        };
        let shares = shares.clone();
        std::thread::spawn(move || {
            if let Err(error) = files::serve(stream, &shares) {
                eprintln!("tilo-legacy hosted files: {error}");
            }
        });
    }
    Ok(ExitCode::SUCCESS)
}

fn ask(flags: &Flags, request: &Request) -> Result<ExitCode, String> {
    let socket = flags.path("--run")?.join("hosted.sock");
    let reply = control::ask(&socket, request, Duration::from_secs(600))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no hosted-VM service listens at {}", socket.display()))?;
    match reply {
        Reply::Status(vms) => {
            for vm in vms {
                println!(
                    "{:<40} {:<8} {:<10} {:<10} wake {} snapshot {}",
                    vm.vm,
                    vm.size,
                    vm.phase,
                    vm.account,
                    vm.last_wake_ms.map_or_else(
                        || "-".to_string(),
                        |ms| format!(
                            "{ms} ms ({})",
                            if vm.last_wake_restored == Some(true) {
                                "restored"
                            } else {
                                "booted"
                            }
                        )
                    ),
                    vm.snapshot_bytes.map_or_else(
                        || "-".to_string(),
                        |bytes| format!("{} MB", bytes / 1_000_000)
                    ),
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Reply::Usage { records, by_size } => {
            let json = serde_json::json!({ "records": records, "by_size": by_size });
            println!("{json:#}");
            Ok(ExitCode::SUCCESS)
        }
        Reply::Done | Reply::Ready { .. } => Ok(ExitCode::SUCCESS),
        Reply::Refused(refusal) => Err(refusal.message),
        Reply::Error(message) => Err(message),
    }
}
