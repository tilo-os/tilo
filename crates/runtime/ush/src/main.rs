//! `qos-ush` — one binary, three roles, dispatched on `argv[0]`.
//!
//! - `ush` / `dsh` on the host: build a guest and run a shell in it.
//! - `pkg`, `perm`, `scan`, `guard`, `quarantine`, `restart` inside a guest:
//!   the runtime's own commands, symlinks to this binary on the guest's `PATH`.
//! - `qos-ush serve`: the session supervisor an app asks for a guest.
//!
//! One binary because the guest has to run the *same* build that built its
//! confinement, and because `pkg` inside a guest and the supervisor outside it
//! are two halves of one protocol — splitting them would let the halves drift.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ush::broker::{Request, Response};
use ush::profile::Profile;
use ush::store::Store;
use ush::supervisor::{Session, SessionOptions};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let arg0 = Path::new(&argv[0])
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Inside a guest the runtime is reached through its own name. Outside it,
    // `ush` and `dsh` are the two profiles of the same program.
    let result = match arg0.as_str() {
        "pkg" => guest::pkg(&argv[1..]),
        "perm" => guest::perm(&argv[1..]),
        "scan" => guest::scan(&argv[1..]),
        "guard" => guest::guard(&argv[1..]),
        "quarantine" => guest::quarantine(&argv[1..]),
        "restart" => guest::restart(),
        "ush" => host(Profile::Ush, &argv[1..]),
        "dsh" => host(Profile::Dsh, &argv[1..]),
        _ => dispatch(&argv[1..]),
    };

    match result {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("{}: {e:#}", if arg0.is_empty() { "ush" } else { &arg0 });
            ExitCode::from(1)
        }
    }
}

/// `qos-ush <subcommand>` — the spelling used when the binary is invoked by
/// its own name rather than through a shim.
fn dispatch(args: &[String]) -> anyhow::Result<i32> {
    match args.first().map(String::as_str) {
        Some("serve") => cmd_serve(&args[1..]),
        Some("guest-init") => guest::init(&args[1..]),
        Some("dsh") => host(Profile::Dsh, &args[1..]),
        Some("ush" | "run") => host(Profile::Ush, &args[1..]),
        Some("pkg") => guest::pkg(&args[1..]),
        Some("perm") => guest::perm(&args[1..]),
        Some("scan") => guest::scan(&args[1..]),
        Some("guard") => guest::guard(&args[1..]),
        Some("quarantine") => guest::quarantine(&args[1..]),
        Some("--help" | "-h") | None => {
            print!("{USAGE}");
            Ok(0)
        }
        Some(other) => anyhow::bail!("unknown command `{other}`; try --help"),
    }
}

const USAGE: &str = "\
qos-ush — an application runtime that hosts a shell.

On the host:
  ush [-c COMMAND] [-v] [--ephemeral]   open a sandboxed shell
  dsh [-c COMMAND] [-v]                 open a developer shell (NOT a sandbox)
  qos-ush serve [--socket PATH]         hand guests to apps that ask

Inside a guest:
  pkg install [--one-time] PKG…    pkg remove PKG…    pkg list
  pkg diff                         pkg inspect PKG    pkg freeze
  pkg burn
  perm trust-dir DIR               perm list          perm revoke ID
  scan PATH
  guard status
  quarantine list | add PATH | restore PATH
  restart
";

// host side

fn host(profile: Profile, args: &[String]) -> anyhow::Result<i32> {
    let mut opts = SessionOptions {
        profile,
        ..SessionOptions::default()
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-c" => {
                let script = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("-c needs a command"))?;
                // Matching `sh -c`: everything after it is one script, run by
                // the guest's shell, not split into an argv here.
                opts.command = vec![
                    ush::supervisor::guest_shell(&opts.base)
                        .to_string_lossy()
                        .into_owned(),
                    "-c".into(),
                    script.clone(),
                ];
                i += 2;
            }
            "-v" | "--verbose" => {
                opts.verbose = true;
                i += 1;
            }
            "--ephemeral" => {
                opts.ephemeral = true;
                i += 1;
            }
            "--base" => {
                opts.base = PathBuf::from(
                    args.get(i + 1)
                        .ok_or_else(|| anyhow::anyhow!("--base needs a path"))?,
                );
                i += 2;
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(0);
            }
            other => anyhow::bail!("unknown option `{other}`; try --help"),
        }
    }

    init_tracing(opts.verbose);
    if profile == Profile::Dsh {
        eprintln!(
            "dsh: {}\ndsh: everything you run here has the authority of your \
             user account. Do not run software you do not trust.",
            profile.posture_summary()
        );
    }
    let mut session = Session::new(Store::discover(), opts);
    session.run()
}

