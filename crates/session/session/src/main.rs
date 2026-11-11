//! The native session: `tilo-session [--backend drm|headless|nested] [--size WxH]
//! [--connector NAME] [--output [NAME=]WxH[@X,Y]]... [--scale S|--panel-mm WxH]
//! [--shell screen] [--host-desktop] [--admit-repl PATH]...
//! [--test-control PATH] [--admin-control PATH] [--seed-apps] [-- command args...]`.
//! It listens on a fresh `wayland-N` socket, serves X11 clients one display per
//! workspace (unless `--no-x11`), prints the socket and the agent door it serves,
//! and starts `command` with `WAYLAND_DISPLAY` set to it; with a command, the
//! session lasts as long as it does. A nested session serves no bus unless a flag
//! names one. SIGTERM, SIGINT and SIGHUP end the session as ending on its own does.

#[cfg(target_os = "linux")]
use tilo_session::desktop_services::{Bus, Buses};
#[cfg(target_os = "linux")]
use tilo_session::{Backend, Reach, SessionEnvironment};

/// Byte string stamped into a session built with `--features dev-session`, so
/// the package's developer entry can tell a deliberately built session from
/// any other executable before it starts one as the login session. A release
/// build never carries it.
///
/// `#[used]` keeps the linker from discarding a static nothing reads.
#[cfg(all(feature = "dev-session", debug_assertions))]
#[used]
static DEV_SESSION_BUILD_MARKER: [u8; 32] = *b"tilo-app-session-build-marker-v1";

/// Byte string stamped into a session built with `--features dev-overrides` in
/// a release profile: the dev image's session (os/Containerfile.dev, whose
/// components carry `dev-overrides`, os/components.toml), which opens the
/// test-control socket `tilo-vm` and the capture harnesses drive a desktop
/// through. The marker above is a debug build's and a release build never
/// carries it, so the dev image had nothing to prove that the session it ships
/// is the one it built rather than the base image's.
///
/// `#[used]` keeps the linker from discarding a static nothing reads.
#[cfg(all(feature = "dev-overrides", not(debug_assertions)))]
#[used]
static DEV_IMAGE_SESSION_BUILD_MARKER: [u8; 27] = *b"tilo-dev-image-session-v1--";

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    // A workspace this session joins is decoded in a worker process started from this executable;
    // that worker is served here and exits. This comes before anything here starts a thread, as
    // serving one requires.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return ExitCode::from(code);
    }
    // The earliest point the process can measure itself from: what the session's own startup marks
    // are read against.
    tilo_startup_metrics::init();
    let _startup_metrics = tilo_startup_metrics::ScopedDumper;
    tilo_startup_metrics::mark("main-start");
    // Everything this process reports, and the panic that would end it, goes through the one log
    // from here on — including the render service, which is this binary in its own mode.
    tilo_session_logging::install();
    // Libraries that parallelise on their own (message groups, text layout,
    // rendering) build rayon's process-wide pool on first use: build it here,
    // bounded, before a session thread reaches one.
    tilo_rt::bound_parallel_pool();

    let args: Vec<String> = std::env::args().skip(1).collect();
    // An app's workspace render service is this binary in its own mode: it opens no display and
    // starts no session (`tilo-render-service`).
    if let Some(code) = tilo_render_service::dispatch(&args) {
        return code;
    }
    let (ours, command) = split_command(&args);
    // `--firstboot` is the host package's wrapper hook, which goes on to the session it was
    // starting.
    let host_desktop = ours.iter().any(|arg| arg == "--host-desktop");
    if host_desktop {
        // A session installed on another distribution keeps its device identity where that login
        // can write, and every child it starts — the launcher, the brokers — reads the same tree.
        // This runs before anything here starts a thread, as it requires.
        match tilo_onboarding_host::identity_provisioning::configure_host_identity_root() {
            Ok(root) => eprintln!(
                "Tilo host session: device identity will be stored under {}",
                root.display()
            ),
            Err(error) => {
                eprintln!("tilo-session: cannot resolve host-session identity storage: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    // The desktop's tilolang surfaces start compiled once Cranelift compiled them in an earlier
    // session.
    if let Some(cache) = tilo_paths::xdg::cache_home()
        && let Err(error) = tilolang_tilo::runtime::engine::keep_compiled_in(
            cache.join("tilo").join("tilolang-compiled"),
        )
    {
        eprintln!("tilo-session: compiled tilolang modules are not kept: {error}");
    }
    if let Some(done) = first_boot(ours, host_desktop) {
        return done;
    }
    match tilo_session::startup::command(ours) {
        Err(said) => {
            eprintln!("tilo-session: {said}");
            return ExitCode::from(2);
        }
        Ok(Some(command)) => return tilo_session::startup::run(command, host_desktop),
        Ok(None) => {}
    }
    // The desk placing, drawing and answering a window is what a person waits
    // on when they open something, so this session carries the session's own
    // CPU and IO weight. The image sets it on the compositor's logind scope
    // before the session starts; a session a host started without those units,
    // or from a terminal, carries it itself, and a cgroup already weighted at
    // or above it is left alone (see `launcher::priority::Tier::Session`).
    if let Err(error) = launcher::app_cgroup::claim_session_weight() {
        eprintln!("tilo-session: keeping the desk's CPU weight: {error}");
    }
    let identity = tilo_session::startup::identity();
    tilo_startup_metrics::mark("identity-verified");
    match &identity {
        tilo_session::startup::Identity::Desk
        | tilo_session::startup::Identity::WaitForClock { .. } => {}
        tilo_session::startup::Identity::Recovery(problem) => {
            eprintln!(
                "tilo-session: this device's Tilo identity needs its owner: {}",
                problem.as_str()
            );
        }
        tilo_session::startup::Identity::Refused(said) => {
            eprintln!("tilo-session: this device's identity did not verify: {said}");
            return ExitCode::FAILURE;
        }
    }
    // Every installed app's surface, named before the session serves anything:
    // the person records it keeps are what a service writing one of them is
    // admitted against, and the filesystem daemon joins the person's own as it
    // starts beside the session, before any window of the Files app opened.
    tilo_app_launch::name_installed_surfaces();
    let Some(line) = command_line(ours) else {
        return ExitCode::FAILURE;
    };
    start(ours, command, line, host_desktop, &identity)
}

/// The session's own arguments and the command it starts, split at `--` .
#[cfg(target_os = "linux")]
fn split_command(args: &[String]) -> (&[String], &[String]) {
    match args.iter().position(|arg| arg == "--") {
        Some(split) => (
            args.get(..split).unwrap_or_default(),
            args.get(split.saturating_add(1)..).unwrap_or_default(),
        ),
        None => (args, &[][..]),
    }
}

/// The machine's first boot: setup runs and the process stops there, before any GPU, font or
/// session state is opened. What it returns is the code the process exits with when it stops here.
#[cfg(target_os = "linux")]
fn first_boot(ours: &[String], host_desktop: bool) -> Option<std::process::ExitCode> {
    use std::process::ExitCode;
    use tilo_session::firstboot::{self, OneShot};
    match firstboot::one_shot(ours) {
        Ok(Some(OneShot::SetupOnly)) => Some(match firstboot::run_setup(host_desktop) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("tilo-session: first boot setup failed: {error}");
                ExitCode::FAILURE
            }
        }),
        Ok(Some(OneShot::SetupThenSession)) => {
            if let Err(error) = firstboot::run_setup(host_desktop) {
                // A machine that cannot be prepared still boots: the person sees what is missing
                // rather than a login that never comes.
                eprintln!("tilo-session: first boot setup failed: {error}; starting anyway");
            }
            None
        }
        Ok(None) => None,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            Some(ExitCode::from(2))
        }
    }
}

