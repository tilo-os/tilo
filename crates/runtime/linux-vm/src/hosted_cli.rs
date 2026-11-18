//! `tilo-linux-vm hosted …`: the hosted-VM service and what operators ask it.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use linux_vm::hosted::control::{self, Reply, Request};
use linux_vm::hosted::files::{self, Served};
use linux_vm::hosted::firecracker::Machine;
use linux_vm::hosted::guest;
use linux_vm::hosted::jail;
use linux_vm::hosted::placement::Host;
use linux_vm::hosted::service::{Service, ServiceConfig};
use linux_vm::hosted::vm::{Settings, system_clock};

pub const USAGE: &str = "\
usage: tilo-linux-vm hosted <command>

  serve --state DIR --run DIR --kernel VMLINUX [--initramfs FILE] [--cmdline TEXT]
        [--disk-image RAW] [--firecracker PATH] [--plans FILE] [--idle-suspend SECONDS]
        [--evict-notice FILE] [--memory-mib MIB] [--memory-reserve-mib MIB]
        [--memory-headroom-mib MIB] [--status-listen ADDR]
        [--move-listen SOCKET] [--server-endpoint ADDR:PORT]
        (--jail-helper PATH [--jail-config FILE] | --unjailed)
                         own this host's workspace VMs; listens on RUN/hosted.sock.
                         Each VM runs jailed, with a network, through the
                         privileged tilo-hosted-jail helper and its root-owned
                         configuration (default /etc/tilo/hosted-jail.json);
                         --unjailed is for development only: no network.
                         A jailed host keeps a template of each VM size in use
                         under STATE/templates, booted once with the session's
                         and Chromium's files in its page cache, and starts
                         each new VM as a copy-on-write clone of it.
                         When FILE appears (written by whatever watches the
                         cloud's notice that it takes this host back), every
                         VM is suspended onto STATE and no more are woken.
                         --memory-mib is the node's memory its VMs are packed
                         into (the whole node by default): admission counts
                         what each VM really takes, as the kernel measures its
                         processes, not the size it was configured at.
                         --memory-reserve-mib (2048 by default) is what
                         stays free for the host itself, and
                         --memory-headroom-mib (128) is added to what a VM is
                         counted at until it has been measured. A wake past
                         that waits; when the node's free memory falls under
                         the reserve, the VMs' balloons are asked for memory
                         back before any wake is refused.
                         --status-listen answers Status alone over TCP, for a
                         placer.
                         --move-listen accepts what another machine sends over
                         SOCKET: a VM that moves to this machine, or a replica copy
                         of one it is kept for. Same user only.
                         --server-endpoint serves the workspaces' Browser in
                         their VMs: ADDR:PORT is where the browser session in a
                         VM reaches the server, the one address besides the
                         internet a VM may reach: the server's public address,
                         or its cluster address when the server is not
                         reachable from outside, or a loopback address when
                         the server runs on the host itself.
  status --run DIR       every VM, its phase and its last wake
  place --listen ADDR
        (--statefulset NAMESPACE/NAME --status-port PORT --max N
         | --hosts NAME=ADDR[,NAME=ADDR])
                         say which host takes a workspace's VM nobody keeps
                         (the first with room), and start hosts, the
                         StatefulSet's pods NAME-0, NAME-1, …, as their VMs fill
                         them: past three quarters, or for any VM waiting.
                         --hosts names a stack's hosts and where each answers
                         Status, for a stack that runs a known set of machines
                         and starts none
  fetch-guest --images DIR [--ref REF]
                         fetch the pinned guest image into DIR and print the
                         kernel's and the root filesystem's paths
  usage --run DIR --account ID [--from MS] [--to MS]
                         the account's usage records and totals, as JSON
  run --run DIR --workspace ID --account ID [--environment ID] [--size SIZE]
      -- PROGRAM [ARG...]
                         wake the workspace's VM as the account and run one
                         program in it, as the tenant, its output relayed; exits
                         with its status. For operators checking a host
  suspend --run DIR WORKSPACE ENVIRONMENT
  stop --run DIR WORKSPACE ENVIRONMENT
  branch --run DIR --workspace ID --account ID --from WORKSPACE [--environment ID]
         [--size SIZE]
                         make the workspace's VM from the disk of FROM's VM as it
                         is now (its tools, its tenant's home), in the account
                         FROM's VM is the account's; it boots cold and forgets
                         FROM's sessions before its first one
  delete --run DIR WORKSPACE ENVIRONMENT
                         stop the workspace's VM and remove its disk and
                         snapshot; it no longer counts toward the account's VMs
  drain                  take no more VMs and suspend every awake one onto the
                         state directory, for a rolling update; the cloud's
                         eviction notice does the same
  move-vm WORKSPACE ENVIRONMENT --to SOCKET
                         move the workspace's VM to the machine serving SOCKET: it is
                         suspended, its disk and snapshot streamed there, and that
                         machine forgets it once this one has all of it
  replicate WORKSPACE ENVIRONMENT --to SOCKET
                         copy the workspace's VM to the replica machine serving SOCKET,
                         only the blocks that changed since its last copy
  browser-open --run DIR --workspace ID --account ID [--size SIZE] [--store DIR]
               [--material JSON]
                         open or attach the browser session in a workspace's
                         VM, and say what it took
  browser-stop --run DIR WORKSPACE
                         end the browser session in a workspace's VM now
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
                        size: vm_size(&flags)?,
                        store: flags.get("--store").map(PathBuf::from),
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
        "branch" | "delete" => branch_or_delete(command, &Flags::parse(rest, &[])?),
        "drain" => ask(&Flags::parse(rest, &[])?, &Request::Drain),
        "move-vm" | "replicate" => move_or_replicate(command, &Flags::parse(rest, &[])?),
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

/// Make a workspace's VM a branch of another's, or delete one.
fn branch_or_delete(command: &str, flags: &Flags) -> Result<ExitCode, String> {
    let request = if command == "branch" {
        Request::Branch {
            vm: control::VmRequest {
                workspace: flags.need("--workspace")?.to_string(),
                environment: flags.get("--environment").unwrap_or("default").to_string(),
                account: flags.need("--account")?.to_string(),
                size: vm_size(flags)?,
            },
            from: flags.need("--from")?.to_string(),
        }
    } else {
        let [workspace, environment] = flags.rest.as_slice() else {
            return Err("delete: WORKSPACE ENVIRONMENT".to_string());
        };
        Request::Delete {
            workspace: workspace.clone(),
            environment: environment.clone(),
        }
    };
    ask(flags, &request)
}

/// Move a workspace's VM to another machine, or copy it to a replica machine.
fn move_or_replicate(command: &str, flags: &Flags) -> Result<ExitCode, String> {
    let [workspace, environment] = flags.rest.as_slice() else {
        return Err(format!("{command}: WORKSPACE ENVIRONMENT\n{USAGE}"));
    };
    let to = flags.path("--to")?;
    let (workspace, environment) = (workspace.clone(), environment.clone());
    let request = if command == "move-vm" {
        Request::MoveVm {
            workspace,
            environment,
            to,
        }
    } else {
        Request::ReplicateVm {
            workspace,
            environment,
            to,
        }
    };
    ask(flags, &request)
}

/// Fetch the pinned guest image and say what a VM boots: the kernel's path and the root
/// filesystem's path, one per line.
fn fetch_guest(flags: &Flags) -> Result<ExitCode, String> {
    let images = flags.path("--images")?;
    let reference = guest::reference(flags.get("--ref")).map_err(|e| e.to_string())?;
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
            // The browser session in a VM joins its workspace through the server, inside the host's
            // network: the one address besides the internet a VM may reach.
            server: flags
                .get("--server-endpoint")
                .map(linux_vm::hosted::network::Endpoint::parse)
                .transpose()?,
            // New VMs are clones of a template of their size, kept beside them: only a jailed VM
            // can be one.
            templates: Some(flags.path("--state")?.join("templates")),
        },
    };
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        let handler = on_signal as extern "C" fn(libc::c_int) as *const () as libc::sighandler_t;
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
    let mut service = Service::open(config, system_clock()).map_err(|e| e.to_string())?;
    let budget = memory_budget(flags)?;
    service = service.with_memory(budget);
    eprintln!(
        "tilo-linux-vm hosted: admitting VMs by what they take: {} for them of the node's {}, \
         {} kept for this host, and a new VM counted at {} MiB until it is measured",
        budget
            .for_vms_mib()
            .map_or_else(|| "no limit".to_owned(), |mib| format!("{mib} MiB")),
        budget
            .node_mib
            .map_or_else(|| "unknown".to_owned(), |mib| format!("{mib} MiB")),
        budget.reserve_mib,
        budget.expected_mib(),
    );
    let service = Arc::new(service);
    if let Some(socket) = flags.get("--move-listen") {
        service
            .serve_moves(std::path::Path::new(socket))
            .map_err(|e| format!("--move-listen {socket}: {e}"))?;
    }
    if let Some(address) = flags.get("--status-listen") {
        let listener = std::net::TcpListener::bind(address)
            .map_err(|e| format!("--status-listen {address}: {e}"))?;
        service.serve_status(listener);
    }
    eprintln!(
        "tilo-linux-vm hosted: serving {}",
        service.config().socket().display()
    );
    let evict_notice = flags.get("--evict-notice").map(PathBuf::from);
    service
        .serve(&STOP, evict_notice.as_deref())
        .map_err(|e| e.to_string())?;
    Ok(ExitCode::SUCCESS)
}