fn cmd_serve(args: &[String]) -> anyhow::Result<i32> {
    let mut socket = default_service_socket();
    let mut base = PathBuf::from("/");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" => {
                socket = PathBuf::from(
                    args.get(i + 1)
                        .ok_or_else(|| anyhow::anyhow!("--socket needs a path"))?,
                );
                i += 2;
            }
            "--base" => {
                base = PathBuf::from(
                    args.get(i + 1)
                        .ok_or_else(|| anyhow::anyhow!("--base needs a path"))?,
                );
                i += 2;
            }
            other => anyhow::bail!("unknown option `{other}`"),
        }
    }
    init_tracing(true);
    ush::supervisor::serve(&socket, Store::discover(), base)?;
    Ok(0)
}

/// `$XDG_RUNTIME_DIR/qos/services/ush.sock` — the path `qos-launcher` looks in
/// for a `[sandbox.services]` entry named `ush`.
fn default_service_socket() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
        || PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })),
        PathBuf::from,
    );
    runtime.join("qos/services/ush.sock")
}

fn init_tracing(verbose: bool) {
    let level = if verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::WARN
    };
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .without_time()
        .try_init();
}

mod guest {
    use super::{Path, PathBuf, Request, Response};
    use ush::pkg::{InstalledPackage, Manifest};

    fn broker_socket() -> PathBuf {
        std::env::var_os("QOS_USH_BROKER")
            .map_or_else(|| PathBuf::from(ush::GUEST_BROKER_SOCK), PathBuf::from)
    }

    fn ask(req: &Request) -> anyhow::Result<Response> {
        ush::broker::ask(&broker_socket(), req).map_err(|e| {
            anyhow::anyhow!(
                "the broker is not reachable at {} ({e}). These commands only \
                 work inside a guest — run `ush` first.",
                broker_socket().display()
            )
        })
    }

    fn report(resp: Response) -> anyhow::Result<i32> {
        match resp {
            Response::Ok => Ok(0),
            Response::NeedsRestart { detail } => {
                println!("{detail}");
                Ok(0)
            }
            Response::Denied { reason } => {
                eprintln!("refused: {reason}");
                Ok(1)
            }
            other => {
                println!("{other:?}");
                Ok(0)
            }
        }
    }

    /// The guest's first process: installs Landlock inside the namespace, where
    /// the rules name paths the guest will actually use, then becomes the shell.
    pub fn init(args: &[String]) -> anyhow::Result<i32> {
        use std::os::unix::process::CommandExt;
        let rest: Vec<String> = args
            .iter()
            .skip_while(|a| *a != "--")
            .skip(1)
            .cloned()
            .collect();
        anyhow::ensure!(!rest.is_empty(), "guest-init needs a command after `--`");

        // Everything the guest may write, in here: layered system dirs, scratch,
        // home, and whatever was trusted (named again so a later remount can't
        // widen it).
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/root"), PathBuf::from);
        let mut rw: Vec<PathBuf> = ["/usr", "/etc", "/tmp", "/var", "/run", "/dev", "/proc"]
            .iter()
            .map(PathBuf::from)
            .collect();
        rw.push(home);
        // The root is readable, not writable — without a rule for it the ruleset
        // denies `stat("/")`, and `ls /` fails looking like a broken system.
        let mut ro: Vec<PathBuf> = vec![PathBuf::from("/")];
        match ask(&Request::Status) {
            Ok(Response::Status { status }) => {
                rw.extend(status.trusted_dirs.iter().map(PathBuf::from));
                ro.extend(status.readable_dirs.iter().map(PathBuf::from));
            }
            // No broker means no policy to read, which means no grants — the
            // restriction is *tighter*, not skipped.
            _ => tracing::debug!("no broker; restricting to the base set only"),
        }
        let outcome = ush::guard::restrict_self(&rw, &ro);
        tracing::debug!("landlock: {}", outcome.describe());

        Err(std::process::Command::new(&rest[0])
            .args(&rest[1..])
            .exec()
            .into())
    }

