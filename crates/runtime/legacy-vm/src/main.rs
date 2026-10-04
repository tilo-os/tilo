//! `tilo-legacy`: run and install legacy apps in the user's legacy VM.

use std::io::IsTerminal as _;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use legacy_agent_protocol::client::{self, LaunchIo};
use legacy_agent_protocol::link::AgentLink;
use legacy_agent_protocol::waypipe::PORT_ENV;
use legacy_agent_protocol::{
    AGENT_PORT, Display, GuestMessage, GuestOs, HostMessage, Launch, Stdio as LaunchStdio,
};
use legacy_vm::apps::{self, Registry};
use legacy_vm::config::{Paths, VmConfig};
use legacy_vm::control::{self, ControlReply, ControlRequest};
use legacy_vm::grants::Grants;
use legacy_vm::supervisor::Supervisor;
use legacy_vm::{confine, image, virtiofs};

const USAGE: &str = "\
usage: tilo-legacy [--guest linux] [--image QCOW2] [--idle-suspend SECONDS] <command>

  launch <app> [--cwd DIR] [--share-cwd] [ARG…]
                         run an installed app; its windows open in the current strip
  run [--cwd DIR] [--share-cwd] [--no-window] -- COMMAND [ARG…]
                         run any command in the VM, with stdio relayed
  install <flatpak-ref>  install an app from Flathub into the VM
  uninstall <app>        remove an installed app
  apps                   list installed apps and their commands
  grant <dir> [--read-only]
                         share a directory with the VM at the same path
  revoke <dir>           stop sharing a directory
  grants                 list shared directories
  status                 what the VM is doing
  suspend                save the VM to disk now
  stop                   shut the VM down
  image fetch            fetch the pinned guest image
";

struct Options {
    guest: GuestOs,
    image: Option<PathBuf>,
    /// How long the VM may sit with nothing running before it is saved to disk.
    idle_suspend: Option<Duration>,
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut options = Options {
        guest: GuestOs::Linux,
        image: None,
        idle_suspend: None,
    };
    while let [flag, value, ..] = args.as_slice() {
        match flag.as_str() {
            "--guest" => {
                let Some(guest) = GuestOs::parse(value) else {
                    return fail(&format!("unknown guest {value:?}"));
                };
                options.guest = guest;
            }
            "--image" => options.image = Some(PathBuf::from(value)),
            "--idle-suspend" => {
                let Ok(seconds) = value.parse::<u64>() else {
                    return fail(&format!("--idle-suspend takes seconds, not {value:?}"));
                };
                options.idle_suspend = Some(Duration::from_secs(seconds.max(1)));
            }
            _ => break,
        }
        args.drain(..2);
    }
    if options.guest == GuestOs::Windows {
        return fail(
            "the Windows legacy VM is the Windows appliance; manage it from Settings › Windows apps",
        );
    }
    let Some(command) = args.first().cloned() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let rest = args.split_off(1);
    if command == "confine-fs" {
        let Some((ro, rw, program)) = virtiofs::parse_confined(&rest) else {
            return fail("confine-fs: --ro DIR… --rw DIR… -- PROGRAM…");
        };
        return fail(&confine::exec_confined(&ro, &rw, &program));
    }
    let Some(paths) = Paths::for_user(options.guest) else {
        return fail("no XDG runtime directory; a legacy VM belongs to a login session");
    };
    let result = match command.as_str() {
        "launch" => launch_app(&options, &paths, &rest),
        "run" => run_command(&options, &paths, &rest),
        "install" => install(&options, &paths, &rest),
        "uninstall" => uninstall(&options, &paths, &rest),
        "apps" => Ok(list_apps(&paths)),
        "grant" => grant(&paths, &rest),
        "revoke" => revoke(&paths, &rest),
        "grants" => Ok(list_grants(&paths)),
        "status" => status(&options, &paths),
        "suspend" => suspend(&paths),
        "stop" => stop(&options, &paths),
        "image" if rest.first().map(String::as_str) == Some("fetch") => fetch_image(&paths),
        "serve" => serve(&options, paths),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command {other:?}\n{USAGE}")),
    };
    result.unwrap_or_else(|message| fail(&message))
}

fn fail(message: &str) -> ExitCode {
    eprintln!("tilo-legacy: {message}");
    ExitCode::FAILURE
}