/// What the command line asked of the session: the backend it runs on, the size of a headless or
/// nested one, and what it reaches of the desktop it runs in.
#[cfg(target_os = "linux")]
struct CommandLine {
    backend: Backend,
    size: (u16, u16),
    /// The scale a headless output draws at (`--scale`), or the physical size of its panel
    /// (`--panel-mm`), which decides the scale ([`tilo_compositor::panel_scale`]). `None` draws at
    /// 1.
    headless: HeadlessPanel,
    /// The connector a headless session's first output carries (`--connector`).
    connector: String,
    /// The displays a headless session opens after the first (`--output`).
    outputs: Vec<NamedOutput>,
    reach: Reach,
    /// Whether the tilolang shell runs, when the command line asked for it.
    shell: bool,
    /// The programs the person admits to act as them at the agent door
    /// (`--admit-repl PATH`, as often as they name one): their decision,
    /// made as they start the session.
    admit_repl: Vec<std::path::PathBuf>,
    /// Where the development test-control socket opens, when the command line named one and this
    /// build opens it.
    test_control: Option<std::path::PathBuf>,
    /// Where the owner's control socket opens, when this session serves one: the path the command
    /// line named, else the seat's own (`--headless-owner-control` opts a headless session in).
    admin_control: Option<std::path::PathBuf>,
    /// Whether the session publishes the records of the tilolang apps installed in its app store
    /// (`--seed-apps`, in a development build).
    seed_apps: bool,
}

/// Whether this account's identity is the one that claimed this device at its first
/// login — the fact that, with `wheel`, makes the session the device's owner and its
/// control door the person's own.
///
/// # Errors
///
/// When the claim or the identity exists but cannot be read: the answer is refused, never
/// guessed.
#[cfg(target_os = "linux")]
fn device_owner() -> std::io::Result<bool> {
    let identity_dir = runtime_identity::identity_dir_from_env()?;
    tilo_identity_host::provisioning::active_identity_owns_device(
        &identity_dir,
        &tilo_identity_host::provisioning::collaboration_identity_paths(),
    )
}

/// The density a headless output comes up at: a scale named outright, or the panel's physical size,
/// from which the scale is derived as a real connector's is.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq)]
enum HeadlessPanel {
    /// Physical pixels per logical pixel.
    Scale(f64),
    /// The panel's physical size across and down, in millimetres.
    Millimetres((u32, u32)),
}

/// The argument `flag` names on `ours` .
#[cfg(target_os = "linux")]
fn value<'a>(ours: &'a [String], flag: &str) -> Option<&'a str> {
    ours.iter()
        .position(|arg| arg == flag)
        .and_then(|index| ours.get(index.saturating_add(1)))
        .map(String::as_str)
}

/// Every argument `flag` names on `ours` , in order.
#[cfg(target_os = "linux")]
fn values<'a>(ours: &'a [String], flag: &str) -> Vec<&'a str> {
    ours.iter()
        .enumerate()
        .filter(|(_, arg)| *arg == flag)
        .filter_map(|(index, _)| ours.get(index.saturating_add(1)))
        .map(String::as_str)
        .collect()
}

/// A headless display `--output` names: its connector, its size in pixels,
/// and where its picture starts across and down the desktop.
#[cfg(target_os = "linux")]
type NamedOutput = (String, (u16, u16), Option<(i32, i32)>);