    pub fn pkg(args: &[String]) -> anyhow::Result<i32> {
        match args.first().map(String::as_str) {
            Some("install") => pkg_install(&args[1..]),
            Some("remove") => pkg_remove(&args[1..]),
            Some("list") => pkg_list(),
            Some("diff") => pkg_diff(),
            Some("inspect") => pkg_inspect(&args[1..]),
            Some("freeze") => pkg_freeze(),
            Some("burn") => report(ask(&Request::Burn)?),
            _ => {
                eprintln!(
                    "pkg install [--one-time] PKG… | remove PKG… | list | diff | \
                     inspect PKG | freeze | burn"
                );
                Ok(2)
            }
        }
    }

    fn manifest_path() -> PathBuf {
        PathBuf::from(ush::pkg::MANIFEST_PATH)
    }

    fn pkg_install(args: &[String]) -> anyhow::Result<i32> {
        let one_time = args.iter().any(|a| a == "--one-time");
        let packages: Vec<String> = args
            .iter()
            .filter(|a| !a.starts_with("--"))
            .cloned()
            .collect();
        anyhow::ensure!(!packages.is_empty(), "pkg install needs a package name");

        // A base-provided package isn't something this layer installed; recording
        // it as one-time would be harmful, since undoing it runs `dnf remove` and
        // writes a whiteout, deleting a base file from the guest's view. Ask rpm first.
        let (already, wanted): (Vec<String>, Vec<String>) = packages
            .into_iter()
            .partition(|p| ush::pkg::is_installed(p));
        for name in &already {
            println!(
                "pkg: {name} is already part of the base image; nothing to install \
                 and nothing for this layer to own."
            );
        }
        if wanted.is_empty() {
            return Ok(0);
        }
        let packages = wanted;

        let argv = ush::pkg::install_argv(&packages, &ush::pkg::releasever());
        let status = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .status()?;
        if !status.success() {
            eprintln!(
                "pkg: the transaction failed. If it was refused while writing \
                 under /usr, the file already existed in the base image — a \
                 layer can add to the base and cannot alter it."
            );
            return Ok(status.code().unwrap_or(1));
        }

        let mut manifest = Manifest::load(&manifest_path());
        for name in &packages {
            manifest.record(InstalledPackage {
                nevra: ush::pkg::nevra(name),
                name: name.clone(),
                at: ush::audit::now_unix(),
                one_time,
            });
            let _ = ask(&Request::PkgRecorded {
                package: name.clone(),
                action: "install".into(),
                one_time,
            });
        }
        manifest.save(&manifest_path())?;
        if one_time {
            println!(
                "pkg: {} installed into this session's layer; it is removed when \
                 the session ends unless you run `pkg freeze`.",
                packages.join(", ")
            );
        }
        Ok(0)
    }

