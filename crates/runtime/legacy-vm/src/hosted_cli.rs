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
        [--evict-notice FILE] [--memory-mib MIB] [--status-listen ADDR]
        [--browser --browser-kernel VMLINUX --browser-disk-image RAW
         [--browser-spares N] [--browser-idle-stop SECONDS] [--browser-lifetime SECONDS]]
        (--jail-helper PATH [--jail-config FILE] | --unjailed)
                         own this host's workspace VMs; listens on RUN/hosted.sock.
                         Each VM runs jailed, with a network, through the
                         privileged tilo-hosted-jail helper and its root-owned
                         configuration (default /etc/tilo/hosted-jail.json);
                         --unjailed is for development only: no network.
                         When FILE appears (written by whatever watches the
                         cloud's notice that it takes this host back), every
                         VM is suspended onto STATE and no more are woken.
                         --memory-mib bounds what the VMs awake at once take
                         together, by their sizes; a wake past it waits.
                         --status-listen answers Status alone over TCP, for a
                         placer.
                         --browser also runs the workspaces' browser
                         environments from the browser guest image: N spares
                         (default 2) boot with a session waiting for a
                         workspace, each environment ends after its idle limit
                         (default 600 s) or its lifetime (default 7200 s), and
                         it keeps nothing when it stops.
  status --run DIR       every VM, its phase and its last wake
  place --listen ADDR --record FILE --statefulset NAMESPACE/NAME --max N
        --status-port PORT
                         say which host runs each workspace's VM (the one holding
                         it, else the first with room), and start hosts, the
                         StatefulSet's pods NAME-0, NAME-1, …, as their VMs fill
                         them: past three quarters, or for any VM waiting
  fetch-guest --images DIR [--ref REF] [--browser]
                         fetch the pinned guest image into DIR and print the
                         kernel's and the root filesystem's paths; --browser
                         fetches the browser guest instead
  usage --run DIR --account ID [--from MS] [--to MS]
                         the account's usage records and totals, as JSON
  run --run DIR --workspace ID --account ID [--environment ID] [--size SIZE]
      -- PROGRAM [ARG...]
                         wake the workspace's VM as the account and run one
                         program in it, as the tenant, its output relayed; exits
                         with its status. For operators checking a host
  suspend --run DIR WORKSPACE ENVIRONMENT
  stop --run DIR WORKSPACE ENVIRONMENT
  browser-open --run DIR --workspace ID --account ID [--material JSON]
                         open or attach a workspace's browser environment, and
                         say what it took
  browser-stop --run DIR WORKSPACE
                         end a workspace's browser environment now
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
        "serve" => serve(&Flags::parse(rest, &["--unjailed", "--browser"])?),
        "files-serve" => files_serve(&Flags::parse(rest, &["--read-only"])?),
        "fetch-guest" => fetch_guest(&Flags::parse(rest, &["--browser"])?),
        "status" => ask(&Flags::parse(rest, &[])?, &Request::Status),
        "run" => run(rest),
        "place" => place(&Flags::parse(rest, &[])?),
        "browser-open" => {
            let flags = Flags::parse(rest, &[])?;
            ask(
                &flags,
                &Request::OpenBrowser {
                    browser: control::OpenBrowser {
                        workspace: flags.need("--workspace")?.to_string(),
                        account: flags.need("--account")?.to_string(),
                        material: flags.get("--material").unwrap_or("{}").to_string(),
                    },
                },
            )
        }
        "browser-stop" => {
            let flags = Flags::parse(rest, &[])?;
            let [workspace] = flags.rest.as_slice() else {
                return Err("browser-stop: WORKSPACE".to_string());
            };
            ask(
                &flags,
                &Request::StopBrowser {
                    workspace: workspace.clone(),
                },
            )
        }
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
    let guest = if flags.has("--browser") {
        guest::Guest::Browser
    } else {
        guest::Guest::Linux
    };
    let reference = guest::reference(guest, flags.get("--ref")).map_err(|e| e.to_string())?;
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
            browser: if flags.has("--browser") {
                Some(browser_guest(flags)?)
            } else {
                None
            },
        },
    };
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        let handler = on_signal as extern "C" fn(libc::c_int) as *const () as libc::sighandler_t;
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
    let mut service = Service::open(config, system_clock()).map_err(|e| e.to_string())?;
    if let Some(mib) = flags.get("--memory-mib") {
        let mib = mib
            .parse::<u64>()
            .map_err(|_| "--memory-mib takes mebibytes".to_string())?;
        service = service.with_memory_budget(mib);
    }
    let service = Arc::new(service);
    if let Some(address) = flags.get("--status-listen") {
        let listener = std::net::TcpListener::bind(address)
            .map_err(|e| format!("--status-listen {address}: {e}"))?;
        service.serve_status(listener);
    }
    eprintln!(
        "tilo-legacy hosted: serving {}",
        service.config().socket().display()
    );
    let evict_notice = flags.get("--evict-notice").map(PathBuf::from);
    service
        .serve(&STOP, evict_notice.as_deref())
        .map_err(|e| e.to_string())?;
    Ok(ExitCode::SUCCESS)
}