/// The headless display `value` names: `WxH` , `WxH@X,Y` , and `NAME=` before either to name the
/// connector, which is `HEADLESS-{index}` otherwise.
#[cfg(target_os = "linux")]
fn named_output(value: &str, index: usize) -> Option<NamedOutput> {
    let (name, rest) = match value.split_once('=') {
        Some((name, rest)) => (name.to_owned(), rest),
        None => (format!("HEADLESS-{index}"), value),
    };
    let (size, place) = match rest.split_once('@') {
        Some((size, place)) => {
            let (x, y) = place.split_once(',')?;
            (size, Some((x.parse().ok()?, y.parse().ok()?)))
        }
        None => (rest, None),
    };
    let (across, down) = size.split_once('x')?;
    Some((name, (across.parse().ok()?, down.parse().ok()?), place))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{named_output, parse_output};

    fn flags(extra: &[&str]) -> Vec<String> {
        std::iter::once("session")
            .chain(extra.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_headless_session_names_the_connector_of_its_first_output() {
        let output = parse_output(&flags(&[
            "--backend",
            "headless",
            "--connector",
            "eDP-1",
            "--output",
            "HDMI-A-1=1920x1080@1280,0",
        ]))
        .expect("parsed");
        assert_eq!(output.connector, "eDP-1");
        assert_eq!(
            output.outputs,
            vec![("HDMI-A-1".to_owned(), (1920, 1080), Some((1280, 0)))]
        );

        assert_eq!(
            parse_output(&flags(&["--backend", "headless"])).map(|output| output.connector),
            Some("HEADLESS-1".to_owned()),
            "the default connector"
        );
        assert!(
            parse_output(&flags(&["--backend", "nested", "--connector", "eDP-1"])).is_none(),
            "only a headless session's outputs have connectors of their own"
        );
    }

    #[test]
    fn an_output_names_its_size_place_and_connector() {
        let named = |value: &str| named_output(value, 2);
        assert_eq!(
            named("1280x800"),
            Some(("HEADLESS-2".to_owned(), (1280, 800), None))
        );
        assert_eq!(
            named("1920x1080@1280,0"),
            Some(("HEADLESS-2".to_owned(), (1920, 1080), Some((1280, 0))))
        );
        assert_eq!(
            named("DP-2=2560x1440@-2560,0"),
            Some(("DP-2".to_owned(), (2560, 1440), Some((-2560, 0)))),
            "a connector named outright, left of the first display"
        );
        assert_eq!(named("wide"), None);
        assert_eq!(named("1280"), None);
        assert_eq!(named("1280x800@middle"), None);
    }
}

/// The output `ours` asks for: its backend, the panel it draws, and whether the tilolang shell
/// runs.
#[cfg(target_os = "linux")]
struct Output {
    backend: Backend,
    /// The panel's size across and down, in logical pixels.
    size: (u16, u16),
    /// The density a headless output draws at.
    headless: HeadlessPanel,
    /// The connector a headless session's first output carries, so a desk can be posed as one whose
    /// panel is a named connector.
    connector: String,
    /// The displays a headless session opens after the first, each its own output where it is put.
    outputs: Vec<NamedOutput>,
    shell: bool,
}

#[cfg(target_os = "linux")]
fn parse_output(ours: &[String]) -> Option<Output> {
    let name = value(ours, "--backend").unwrap_or("drm");
    let Some(backend) = Backend::named(name) else {
        eprintln!("tilo-session: unknown backend {name} (drm, headless, nested)");
        return None;
    };
    let shell = match value(ours, "--shell") {
        None => false,
        Some("screen") => true,
        Some(other) => {
            eprintln!("tilo-session: unknown shell {other} (screen)");
            return None;
        }
    };
    let size: (u16, u16) = value(ours, "--size")
        .and_then(|size| size.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((1280, 720));
    // A headless output's density, as DRM takes its own from the connector: a scale named outright,
    // or the panel's physical size, from which the scale is derived. Without either it draws at 1.
    let headless = match (value(ours, "--scale"), value(ours, "--panel-mm")) {
        (Some(scale), _) => match scale.parse::<f64>() {
            Ok(scale) if scale.is_finite() && scale > 0.0 => HeadlessPanel::Scale(scale),
            _ => {
                eprintln!("tilo-session: --scale takes a positive number");
                return None;
            }
        },
        (None, Some(mm)) => {
            let Some(size) = millimetres(mm) else {
                eprintln!("tilo-session: --panel-mm takes WxH in whole millimetres");
                return None;
            };
            HeadlessPanel::Millimetres(size)
        }
        (None, None) => HeadlessPanel::Scale(1.0),
    };
    if !matches!(backend, Backend::Headless) && headless != HeadlessPanel::Scale(1.0) {
        eprintln!("tilo-session: --scale and --panel-mm are for a headless session");
    }
    let connector = match value(ours, "--connector") {
        None => "HEADLESS-1".to_owned(),
        Some(_) if !matches!(backend, Backend::Headless) => {
            eprintln!("tilo-session: --connector is for a headless session");
            return None;
        }
        Some("") => {
            eprintln!("tilo-session: --connector takes a connector name");
            return None;
        }
        Some(connector) => connector.to_owned(),
    };
    let asked = values(ours, "--output");
    let mut outputs = Vec::with_capacity(asked.len());
    for (index, asked) in asked.into_iter().enumerate() {
        let Some(output) = named_output(asked, index.saturating_add(2)) else {
            eprintln!("tilo-session: --output takes WxH, WxH@X,Y or NAME=WxH@X,Y, not {asked}");
            return None;
        };
        outputs.push(output);
    }
    if !outputs.is_empty() && !matches!(backend, Backend::Headless) {
        eprintln!("tilo-session: --output is for a headless session");
        return None;
    }
    Some(Output {
        backend,
        size,
        headless,
        connector,
        outputs,
        shell,
    })
}

/// The panel's physical size `--panel-mm` names: across and down, in whole millimetres.
#[cfg(target_os = "linux")]
fn millimetres(mm: &str) -> Option<(u32, u32)> {
    let (across, down) = mm.split_once('x')?;
    match (across.parse::<u32>(), down.parse::<u32>()) {
        (Ok(across), Ok(down)) if across > 0 && down > 0 => Some((across, down)),
        _ => None,
    }
}

/// The bus `flag` names, for the desktop this session runs in.
#[cfg(target_os = "linux")]
fn bus(ours: &[String], flag: &str) -> Bus {
    match value(ours, flag) {
        None => Bus::Default,
        Some("none") => Bus::None,
        Some(address) => Bus::Address(address.to_owned()),
    }
}

/// What a session in a window of another desktop reaches of it: no bus of it unless a flag names
/// one, and no displays exported to it.
#[cfg(target_os = "linux")]
fn reach(ours: &[String], backend: Backend) -> Reach {
    Reach::new(
        backend,
        Buses {
            session: bus(ours, "--session-bus"),
            system: bus(ours, "--system-bus"),
        },
    )
}

/// The programs the person admits to act as them at the agent door (`--admit-repl PATH`, as often
/// as they name one).
#[cfg(target_os = "linux")]
fn admit_repl(ours: &[String]) -> Vec<std::path::PathBuf> {
    ours.iter()
        .zip(ours.iter().skip(1))
        .filter(|(flag, _)| *flag == "--admit-repl")
        .map(|(_, program)| std::path::PathBuf::from(program))
        .collect()
}

/// Where the development test-control socket opens, when the command line named one and this build
/// opens it.
#[cfg(target_os = "linux")]
fn test_control(ours: &[String]) -> Option<std::path::PathBuf> {
    let named = value(ours, "--test-control");
    let control =
        tilo_session::test_control::path(tilo_session::test_control::OPENED_BY_THIS_BUILD, named);
    if named.is_some() && control.is_none() {
        eprintln!("tilo-session: --test-control is honoured by development builds only");
    }
    control
}

/// Where the owner's control socket opens, when this session serves one.
#[cfg(target_os = "linux")]
fn admin_control(ours: &[String], backend: Backend) -> Option<std::path::PathBuf> {
    // The owner's door: the seat's own for a session that owns the machine, the headless session's
    // own when it was asked to open one, and nothing for a session in another desktop's window.
    let named = ours
        .iter()
        .position(|arg| arg == "--admin-control")
        .and_then(|index| ours.get(index.saturating_add(1)))
        .map(std::path::PathBuf::from);
    let named = match named {
        Some(path) if !path.is_absolute() => {
            eprintln!("tilo-session: --admin-control takes an absolute path");
            None
        }
        named => named,
    };
    let headless_owner = ours.iter().any(|arg| arg == "--headless-owner-control");
    if headless_owner && backend != Backend::Headless {
        eprintln!("tilo-session: --headless-owner-control is for a headless session");
    }
    // The machine this session runs on, whether or not its account is the one that owns it.
    let this_machine = match backend {
        Backend::Headless => headless_owner,
        Backend::Nested => false,
        Backend::Drm => true,
    };
    // The owner's door is the device owner's: an account in `wheel` whose identity claimed the
    // device at its first login. A path named on the command line is the person's own say-so,
    // and a session in another desktop's window, or one nobody asked for an owner's door,
    // serves none.
    let owned = match named {
        Some(_) => this_machine,
        None if !this_machine => false,
        None => match tilo_session::test_control::owner_decision(
            os_actions::users::administers_this_device(),
            device_owner(),
        ) {
            Ok(()) => true,
            Err(refusal) => {
                eprintln!("tilo-session: serving no owner's control door: {refusal}");
                false
            }
        },
    };
    tilo_session::test_control::admin_path(owned, backend == Backend::Headless, named)
}

/// Whether the session publishes the seeded tilolang apps.
#[cfg(target_os = "linux")]
fn seed_apps(ours: &[String]) -> bool {
    let asked = ours.iter().any(|arg| arg == "--seed-apps");
    let seed = asked && tilo_session::seeded_apps::SEEDED_BY_THIS_BUILD;
    if asked && !seed {
        eprintln!("tilo-session: --seed-apps is honoured by development builds only");
    }
    seed
}

/// The command line `ours` parsed, or `None` when it names a backend there is not.
#[cfg(target_os = "linux")]
fn command_line(ours: &[String]) -> Option<CommandLine> {
    let output = parse_output(ours)?;
    Some(CommandLine {
        backend: output.backend,
        size: output.size,
        headless: output.headless,
        connector: output.connector,
        outputs: output.outputs,
        shell: output.shell,
        reach: reach(ours, output.backend),
        admit_repl: admit_repl(ours),
        test_control: test_control(ours),
        admin_control: admin_control(ours, output.backend),
        seed_apps: seed_apps(ours),
    })
}

/// The session `line` describes, run until `command` exits.
#[cfg(target_os = "linux")]
#[allow(
    clippy::too_many_lines,
    reason = "the session's startup reads in one place, from its services to the doors it opens"
)]
fn start(
    ours: &[String],
    command: &[String],
    line: CommandLine,
    host_desktop: bool,
    identity: &tilo_session::startup::Identity,
) -> std::process::ExitCode {
    use std::process::ExitCode;
    use std::sync::Arc;
    use tilo_session::Options;

    let CommandLine {
        backend,
        size,
        headless,
        connector,
        outputs,
        reach,
        shell,
        admit_repl,
        test_control,
        admin_control,
        seed_apps,
    } = line;
    let buses = reach.buses.clone();
    // A mark never waits for the disk from here on: the session is several threads already, and the
    // file is written beside them.
    tilo_startup_metrics::start_writer();
    tilo_startup_metrics::mark("services-start");
    // A session in a workspace's VM takes the device identity its host handed it before it
    // reads anything of itself: the workspace, the record whose window it opens and the key it acts
    // with are the relay's placement record, the first line of its standard input
    // (`docs/hosted-browser.md`, "What the session in the VM is handed").
    let hosted_browser = match tilo_session::hosted_browser::handed() {
        Ok(hosted) => hosted,
        Err(said) => {
            eprintln!("tilo-session: {said}");
            return ExitCode::FAILURE;
        }
    };
    let serves_an_environment = hosted_browser.is_some();
    // Who sits here, read now and kept current by workers: nothing the loop asks of the identity
    // directory waits on the disk.
    tilo_session::identity::install();
    tilo_startup_metrics::mark("identity-installed");
    // The font system every renderer shares is built on a thread from here: the services, the desk
    // build and the compositor's first frame run beside it, and what needs text finds it ready.
    // Building it on this thread instead put font discovery and face mapping (the font index alone
    // is a large read) in front of everything the session says to its first client.
    let _warmup = ui::warmup::spawn_warmup();
    let wake = tilo_service::LoopWake::default();
    // The person's settings, read once here: the power policy they resolve is part of them.
    let config = tilo_session::SessionConfig::load();
    tilo_startup_metrics::mark("settings-read");
    // The machine's power: that policy, applied by the session's loop and acted on over logind by
    // the service started beside the others. A headless session has no lid, keys or person at a
    // seat: it would act on the battery and idle of a machine whose own session does that.
    let power =
        (backend != Backend::Headless).then(|| tilo_session_power::PowerLink::new(config.power));
    // A session in a window of another desktop runs on that desktop's machine: its services stay
    // off the host's daemons, its device stays the host's, and no session action it takes reaches
    // the host.
    let machine = if matches!(backend, Backend::Nested) {
        tilo_session::Machine::Window
    } else {
        tilo_session::Machine::Owned
    };
    let (mut services, _following) = services(&wake, power.clone(), machine);
    let (capture, captures) =
        tilo_session::capture::channel_to(tilo_session::capture::directory(), wake.handle());
    // The portal's side of the desktop, which the session answers.
    let (portal_side, portal_link) = tilo_session::portal_link::channel(wake.handle());
    let desktop = desktop_services(
        &buses,
        &wake,
        Arc::new(capture),
        &portal_side,
        &mut services,
        power
            .as_ref()
            .map(|power_link| power_link.registry().clone()),
    );
    // Where input captures and the shared screens are is the portal's: its capture broker tells
    // them what the session's seat and outputs are.
    let portal = desktop
        .as_ref()
        .and_then(|desktop| desktop.portal())
        .map(|serving| portal_link.with_captures(serving.input_captures().clone()));
    tell_the_person(backend, desktop.as_deref());
    // The apps this session opens start from its own displays and the person's variables, once the
    // compositor has bound its socket.
    let launch = std::rc::Rc::new(tilo_session::apps::Apps::new(
        tilo_app_launch::SessionEnvironment::from_process(),
        Some(wake.handle()),
    ));
    // A drive the person plugs in is announced like any other news, and the eject the notice offers
    // goes over the system bus the desktop services already hold.
    let drives = drives(backend, desktop.as_ref(), &wake);
    tilo_startup_metrics::mark("services-done");
    let mut options = Options {
        services,
        wake,
        // A session in a workspace's VM is not a first use of a computer: its identity is the
        // one the relay handed it, so nothing is left to set up and nothing stands over the window
        // it was started for.
        setup: (!serves_an_environment)
            .then(|| tilo_session::startup::before_desk(identity, host_desktop))
            .flatten(),
        captures: Some(captures),
        questions: desktop.as_ref().map(|desktop| desktop.questions().clone()),
        apps: Some(std::rc::Rc::clone(&launch) as std::rc::Rc<dyn tilo_session::AppActions>),
        launch: Some(std::rc::Rc::clone(&launch)),
        drives,
        desktop,
        portal,
        power,
        machine,
        shell,
        admit_repl,
        test_control,
        admin_control,
        hosted_browser,
        ..device_options(machine, config)
    };
    // The person's files: the login's files daemon serves them as the Files app's windowless
    // endpoint, so the session records its process before the daemon's connections to the broker
    // arrive. A session in another desktop's window serves no files daemon of its own.
    if machine.is_owned() {
        tilo_session::files_daemon::record(
            Arc::clone(&options.launches),
            collab_types::personal_cluster_of(&tilo_session::identity::seated_person(&options))
                .map(|cluster| cluster.to_string()),
        );
    }
    // A headless session has no input device: nothing the person does can ever reset its idle
    // timers, so they would only blank and lock it.
    if backend == Backend::Headless {
        options.config.idle = tilo_compositor::IdleConfig::never();
    }
    if seed_apps {
        match tilo_session::seeded_apps::seeded_from_env(&tilo_session::identity::seated_person(
            &options,
        )) {
            Ok(apps) => options.services.push(Box::new(apps)),
            Err(error) => eprintln!("tilo-session: the seeded apps did not load: {error}"),
        }
        // A Rust app's sample resources and views, so a shell draws them live: a development shot
        // of a Rust view on the canvas builds with `dev-rust-views` .
        #[cfg(feature = "dev-rust-views")]
        match tilo_session::dev_rust_views::RustApps::new() {
            Ok(apps) => options.services.push(Box::new(apps)),
            Err(error) => tracing::warn!(%error, "the sample Rust apps did not open"),
        }
    }
    // A signal ends the loop rather than the process, so the session drops what it holds as it does
    // when it ends on its own.
    if let Err(error) = tilo_session::termination::install(options.wake.handle()) {
        eprintln!("tilo-session: a signal ends this session without its cleanup: {error}");
    }
    // The lock record is the login's, kept by the session that owns the machine: one in a window of
    // another desktop, or a headless one, keeps no record of its host's login. A record that cannot
    // be armed stops the login rather than start one that a crash would leave unlocked.
    if backend == Backend::Drm {
        match tilo_session::recovery::Recovery::from_environment() {
            Ok(recovery) => options.recovery = recovery,
            Err(error) => {
                eprintln!("tilo-session: cannot establish the login's lock recovery: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    let agent_door = options.agent_door.clone();
    let test_control = options.test_control.clone();
    let admin_control = options.admin_control.clone();
    tilo_startup_metrics::mark("session-open-start");
    let mut session = match start_session(backend, options, size, headless, &connector, &outputs) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            return ExitCode::FAILURE;
        }
    };
    watch_the_loop(backend, &mut session);
    let session_bus = match &buses.session {
        Bus::Address(address) => Some(address.as_str()),
        _ => None,
    };
    run(
        session,
        ours,
        command,
        Some(&launch),
        session_bus,
        reach.export_displays,
        host_desktop,
        &Doors {
            agent: agent_door.as_deref(),
            test_control: test_control.as_deref(),
            admin: admin_control.as_deref(),
        },
    )
}

/// The sockets the session bound as it started, printed for what drives it.
#[cfg(target_os = "linux")]
struct Doors<'a> {
    /// The agent door (`TILO_AGENT_DOOR=`).
    agent: Option<&'a std::path::Path>,
    /// The development test-control socket (`TILO_TEST_CONTROL=`).
    test_control: Option<&'a std::path::Path>,
    /// The owner's control socket (`TILO_ADMIN_CONTROL=`).
    admin: Option<&'a std::path::Path>,
}

/// The session on `backend` 's displays, at `size` where the backend runs in one.
#[cfg(target_os = "linux")]
fn start_session(
    backend: Backend,
    options: tilo_session::Options,
    size: (u16, u16),
    headless: HeadlessPanel,
    connector: &str,
    outputs: &[NamedOutput],
) -> Result<tilo_session::Session, tilo_session::Error> {
    use tilo_compositor::{DisplayConfig, NestedConfig, OutputConfig};

    use tilo_session::Session;
    match backend {
        Backend::Headless => {
            let output = OutputConfig::new(connector, i32::from(size.0), i32::from(size.1));
            // The scale a real connector starts at: the one named, or the one its panel's density
            // argues for, as DRM derives it.
            let output = match headless {
                HeadlessPanel::Scale(scale) => output.with_scale(scale),
                HeadlessPanel::Millimetres(mm) => output.panel(mm),
            };
            let mut displays = vec![output];
            for (name, (across, down), place) in outputs {
                let mut output =
                    OutputConfig::new(name.as_str(), i32::from(*across), i32::from(*down));
                output = match headless {
                    HeadlessPanel::Scale(scale) => output.with_scale(scale),
                    HeadlessPanel::Millimetres(mm) => output.panel(mm),
                };
                if let Some((x, y)) = place {
                    output = output.at(*x, *y);
                }
                displays.push(output);
            }
            Session::headless(options, &displays)
        }
        Backend::Drm => Session::drm(options, DisplayConfig::default()),
        Backend::Nested => Session::nested(
            options,
            &NestedConfig {
                size,
                ..NestedConfig::default()
            },
        ),
    }
}

/// `session` running until `command` exits, its displays printed.
#[cfg(target_os = "linux")]
fn run(
    mut session: tilo_session::Session,
    ours: &[String],
    command: &[String],
    launch: Option<&std::rc::Rc<tilo_session::apps::Apps>>,
    session_bus: Option<&str>,
    export_displays: bool,
    host_desktop: bool,
    doors: &Doors<'_>,
) -> std::process::ExitCode {
    use std::process::ExitCode;

    let socket = match session.compositor().listen() {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            return ExitCode::FAILURE;
        }
    };
    tilo_startup_metrics::mark("display-bound");
    println!("WAYLAND_DISPLAY={socket}");
    // A session added to this machine's own login serves the installed folder importer: whether
    // identity setup is over, on the socket named for this session's display, and the review the
    // person left to be opened after it.
    if host_desktop && let Err(error) = session.serve_folder_import(std::ffi::OsStr::new(&socket)) {
        tracing::warn!(%error, display = %socket, "Add folders cannot ask whether setup is done");
    }
    // Where a program of this login asks to work here as an agent: what `tilolang repl --door`
    // connects to. The session bound it as it started.
    if let Some(door) = doors.agent.filter(|door| door.exists()) {
        println!("TILO_AGENT_DOOR={}", door.display());
    }
    // Where `tilo-control` drives it, in a development build started with one.
    if let Some(control) = doors.test_control.filter(|control| control.exists()) {
        println!("{}={}", tilo_session::test_control::ENV, control.display());
    }
    // Where the owner's own programs drive this desktop, when it serves them.
    if let Some(admin) = doors.admin.filter(|admin| admin.exists()) {
        println!(
            "{}={}",
            tilo_session::test_control::ADMIN_ENV,
            admin.display()
        );
    }
    // X11 is served per workspace: each gets a display when an app is launched into it, and its
    // server starts when that app connects. `--no-x11` serves none of them.
    let x11 = session.compositor().x11_displays();
    match &x11 {
        Some(displays) if ours.iter().any(|arg| arg == "--no-x11") => displays.disable(),
        Some(_) => {}
        None => tracing::warn!("the compositor serves no X11 display: no X11 apps"),
    }
    // The apps opened from now on connect where the person's windows are, and each is told the
    // display of the workspace it is launched into.
    if let Some(launch) = launch {
        launch.set_displays(
            tilo_paths::xdg::runtime_dir_or_default().join(&socket),
            x11,
            session_bus.map(str::to_owned),
        );
        launch.set_render_node(session.compositor().render_node());
    }
    let environment = SessionEnvironment {
        wayland_display: socket,
    };
    if export_displays {
        environment.clone().export();
    }
    let mut child = command
        .split_first()
        .and_then(|(program, rest)| start_command(program, rest, &environment, session_bus));
    match session.run(|_| {
        tilo_session::termination::asked()
            || child
                .as_mut()
                .is_some_and(|child| child.try_wait().is_ok_and(|status| status.is_some()))
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            ExitCode::FAILURE
        }
    }
}

/// What this desktop knows of its own machine reaches the person through their notifications. Only
/// the session that owns the machine tells them: a session in a window of another desktop, or a
/// headless one, has that machine's storage, updates and history to leave alone.
#[cfg(target_os = "linux")]
fn tell_the_person(
    backend: Backend,
    desktop: Option<&tilo_session::desktop_services::DesktopServices>,
) {
    if backend == Backend::Drm
        && let Some(desktop) = desktop
    {
        tilo_session::notices::start(desktop.notifications());
    }
}

/// The drives this desktop announces, when it owns the machine and the desktop services hold a
/// system bus to ask udisks over: a session in a window of another desktop leaves that desktop's
/// drives to it.
#[cfg(target_os = "linux")]
fn drives(
    backend: Backend,
    desktop: Option<&std::rc::Rc<tilo_session::desktop_services::DesktopServices>>,
    wake: &tilo_service::LoopWake,
) -> Option<std::rc::Rc<tilo_session::drives::Drives>> {
    if backend != Backend::Drm {
        return None;
    }
    let desktop = desktop?;
    let connection = desktop.system_bus()?.clone();
    Some(std::rc::Rc::new(tilo_session::drives::Drives::new(
        tilo_session::drives::udisks(connection, wake.handle()),
        desktop.notifications().clone(),
    )))
}

/// A main loop that stops for good keeps the display frozen on its last frame and the keyboard
/// unread; the watchdog records why and ends the process, so the person comes back to the login
/// screen. Only the TTY backend owns the display; a window or a headless run leaves nothing to hand
/// back.
#[cfg(target_os = "linux")]
fn watch_the_loop(backend: Backend, session: &mut tilo_session::Session) {
    if backend == Backend::Drm
        && let Err(error) = session.guard_main_loop()
    {
        tracing::warn!(%error, "the main loop is not watched");
    }
}

/// The desktop services on `buses`, their resources added to `services`;
/// what keeps them running.
#[cfg(target_os = "linux")]
fn desktop_services(
    buses: &tilo_session::desktop_services::Buses,
    wake: &tilo_service::LoopWake,
    capture: std::sync::Arc<dyn tilo_portal::Capture>,
    side: &tilo_session::portal_link::PortalSide,
    services: &mut Vec<Box<dyn tilo_session::Service>>,
    inhibitors: Option<tilo_power_mgmt::InhibitRegistry>,
) -> Option<std::rc::Rc<tilo_session::desktop_services::DesktopServices>> {
    match tilo_session::desktop_services::DesktopServices::start(
        buses,
        &wake.handle(),
        capture,
        side,
        inhibitors,
    ) {
        Ok(desktop) => {
            services.extend(desktop.services());
            Some(std::rc::Rc::new(desktop))
        }
        Err(error) => {
            tracing::warn!(%error, "the desktop services did not start");
            None
        }
    }
}

/// What the session reads of this device: the person's `config` as read once at startup, their
/// search, quick settings over its own backends and the players, its session actions and who
/// unlocks it. A session in a window of another desktop ([`tilo_session::Machine::Window`]) reads
/// no device of its own and carries out no session action: the window's machine is the host's.
#[cfg(target_os = "linux")]
fn device_options(
    machine: tilo_session::Machine,
    config: tilo_session::SessionConfig,
) -> tilo_session::Options {
    use tilo_session::{
        ActionPlane, Options, Vpn,
        displays::{Backlights, Screens},
    };
    let owned = machine.is_owned();
    // This device's apps, as search and the surfaces that name an app read them, and the index
    // apps publish what they hold into.
    let content = tilo_desktop_search::content_index::session();
    Options {
        config,
        search: search_options(std::sync::Arc::clone(&content)),
        lists: tilo_session::Lists::of_device(),
        content,
        // The apps this session starts reach its services over the socket the launcher hands them;
        // a session inside another desktop's window serves them on a socket of its own, leaving
        // that desktop's alone.
        action_plane: ActionPlane::for_machine(machine),
        // A program of this login asks the person at the seat's agent door to work in a workspace
        // as an agent; a session in another desktop's window serves none.
        agent_door: owned.then(tilo_session::agent_door::path).flatten(),
        // A session in another desktop's window dims none of the window's machine: the display it
        // draws in is that desktop's.
        backlights: owned.then(|| Box::new(Screens) as Box<dyn Backlights>),
        // The controllers in the room drive the desktop of the session that owns the machine; a
        // session in another desktop's window leaves them to that desktop.
        controllers: owned.then_some(tilo_session::controllers::Source::Machine),
        session_actions: if owned {
            os_actions::perform_session_action
        } else {
            tilo_session::no_session_actions
        },
        // The lock screen, the on-screen keyboard and quick settings are the
        // session's own apps: the screen is started when the session locks,
        // the keyboard whenever the person may want it, under the hands they
        // type with, and quick settings for the session's life, shown while
        // the stack has its popover up.
        surfaces: surfaces(),
        user: tilo_session::unlock::login_user(),
        verifier: tilo_session::unlock::installed(),
        state: tilo_paths::state_dir(),
        // The desk the shell this session replaces kept here, brought over on the first start that
        // has no desk of its own.
        old_desk: tilo_session::shell_state_path(),
        // A session in a window of another desktop leaves the machine's traffic to the session that
        // owns it.
        vpn: owned.then(Vpn::machine),
        ..Options::default()
    }
}

/// The surface apps the session starts itself: its lock screen, its on-screen keyboard, and quick
/// settings. Each runs when what it shows is wanted; a machine without one loses that surface,
/// never the session.
#[cfg(target_os = "linux")]
fn surfaces() -> Vec<tilo_session::surfaces::Surface> {
    let mut surfaces = lock_screen();
    match tilo_session::keyboard::surface() {
        Ok(keyboard) => surfaces.push(keyboard),
        Err(error) => eprintln!("tilo-session: no on-screen keyboard: {error}"),
    }
    match tilo_session::quick_settings::surface() {
        Ok(panel) => surfaces.push(panel.person(tilo_session::unlock::login_user())),
        Err(error) => eprintln!("tilo-session: no quick settings: {error}"),
    }
    surfaces
}

/// The lock screen, when one is installed: the app whose window asks the compositor for the
/// session's lock, drawn on every output and given every input while the session is locked.
#[cfg(target_os = "linux")]
fn lock_screen() -> Vec<tilo_session::surfaces::Surface> {
    let program = match tilo_session::surfaces::program_path("tilo-lock-screen") {
        Ok(program) => program,
        Err(error) => {
            eprintln!("tilo-session: no lock screen: {error}");
            return Vec::new();
        }
    };
    vec![
        tilo_session::surfaces::Surface::new("lock", program)
            .person(tilo_session::unlock::login_user())
            .lock(),
    ]
}

/// `program` started with `rest` on `environment` 's displays and, when a flag named the session
/// bus, on that bus rather than the one it inherits.
#[cfg(target_os = "linux")]
fn start_command(
    program: &str,
    rest: &[String],
    environment: &SessionEnvironment,
    session_bus: Option<&str>,
) -> Option<std::process::Child> {
    let mut started = std::process::Command::new(program);
    started
        .args(rest)
        .env("WAYLAND_DISPLAY", &environment.wayland_display);
    // A display belongs to a workspace: an app started with the session is in none, and gets the
    // display of the workspace it is launched into later.
    started.env_remove("DISPLAY");
    if let Some(address) = session_bus {
        started.env("DBUS_SESSION_BUS_ADDRESS", address);
    }
    started
        .spawn()
        .map_err(|error| eprintln!("tilo-session: {program}: {error}"))
        .ok()
}

/// What keeps the services' readings following the device while it lives.
#[cfg(target_os = "linux")]
type Following = Vec<Box<dyn std::any::Any>>;

/// The system services the session hosts, on this device's own daemons,
/// and what keeps their readings following the device. A session in a
/// window of another desktop hosts only the settings service: the machine's
/// connectivity, clock, time zone and audio routing are the host's, and
/// changing them from a window would change the host.
#[cfg(target_os = "linux")]
fn services(
    wake: &tilo_service::LoopWake,
    power: Option<tilo_session_power::PowerLink>,
    machine: tilo_session::Machine,
) -> (Vec<Box<dyn tilo_session::Service>>, Following) {
    use std::sync::Arc;
    use tilo_connectivity_service::{Connectivity, device::OsDevice};

    let mut services: Vec<Box<dyn tilo_session::Service>> = Vec::new();
    let mut following: Following = Vec::new();
    services.push(Box::new(tilo_settings_service::SettingsService::new()));
    // What this session ran into: the list as the resource's reading and each record's text as its
    // own, which Settings reads as an app does.
    services.push(Box::new(tilo_crash_records::CrashRecords::new()));
    if !machine.is_owned() {
        return (services, following);
    }
    if let Some(link) = power {
        match tilo_session_power::SessionPower::start(link) {
            Ok(power) => services.push(Box::new(power)),
            Err(error) => tracing::warn!(%error, "the power service did not start"),
        }
    }
    match Connectivity::new(Arc::new(OsDevice), Some(wake.handle())) {
        Ok(connectivity) => {
            match connectivity.follow() {
                Ok(follow) => following.push(Box::new(follow)),
                Err(error) => tracing::warn!(%error, "connectivity does not follow the device"),
            }
            services.push(Box::new(connectivity));
        }
        Err(error) => tracing::warn!(%error, "connectivity service not started"),
    }
    match tilo_system_service::SystemService::new(
        Arc::new(tilo_system_service::device::OsSystem),
        Some(wake.handle()),
    ) {
        Ok(mut system) => {
            if let Err(error) = system.follow_location() {
                tracing::warn!(%error, "the time zone does not follow the device");
            }
            services.push(Box::new(system));
        }
        Err(error) => tracing::warn!(%error, "system service not started"),
    }
    match audio_helper().map(|helper| {
        let mut command = std::process::Command::new(helper);
        command.arg("--serve");
        tilo_service::lines::Process::spawn(
            command,
            tilo_audio_service::publish,
            Some(wake.handle()),
        )
    }) {
        Some(Ok(audio)) => services.push(Box::new(audio)),
        Some(Err(error)) => tracing::warn!(%error, "the audio service did not start"),
        None => tracing::warn!("the audio service is not installed here"),
    }
    (services, following)
}

/// The audio service (`tilo-audio`): beside the session, else where the image installs it.
#[cfg(target_os = "linux")]
fn audio_helper() -> Option<std::path::PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("tilo-audio")));
    beside
        .into_iter()
        .chain([std::path::PathBuf::from("/usr/libexec/tilo-audio")])
        .find(|path| path.exists())
}