    fn pkg_remove(args: &[String]) -> anyhow::Result<i32> {
        let packages: Vec<String> = args
            .iter()
            .filter(|a| !a.starts_with("--"))
            .cloned()
            .collect();
        anyhow::ensure!(!packages.is_empty(), "pkg remove needs a package name");
        let argv = ush::pkg::remove_argv(&packages, &ush::pkg::releasever());
        let status = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .status()?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1));
        }
        let mut manifest = Manifest::load(&manifest_path());
        for name in &packages {
            manifest.forget(name);
            let _ = ask(&Request::PkgRecorded {
                package: name.clone(),
                action: "remove".into(),
                one_time: false,
            });
        }
        manifest.save(&manifest_path())?;
        Ok(0)
    }

    fn pkg_list() -> anyhow::Result<i32> {
        let manifest = Manifest::load(&manifest_path());
        if manifest.packages.is_empty() {
            println!("no packages in this layer; the base is what you see.");
            return Ok(0);
        }
        for p in &manifest.packages {
            println!(
                "{:<40} {}",
                p.nevra,
                if p.one_time { "one-time" } else { "" }
            );
        }
        Ok(0)
    }

    fn pkg_diff() -> anyhow::Result<i32> {
        let Response::Diff { entries, truncated } = ask(&Request::LayerDiff { limit: 500 })? else {
            anyhow::bail!("the broker did not answer with a diff");
        };
        if entries.is_empty() {
            println!("your layers have changed nothing relative to the base.");
            return Ok(0);
        }
        for e in &entries {
            println!("{:<10} {:>10}  {}", e.kind, e.bytes, e.path);
        }
        if truncated > 0 {
            println!("… and {truncated} more");
        }
        Ok(0)
    }

    fn pkg_inspect(args: &[String]) -> anyhow::Result<i32> {
        let name = args
            .first()
            .ok_or_else(|| anyhow::anyhow!("pkg inspect needs a package name"))?;
        let i = ush::pkg::inspect(name);
        println!("{}", i.nevra);
        if !i.summary.is_empty() {
            println!("  {}", i.summary);
        }
        println!("  files    : {}", i.files.len());
        println!(
            "  scripts  : {}",
            if i.scripts.is_empty() {
                "none".to_string()
            } else {
                i.scripts.join(", ")
            }
        );
        println!(
            "  services : {}",
            if i.services.is_empty() {
                "none".to_string()
            } else {
                i.services.join(", ")
            }
        );
        if i.risks.is_empty() {
            println!("  risk     : nothing mechanical to report");
        } else {
            for r in &i.risks {
                println!("  risk     : {r}");
            }
        }
        println!(
            "  note     : this is a mechanical read of what rpm says. No \
             signature or reputation check was performed."
        );
        Ok(0)
    }

    fn pkg_freeze() -> anyhow::Result<i32> {
        let mut manifest = Manifest::load(&manifest_path());
        let promoted = manifest.freeze();
        manifest.save(&manifest_path())?;
        ask(&Request::Freeze)?;
        if promoted.is_empty() {
            println!("nothing was installed for this session only.");
        } else {
            println!("kept: {}", promoted.join(", "));
        }
        Ok(0)
    }

    pub fn perm(args: &[String]) -> anyhow::Result<i32> {
        match args.first().map(String::as_str) {
            Some("trust-dir") => {
                let dir = args
                    .get(1)
                    .ok_or_else(|| anyhow::anyhow!("perm trust-dir needs a directory"))?;
                report(ask(&Request::TrustDir {
                    path: PathBuf::from(dir),
                })?)
            }
            Some("revoke") => {
                let id = args
                    .get(1)
                    .ok_or_else(|| anyhow::anyhow!("perm revoke needs a capability id"))?;
                report(ask(&Request::Revoke { id: id.clone() })?)
            }
            Some("list") | None => {
                let Response::Perms { entries } = ask(&Request::ListPerms)? else {
                    anyhow::bail!("the broker did not answer with a permission list");
                };
                if entries.is_empty() {
                    println!(
                        "nothing is granted. Your home directory in here is this \
                         runtime's, not yours; `perm trust-dir DIR` is how a real \
                         directory appears."
                    );
                }
                for e in &entries {
                    println!("{:<24} {:<10} {:<10} {}", e.id, e.allows, e.expiry, e.scope);
                }
                Ok(0)
            }
            Some(other) => anyhow::bail!("unknown `perm {other}`; try trust-dir, list, revoke"),
        }
    }

    pub fn scan(args: &[String]) -> anyhow::Result<i32> {
        let path = args
            .first()
            .ok_or_else(|| anyhow::anyhow!("scan needs a path"))?;
        let trusted = match ask(&Request::Status) {
            Ok(Response::Status { status }) => {
                status.trusted_dirs.iter().map(PathBuf::from).collect()
            }
            _ => Vec::new(),
        };
        let r = ush::guard::scan(Path::new(path), &trusted);
        println!("{}", r.path.display());
        if !r.exists {
            for n in &r.notes {
                println!("  {n}");
            }
            return Ok(1);
        }
        println!("  size       : {} bytes", r.bytes);
        println!(
            "  kind       : {}",
            if r.is_dir {
                "directory"
            } else if r.executable {
                "executable file"
            } else {
                "file"
            }
        );
        println!(
            "  package    : {}",
            r.owned_by
                .as_deref()
                .unwrap_or("none — not installed by rpm")
        );
        println!(
            "  unmodified : {}",
            if r.owned_by.is_none() {
                "n/a"
            } else if r.modified_since_install {
                "no — rpm disagrees with the installed package"
            } else {
                "yes"
            }
        );
        for n in &r.notes {
            println!("  note       : {n}");
        }
        Ok(0)
    }

    pub fn guard(args: &[String]) -> anyhow::Result<i32> {
        match args.first().map(String::as_str) {
            Some("status") | None => {
                let Response::Status { status } = ask(&Request::Status)? else {
                    anyhow::bail!("the broker did not answer with a status");
                };
                println!("session   : {}", status.session);
                println!("profile   : {}", status.profile);
                println!("posture   : {}", status.posture);
                println!("layer     : {} ({})", status.layer, status.layer_kind);
                println!("seccomp   : {}", status.seccomp);
                println!("landlock  : {}", landlock_now());
                println!(
                    "trusted   : {}",
                    if status.trusted_dirs.is_empty() {
                        "nothing — your files are not in here".to_string()
                    } else {
                        status.trusted_dirs.join(", ")
                    }
                );
                if !status.readable_dirs.is_empty() {
                    println!("readable  : {}", status.readable_dirs.join(", "));
                }
                if !status.one_time_packages.is_empty() {
                    println!(
                        "one-time  : {} (removed when this session ends)",
                        status.one_time_packages.join(", ")
                    );
                }
                println!("audit     : {} records", status.audit_records);
                Ok(0)
            }
            Some(other) => anyhow::bail!("unknown `guard {other}`; try status"),
        }
    }

    /// Report Landlock by testing it, not repeating what was asked for.
    /// `guard status` answers "is this actually confined" — a line that said
    /// "installed" because the install code ran would pass exactly when it
    /// should fail, so the answer comes from an operation the ruleset forbids.
    fn landlock_now() -> String {
        let probed = ush::guard::landlock_probe().describe();
        let reported = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|l| l.strip_prefix("Landlock:").map(|v| v.trim().to_string()))
            });
        match reported {
            Some(v) => format!("{probed} [/proc/self/status: {v}]"),
            None => format!("{probed} [this kernel does not report it in /proc/self/status]"),
        }
    }

    pub fn quarantine(args: &[String]) -> anyhow::Result<i32> {
        let dir = PathBuf::from(ush::guard::QUARANTINE_DIR);
        match args.first().map(String::as_str) {
            Some("add") => {
                let path = args
                    .get(1)
                    .ok_or_else(|| anyhow::anyhow!("quarantine add needs a path"))?;
                let stored = ush::guard::quarantine_add(Path::new(path), &dir)?;
                println!("quarantined {path} → {} (not executable)", stored.display());
                Ok(0)
            }
            Some("restore") => {
                let path = args
                    .get(1)
                    .ok_or_else(|| anyhow::anyhow!("quarantine restore needs a path"))?;
                let wanted = PathBuf::from(path);
                let entry = ush::guard::quarantine_list(&dir)
                    .into_iter()
                    .find(|e| e.original == wanted)
                    .ok_or_else(|| anyhow::anyhow!("{path} is not in quarantine"))?;
                ush::guard::quarantine_restore(&entry)?;
                println!("restored {path}");
                Ok(0)
            }
            Some("list") | None => {
                let entries = ush::guard::quarantine_list(&dir);
                if entries.is_empty() {
                    println!(
                        "quarantine is empty. Nothing is put here automatically: \
                         this runtime ships no signature database, so the \
                         judgement is yours. `quarantine add PATH` moves a file \
                         here and clears its executable bits."
                    );
                    return Ok(0);
                }
                for e in &entries {
                    println!("{:>10}  {}", e.bytes, e.original.display());
                }
                Ok(0)
            }
            Some(other) => {
                anyhow::bail!("unknown `quarantine {other}`; try list, add, restore")
            }
        }
    }

    /// Tear the guest down so the supervisor can build it again. There is no
    /// way to remount a namespace from inside it, so a restart is literally a
    /// new guest: the broker records the request, this ends the current one,
    /// and the supervisor loops — signalling PID 1 (the sandbox's own init
    /// inside the guest's pid namespace) takes every process in it with it.
    pub fn restart() -> anyhow::Result<i32> {
        ask(&Request::Restart)?;
        println!("restarting the guest…");
        // SAFETY: plain `kill(2)`. Inside the guest's pid namespace pid 1 is
        // the sandbox's own init; outside one this fails with EPERM and the
        // fallback below applies.
        if unsafe { libc::kill(1, libc::SIGTERM) } == 0 {
            std::process::exit(0);
        }
        // SAFETY: `getppid` cannot fail; the parent is the shell that ran this.
        unsafe { libc::kill(libc::getppid(), libc::SIGHUP) };
        std::process::exit(0);
    }
}