/// The browser environments' settings: the browser guest image, the spares
/// kept booted, and how long one lives.
fn browser_guest(flags: &Flags) -> Result<legacy_vm::hosted::browser::BrowserGuest, String> {
    let seconds = |name: &str, default: u64| {
        flags
            .get(name)
            .map_or(Ok(default), str::parse::<u64>)
            .map_err(|_| format!("{name} takes seconds"))
    };
    let machine = Machine {
        firecracker: PathBuf::from(flags.get("--firecracker").unwrap_or("firecracker")),
        kernel: flags.path("--browser-kernel")?,
        initramfs: None,
        cmdline: flags
            .get("--cmdline")
            .unwrap_or(Machine::DEFAULT_CMDLINE)
            .to_string(),
        base_disk: flags.get("--browser-disk-image").map(PathBuf::from),
    };
    let mut guest = legacy_vm::hosted::browser::BrowserGuest::new(machine);
    if let Some(spares) = flags.get("--browser-spares") {
        guest.spares = spares
            .parse()
            .map_err(|_| "--browser-spares takes a count".to_string())?;
    }
    guest.idle_stop = Duration::from_secs(seconds("--browser-idle-stop", 600)?.max(1));
    guest.lifetime = Duration::from_secs(seconds("--browser-lifetime", 7_200)?.max(1));
    Ok(guest)
}

/// The VM hosts as a Kubernetes `StatefulSet`: its replicas, read and set
/// through the API server with the pod's service account.
struct StatefulSetScale {
    namespace: String,
    name: String,
}

impl StatefulSetScale {
    const ACCOUNT: &'static str = "/var/run/secrets/kubernetes.io/serviceaccount";