fn config(options: &Options, paths: Paths) -> VmConfig {
    let mut config = VmConfig::new(
        options.guest,
        options.image.clone().unwrap_or_default(),
        paths,
    );
    if let Some(idle) = options.idle_suspend {
        config.idle_suspend = idle;
    }
    config
}

/// Ask the supervisor to wake the VM, starting one when none runs.
fn ensure(
    options: &Options,
    paths: &Paths,
    extra: Vec<PathBuf>,
) -> Result<(AgentLink, u32), String> {
    let request = ControlRequest::Ensure { extra };
    let socket = paths.supervisor_socket();
    let mut reply = control::ask(&socket, &request).map_err(|e| e.to_string())?;
    if reply.is_none() {
        spawn_supervisor(options, paths)?;
        reply = control::ask_within(&socket, &request, Duration::from_secs(10))
            .map_err(|e| e.to_string())?;
    }
    match reply {
        Some(ControlReply::Ready { link, waypipe_port }) => Ok((link, waypipe_port)),
        Some(ControlReply::Error(message)) => Err(message),
        Some(other) => Err(format!("unexpected supervisor reply {other:?}")),
        None => Err(format!(
            "the VM supervisor did not start; see {}",
            paths.supervisor_log().display()
        )),
    }
}

fn spawn_supervisor(options: &Options, paths: &Paths) -> Result<(), String> {
    std::fs::create_dir_all(&paths.runtime).map_err(|e| e.to_string())?;
    let log = std::fs::File::create(paths.supervisor_log()).map_err(|e| e.to_string())?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut command = Command::new(exe);
    command.args(["--guest", options.guest.as_str()]);
    if let Some(image) = &options.image {
        command.arg("--image").arg(image);
    }
    if let Some(idle) = options.idle_suspend {
        command
            .arg("--idle-suspend")
            .arg(idle.as_secs().to_string());
    }
    command
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    // SAFETY: `setsid` is async-signal-safe; the supervisor outlives the
    // terminal or broker that started it and must not get its hangup.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn().map(drop).map_err(|e| e.to_string())
}

fn serve(options: &Options, paths: Paths) -> Result<ExitCode, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let image = options.image.clone();
    Supervisor::new(config(options, paths), image, exe)
        .run()
        .map(|()| ExitCode::SUCCESS)
        .map_err(|e| e.to_string())
}

struct LaunchArgs {
    cwd: Option<PathBuf>,
    share_cwd: bool,
    windowed: bool,
    rest: Vec<String>,
}

fn launch_args(args: &[String]) -> Result<LaunchArgs, String> {
    let mut parsed = LaunchArgs {
        cwd: std::env::current_dir().ok(),
        share_cwd: false,
        windowed: true,
        rest: Vec::new(),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cwd" => {
                parsed.cwd = Some(PathBuf::from(args.next().ok_or("--cwd needs a directory")?));
            }
            "--share-cwd" => parsed.share_cwd = true,
            "--no-window" => parsed.windowed = false,
            "--" => {
                parsed.rest.extend(args.cloned());
                break;
            }
            _ => parsed.rest.push(arg.clone()),
        }
    }
    Ok(parsed)
}

fn launch_app(options: &Options, paths: &Paths, args: &[String]) -> Result<ExitCode, String> {
    let (app, args) = args.split_first().ok_or("launch: which app?")?;
    let registry = Registry::load(&paths.apps());
    let Some(app) = registry.find(app) else {
        return Err(format!(
            "{app} is not installed in the {} VM; install it with `tilo-legacy install <flatpak-ref>`",
            options.guest.as_str()
        ));
    };
    let parsed = launch_args(args)?;
    let mut command = vec!["flatpak".to_string(), "run".to_string(), app.id.clone()];
    command.extend(parsed.rest.iter().cloned());
    run_in_guest(options, paths, command, &parsed, true)
}

fn run_command(options: &Options, paths: &Paths, args: &[String]) -> Result<ExitCode, String> {
    let parsed = launch_args(args)?;
    if parsed.rest.is_empty() {
        return Err("run: no command given".to_string());
    }
    let windowed = parsed.windowed;
    run_in_guest(options, paths, parsed.rest.clone(), &parsed, windowed)
}