/// What search reads of this device: the Tilo apps installed here and the machine's own and the
/// apps the person can get, each followed as it changes, what apps hold, the person's files, the
/// records the workspaces hold, and the search sources they switched off.
#[cfg(target_os = "linux")]
fn search_options(
    content: std::sync::Arc<tilo_desktop_search::content_index::AppContentIndex>,
) -> tilo_session::SearchOptions {
    use std::sync::Arc;
    use tilo_desktop_search::providers::files::FileNames;

    let files = tilo_paths::home().map(|home| {
        let content = tilo_desktop_search::files_index::FilesIndex::open_or_create(&home)
            .ok()
            .map(Arc::new);
        (home.clone(), FileNames::scan(home), content)
    });
    // What a reference written under an app's earlier id names, for search and for every decision
    // this process makes.
    match app_catalogue::apps::earlier_ids_from_env() {
        Ok(earlier) => {
            tilo_action_policy::earlier::register_all(&earlier);
            content.set_earlier_ids(earlier);
        }
        Err(error) => eprintln!("tilo-session: earlier app ids: {error}"),
    }
    // What the apps installed here declare, held where a launched app's surface is: the records an
    // app keeps of the person's are known before it runs, so its endpoint — the Files daemon, on
    // the record route — publishes its tree with the app never opened.
    match app_catalogue::apps::signed_surfaces_from_env() {
        Ok(surfaces) => {
            for (app, _name, surface) in surfaces {
                tilo_action_broker::register_package_surface(&app, &surface);
            }
        }
        Err(error) => eprintln!("tilo-session: installed app surfaces: {error}"),
    }
    tilo_session::SearchOptions {
        indexes: Some(std::sync::Arc::new(
            tilo_session::platform::indexes::SessionIndexes::new(content, files),
        )),
        preferences: tilo_desktop_search::SearchPreferences::from_values(
            &settings_store::load_section(tilo_desktop_search::preferences::SECTION),
        ),
        places: tilo_desktop_search::providers::places::RecentPlaces::default(),
        // The settings pages are this desktop's own: the session offers them to search itself.
        settings: None,
        // The records the workspaces on this device hold, read now and followed as the apps of
        // those workspaces name more.
        records: Some(tilo_session::Records::of(cli::data_home())),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {}