    fn call(&self, method: &str, body: Option<&str>) -> Result<serde_json::Value, String> {
        let token = std::fs::read_to_string(format!("{}/token", Self::ACCOUNT))
            .map_err(|e| format!("the service account's token: {e}"))?;
        let url = format!(
            "https://kubernetes.default.svc/apis/apps/v1/namespaces/{}/statefulsets/{}/scale",
            self.namespace, self.name
        );
        let mut command = std::process::Command::new("curl");
        command
            .args(["-fsS", "--max-time", "10", "-X", method])
            .args(["--cacert", &format!("{}/ca.crt", Self::ACCOUNT)])
            .args(["-H", &format!("Authorization: Bearer {}", token.trim())]);
        if let Some(body) = body {
            command
                .args(["-H", "Content-Type: application/merge-patch+json"])
                .args(["--data", body]);
        }
        let output = command
            .arg(url)
            .output()
            .map_err(|e| format!("curl: {e}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
    }
}

impl legacy_vm::hosted::placement::Scale for StatefulSetScale {
    fn hosts(&self) -> Result<usize, String> {
        let scale = self.call("GET", None)?;
        scale
            .get("spec")
            .and_then(|spec| spec.get("replicas"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(|| format!("no replicas in {scale}"))
    }

    fn set_hosts(&self, count: usize) -> Result<(), String> {
        eprintln!("tilo-legacy hosted place: starting hosts up to {count}");
        self.call(
            "PATCH",
            Some(&format!(r#"{{"spec":{{"replicas":{count}}}}}"#)),
        )
        .map(|_| ())
    }
}

/// Place VMs on the `StatefulSet`'s hosts and scale them.
fn place(flags: &Flags) -> Result<ExitCode, String> {
    use legacy_vm::hosted::placement::{Placer, read_load};

    let (namespace, name) = flags
        .need("--statefulset")?
        .split_once('/')
        .ok_or("--statefulset takes NAMESPACE/NAME")?;
    let max: usize = flags
        .need("--max")?
        .parse()
        .map_err(|_| "--max takes a count".to_string())?;
    let port = flags.need("--status-port")?;
    let suffix = format!(".{name}.{namespace}.svc:{port}");
    let placer = Arc::new(
        Placer::new(
            &flags.path("--record")?,
            &format!("{name}-"),
            max,
            Box::new(StatefulSetScale {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
            }),
            Box::new(read_load),
            system_clock(),
        )?
        .with_status_suffix(&suffix),
    );
    let address = flags.need("--listen")?;
    let listener =
        std::net::TcpListener::bind(address).map_err(|e| format!("--listen {address}: {e}"))?;
    let answering = Arc::clone(&placer);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let placer = Arc::clone(&answering);
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                if let Ok(Some(request)) = control::FRAMING.read_value::<_, Request>(&mut stream) {
                    let reply = placer.handle(request);
                    let _ = control::FRAMING.write_value(&mut stream, &reply);
                }
            });
        }
    });
    eprintln!("tilo-legacy hosted place: placing on {name}-N{suffix}, at most {max}, on {address}");
    while !STOP.load(Ordering::SeqCst) {
        placer.rescale();
        std::thread::sleep(Duration::from_secs(10));
    }
    Ok(ExitCode::SUCCESS)
}

/// Wake a workspace's VM and run one program in it, as ush would.
fn run(args: &[String]) -> Result<ExitCode, String> {
    use legacy_agent_protocol::client::{self, LaunchIo};
    use legacy_agent_protocol::{AGENT_PORT, Display, Launch, Stdio};
    use legacy_vm::hosted::control::VmRequest;
    use legacy_vm::hosted::size::VmSize;

    let mut parts = args.splitn(2, |arg| arg == "--");
    let options = parts.next().unwrap_or_default();
    let program = parts.next().unwrap_or_default();
    if program.is_empty() {
        return Err(format!("run: the program follows --\n{USAGE}"));
    }
    let flags = Flags::parse(options, &[])?;
    let size = match flags.get("--size") {
        Some(name) => VmSize::parse(name)
            .filter(|size| *size != VmSize::Browser)
            .ok_or_else(|| format!("run: no VM size {name:?}"))?,
        None => VmSize::Small,
    };
    let request = Request::Ensure {
        vm: VmRequest {
            workspace: flags.need("--workspace")?.to_string(),
            environment: flags.get("--environment").unwrap_or("default").to_string(),
            account: flags.need("--account")?.to_string(),
            size,
        },
        shares: Vec::new(),
    };
    let socket = flags.path("--run")?.join("hosted.sock");
    let mut told = false;
    let link = loop {
        match control::ask(&socket, &request, Duration::from_secs(300))
            .map_err(|e| e.to_string())?
        {
            Some(Reply::Busy(message)) => {
                if !told {
                    eprintln!("tilo-legacy hosted: {message}");
                    told = true;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            other => break other,
        }
    };
    let link = match link {
        Some(Reply::Ready { link, woke_ms }) => {
            if let Some(ms) = woke_ms {
                eprintln!("tilo-legacy hosted: the VM woke in {ms} ms");
            }
            link
        }
        Some(Reply::Refused(refusal)) => return Err(refusal.message),
        Some(Reply::Error(message)) => return Err(message),
        Some(other) => return Err(format!("unexpected reply: {other:?}")),
        None => return Err(format!("no hosted-VM service at {}", socket.display())),
    };
    let stream = link.connect(AGENT_PORT).map_err(|e| e.to_string())?;
    let (mut stdout, mut stderr) = (std::io::stdout(), std::io::stderr());
    let status = client::launch(
        stream,
        &Launch {
            argv: program.to_vec(),
            cwd: None,
            env: Vec::new(),
            stdio: Stdio::Relay,
            display: Display::None,
        },
        LaunchIo {
            stdin: Some(Box::new(std::io::stdin())),
            stdout: &mut stdout,
            stderr: &mut stderr,
            started: &mut |_| {},
        },
        |_| {},
    )
    .map_err(|e| e.to_string())?;
    Ok(ExitCode::from(
        u8::try_from(status.shell_code().clamp(0, 255)).unwrap_or(1),
    ))
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
        Reply::Status { vms, load } => {
            println!(
                "memory: {} MiB awake of {}, {} MiB waiting",
                load.awake_mib,
                load.budget_mib
                    .map_or_else(|| "no budget".to_owned(), |mib| format!("{mib} MiB")),
                load.waiting_mib
            );
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
        Reply::Opened { opened_ms } => {
            println!(
                "{}",
                opened_ms.map_or_else(|| "attached".to_string(), |ms| format!("opened in {ms} ms"))
            );
            Ok(ExitCode::SUCCESS)
        }
        Reply::Placed { host } => {
            println!("{host}");
            Ok(ExitCode::SUCCESS)
        }
        Reply::Refused(refusal) => Err(refusal.message),
        Reply::Busy(message) | Reply::Error(message) => Err(message),
    }
}