fn run_in_guest(
    options: &Options,
    paths: &Paths,
    argv: Vec<String>,
    parsed: &LaunchArgs,
    windowed: bool,
) -> Result<ExitCode, String> {
    let extra = match (&parsed.cwd, parsed.share_cwd) {
        (Some(cwd), true) => vec![cwd.clone()],
        _ => Vec::new(),
    };
    let (link, port) = ensure(options, paths, extra)?;
    let mut env = vec![(PORT_ENV.to_string(), port.to_string())];
    for name in ["LANG", "LC_ALL", "TERM", "COLORTERM"] {
        if let Ok(value) = std::env::var(name) {
            env.push((name.to_string(), value));
        }
    }
    let launch = Launch {
        argv,
        cwd: parsed.cwd.as_ref().map(|cwd| cwd.display().to_string()),
        env,
        stdio: LaunchStdio::Relay,
        display: if windowed {
            Display::Forwarded
        } else {
            Display::None
        },
    };
    let stream = link
        .connect(AGENT_PORT)
        .map_err(|e| format!("guest agent: {e}"))?;
    // A windowed app started from search has no terminal to read from.
    let stdin: Option<Box<dyn std::io::Read + Send>> =
        if !windowed || std::io::stdin().is_terminal() {
            Some(Box::new(std::io::stdin()))
        } else {
            None
        };
    let (mut stdout, mut stderr) = (std::io::stdout(), std::io::stderr());
    let status = client::launch(
        stream,
        &launch,
        LaunchIo {
            stdin,
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

fn agent_request(
    options: &Options,
    paths: &Paths,
    message: &HostMessage,
) -> Result<GuestMessage, String> {
    let (link, _) = ensure(options, paths, Vec::new())?;
    let mut stream = link.connect(AGENT_PORT).map_err(|e| e.to_string())?;
    client::request(&mut stream, message).map_err(|e| e.to_string())
}

fn refresh_apps(options: &Options, paths: &Paths) -> Result<Registry, String> {
    let GuestMessage::InstalledApps(installed) =
        agent_request(options, paths, &HostMessage::InstalledApps)?
    else {
        return Err("the guest did not list its apps".to_string());
    };
    let registry = Registry::from_guest(options.guest, &installed, &[]);
    registry.save(&paths.apps()).map_err(|e| e.to_string())?;
    if let Some(dir) = apps::applications_dir() {
        apps::sync_desktop_entries(&dir, options.guest, &registry).map_err(|e| e.to_string())?;
    }
    Ok(registry)
}

fn flatpak(options: &Options, paths: &Paths, argv: &[&str]) -> Result<ExitCode, String> {
    let parsed = LaunchArgs {
        cwd: None,
        share_cwd: false,
        windowed: false,
        rest: Vec::new(),
    };
    run_in_guest(
        options,
        paths,
        argv.iter().map(ToString::to_string).collect(),
        &parsed,
        false,
    )
}

fn install(options: &Options, paths: &Paths, args: &[String]) -> Result<ExitCode, String> {
    let [reference] = args else {
        return Err("install: one Flathub ref, e.g. org.gnome.Calculator".to_string());
    };
    if reference.starts_with('-')
        || !reference
            .split('/')
            .all(|part| part.is_empty() || apps::valid_app_id(part))
    {
        return Err(format!("{reference:?} is not a Flatpak ref"));
    }
    let code = flatpak(
        options,
        paths,
        &[
            "flatpak",
            "install",
            "--system",
            "--noninteractive",
            "-y",
            "flathub",
            reference,
        ],
    )?;
    let registry = refresh_apps(options, paths)?;
    for app in &registry.apps {
        if reference.starts_with(&app.id) {
            println!(
                "{} is available as `{}` and in search",
                app.name, app.command
            );
        }
    }
    Ok(code)
}

fn uninstall(options: &Options, paths: &Paths, args: &[String]) -> Result<ExitCode, String> {
    let [name] = args else {
        return Err("uninstall: which app?".to_string());
    };
    let registry = Registry::load(&paths.apps());
    let id = registry
        .find(name)
        .map_or_else(|| name.clone(), |app| app.id.clone());
    let code = flatpak(
        options,
        paths,
        &[
            "flatpak",
            "uninstall",
            "--system",
            "--noninteractive",
            "-y",
            &id,
        ],
    )?;
    refresh_apps(options, paths)?;
    Ok(code)
}

fn list_apps(paths: &Paths) -> ExitCode {
    for app in Registry::load(&paths.apps()).apps {
        println!("{:<20} {:<40} {}", app.command, app.id, app.name);
    }
    ExitCode::SUCCESS
}

fn grant(paths: &Paths, args: &[String]) -> Result<ExitCode, String> {
    let (dir, flags) = args.split_first().ok_or("grant: which directory?")?;
    let read_only = flags.iter().any(|flag| flag == "--read-only");
    let dir = std::fs::canonicalize(dir).map_err(|e| format!("{dir}: {e}"))?;
    let mut grants = Grants::load(&paths.grants());
    grants
        .grant(&dir, read_only)
        .ok_or_else(|| format!("{} cannot be shared with a VM", dir.display()))?;
    grants.save(&paths.grants()).map_err(|e| e.to_string())?;
    resync(paths);
    println!(
        "{} is shared {} at the same path",
        dir.display(),
        if read_only { "read-only" } else { "read-write" }
    );
    Ok(ExitCode::SUCCESS)
}

fn revoke(paths: &Paths, args: &[String]) -> Result<ExitCode, String> {
    let [dir] = args else {
        return Err("revoke: which directory?".to_string());
    };
    let resolved =
        std::fs::canonicalize(dir).map_or_else(|_| dir.clone(), |p| p.display().to_string());
    let mut grants = Grants::load(&paths.grants());
    if !grants.revoke(&resolved) && !grants.revoke(dir) {
        return Err(format!("{dir} is not shared"));
    }
    grants.save(&paths.grants()).map_err(|e| e.to_string())?;
    resync(paths);
    Ok(ExitCode::SUCCESS)
}

/// A running VM picks grant changes up at once; a sleeping one on waking.
fn resync(paths: &Paths) {
    let socket = paths.supervisor_socket();
    if let Ok(Some(ControlReply::Status(status))) = control::ask(&socket, &ControlRequest::Status)
        && status.phase == "Running"
    {
        let _ = control::ask(&socket, &ControlRequest::Ensure { extra: Vec::new() });
    }
}

fn list_grants(paths: &Paths) -> ExitCode {
    for share in Grants::load(&paths.grants()).shares(None) {
        let mode = if share.read_only {
            "read-only"
        } else {
            "read-write"
        };
        println!("{:<11} {}", mode, share.path.display());
    }
    ExitCode::SUCCESS
}

fn status(options: &Options, paths: &Paths) -> Result<ExitCode, String> {
    let reply = control::ask(&paths.supervisor_socket(), &ControlRequest::Status)
        .map_err(|e| e.to_string())?;
    if let Some(ControlReply::Status(report)) = reply {
        println!("phase:   {}", report.phase);
        println!("vsock:   {}", report.transport);
        if let Some(running) = report.running {
            println!("running: {running}");
        }
        if let Some(memory) = report.memory_mib {
            println!("memory:  {memory} MiB");
        }
        for share in report.shares {
            println!("share:   {share}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let config = config(options, paths.clone());
    let virsh = legacy_vm::virsh::Virsh::new(config.domain_name);
    let state = virsh.state().map_err(|e| e.to_string())?;
    let saved =
        state != libvirt_domain::DomainState::Absent && virsh.has_managed_save().unwrap_or(false);
    let phase = match state {
        libvirt_domain::DomainState::Absent if !paths.disk().exists() => "not created",
        _ if saved => "suspended",
        libvirt_domain::DomainState::Running => "running (no supervisor)",
        _ => "off",
    };
    println!("phase:   {phase}");
    println!(
        "vsock:   {}",
        legacy_vm::transport::VsockBackend::detect()
            .map_or("none", legacy_vm::transport::VsockBackend::as_str)
    );
    Ok(ExitCode::SUCCESS)
}

fn suspend(paths: &Paths) -> Result<ExitCode, String> {
    match control::ask(&paths.supervisor_socket(), &ControlRequest::Suspend)
        .map_err(|e| e.to_string())?
    {
        Some(ControlReply::Error(message)) => Err(message),
        _ => Ok(ExitCode::SUCCESS),
    }
}

fn stop(options: &Options, paths: &Paths) -> Result<ExitCode, String> {
    let config = config(options, paths.clone());
    let virsh = legacy_vm::virsh::Virsh::new(config.domain_name);
    if virsh.state().map_err(|e| e.to_string())? == libvirt_domain::DomainState::Running {
        virsh.shutdown().map_err(|e| e.to_string())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn fetch_image(paths: &Paths) -> Result<ExitCode, String> {
    let reference =
        image::current_ref().ok_or_else(|| image::ImageError::NotPublished.to_string())?;
    let base = image::ensure_base(&paths.images, &reference).map_err(|e| e.to_string())?;
    println!("{}", base.display());
    Ok(ExitCode::SUCCESS)
}