fn memory_budget(flags: &Flags) -> Result<linux_vm::hosted::memory::Budget, String> {
    let mib = |name: &str, default: u64| -> Result<u64, String> {
        flags.get(name).map_or(Ok(default), |mib| {
            mib.parse::<u64>()
                .map_err(|_| format!("{name} takes mebibytes"))
        })
    };
    Ok(linux_vm::hosted::memory::Budget {
        node_mib: flags
            .get("--memory-mib")
            .map(|mib| {
                mib.parse::<u64>()
                    .map_err(|_| "--memory-mib takes mebibytes".to_string())
            })
            .transpose()?,
        reserve_mib: mib(
            "--memory-reserve-mib",
            linux_vm::hosted::memory::Budget::DEFAULT_RESERVE_MIB,
        )?,
        headroom_mib: mib(
            "--memory-headroom-mib",
            linux_vm::hosted::memory::Budget::DEFAULT_HEADROOM_MIB,
        )?,
    })
}

fn vm_size(flags: &Flags) -> Result<linux_vm::hosted::size::VmSize, String> {
    use linux_vm::hosted::size::VmSize;
    flags.get("--size").map_or(Ok(VmSize::Small), |name| {
        VmSize::parse(name).ok_or_else(|| format!("no VM size {name:?}"))
    })
}

/// The VM hosts as a Kubernetes `StatefulSet`: its replicas, read and set through the API server
/// with the pod's service account.
struct StatefulSetScale {
    namespace: String,
    name: String,
    port: String,
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

impl linux_vm::hosted::placement::Scale for StatefulSetScale {
    fn hosts(&self) -> Result<Vec<Host>, String> {
        let scale = self.call("GET", None)?;
        let count = scale
            .get("spec")
            .and_then(|spec| spec.get("replicas"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(|| format!("no replicas in {scale}"))?;
        Ok((0..count)
            .map(|index| {
                let (namespace, name, port) = (&self.namespace, &self.name, &self.port);
                Host::new(
                    &format!("{name}-{index}"),
                    &format!("{name}-{index}.{name}.{namespace}.svc:{port}"),
                )
            })
            .collect())
    }

    fn set_hosts(&self, count: usize) -> Result<(), String> {
        eprintln!("tilo-linux-vm hosted place: starting hosts up to {count}");
        self.call(
            "PATCH",
            Some(&format!(r#"{{"spec":{{"replicas":{count}}}}}"#)),
        )
        .map(|_| ())
    }
}

/// Place VMs on the hosts the environment names: a `StatefulSet`'s, which it scales, or the named
/// hosts of a stack that runs a known set of machines, which it never starts.
fn place(flags: &Flags) -> Result<ExitCode, String> {
    use linux_vm::hosted::placement::{FixedHosts, Placer, Scale, read_load};

    let scale: Box<dyn Scale> = match (flags.get("--statefulset"), flags.get("--hosts")) {
        (Some(statefulset), None) => {
            let (namespace, name) = statefulset
                .split_once('/')
                .ok_or("--statefulset takes NAMESPACE/NAME")?;
            Box::new(StatefulSetScale {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
                port: flags.need("--status-port")?.to_owned(),
            })
        }
        (None, Some(hosts)) => Box::new(FixedHosts::parse(hosts)?),
        _ => {
            return Err(format!(
                "place names its hosts once: --statefulset NAMESPACE/NAME --status-port PORT, or \
                 --hosts NAME=ADDR[,NAME=ADDR]\n{USAGE}"
            ));
        }
    };
    let max: usize = match flags.get("--max") {
        Some(max) => max.parse().map_err(|_| "--max takes a count".to_string())?,
        None if flags.get("--hosts").is_some() => scale.hosts()?.len(),
        None => return Err("--statefulset needs --max (the most hosts to start)".to_string()),
    };
    let hosts = scale.hosts().map_or_else(
        |_| "them".to_owned(),
        |hosts| {
            hosts
                .iter()
                .map(|host| host.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        },
    );
    let placer = Arc::new(Placer::new(max, scale, Box::new(read_load), system_clock()));
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
    eprintln!("tilo-linux-vm hosted place: placing on {hosts}, at most {max}, on {address}");
    while !STOP.load(Ordering::SeqCst) {
        placer.rescale();
        std::thread::sleep(Duration::from_secs(10));
    }
    Ok(ExitCode::SUCCESS)
}

/// Wake a workspace's VM and run one program in it, as ush would.
fn run(args: &[String]) -> Result<ExitCode, String> {
    use linux_vm::hosted::control::VmRequest;
    use linux_vm_protocol::client::{self, LaunchIo};
    use linux_vm_protocol::{AGENT_PORT, Display, Launch, Stdio};

    let mut parts = args.splitn(2, |arg| arg == "--");
    let options = parts.next().unwrap_or_default();
    let program = parts.next().unwrap_or_default();
    if program.is_empty() {
        return Err(format!("run: the program follows --\n{USAGE}"));
    }
    let flags = Flags::parse(options, &[])?;
    let size = vm_size(&flags).map_err(|error| format!("run: {error}"))?;
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
                    eprintln!("tilo-linux-vm hosted: {message}");
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
                eprintln!("tilo-linux-vm hosted: the VM woke in {ms} ms");
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
            Err(error) => return Err(format!("accept: {error}")),
        };
        let shares = shares.clone();
        std::thread::spawn(move || {
            if let Err(error) = files::serve(stream, &shares) {
                eprintln!("tilo-linux-vm hosted files: {error}");
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
                "memory: {} MiB in use of {}, {} MiB waiting",
                load.awake_mib,
                load.budget_mib
                    .map_or_else(|| "no budget".to_owned(), |mib| format!("{mib} MiB")),
                load.waiting_mib
            );
            for vm in vms {
                println!(
                    "{:<40} {:<8} {:<10} {:<10} {:<9} wake {} snapshot {}",
                    vm.vm,
                    vm.size,
                    vm.phase,
                    vm.account,
                    vm.resident_mib.map_or_else(
                        || "-".to_string(),
                        |mib| format!(
                            "{mib} MiB{}",
                            if vm.balloon_mib == 0 {
                                String::new()
                            } else {
                                format!(" -{} balloon", vm.balloon_mib)
                            }
                        )
                    ),
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
        Reply::Opened {
            opened_ms,
            provider,
        } => {
            println!(
                "{} {}",
                opened_ms.map_or_else(|| "attached".to_string(), |ms| format!("opened in {ms} ms")),
                provider,
            );
            Ok(ExitCode::SUCCESS)
        }
        Reply::Placed { host } => {
            println!("{host}");
            Ok(ExitCode::SUCCESS)
        }
        Reply::Moved { vm, bytes } => {
            println!("{vm}: {bytes} bytes moved");
            Ok(ExitCode::SUCCESS)
        }
        Reply::Refused(refusal) => Err(refusal.message),
        Reply::Busy(message) | Reply::Error(message) => Err(message),
        Reply::NoVm { workspace } => Err(format!(
            "workspace {workspace} has no Linux VM yet, so there is nothing to branch"
        )),
    }
}
