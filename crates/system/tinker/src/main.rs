//! `tinker` — a place to be root that is not the machine.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use tinker::session::Share;
use tinker::{Diff, LayerKind, SessionSpec, Store, diff, idmap};

#[derive(Parser)]
#[command(
    name = "tinker",
    about = "Install packages, poke at the system, be root — in a layer you can throw away.",
    long_about = "Opens a shell where you are uid 0 and the package manager works, over a base \
                  image plus an overlay layer that holds everything you change. Nothing is \
                  written to the host's own filesystem, so a wrecked layer cannot affect the \
                  next boot. Run `tinker diff` to see exactly what a layer changed, `tinker \
                  keep` to make it permanent, `tinker discard` to make it never have happened.",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open a shell in a layer (the default when no subcommand is given).
    Shell(ShellArgs),
    /// Show every layer, its base, and whether it survives.
    List,
    /// Show what a layer changed, relative to its base.
    Diff {
        name: String,
        /// Include package-manager bookkeeping.
        #[arg(long)]
        all: bool,
    },
    /// Make an ephemeral layer permanent.
    Keep {
        name: String,
        /// Rename it while keeping it.
        #[arg(long = "as", value_name = "NEW_NAME")]
        rename: Option<String>,
        /// A note to show in `tinker list`.
        #[arg(long)]
        note: Option<String>,
    },
    /// Delete a layer and everything in it.
    Discard {
        name: String,
        /// Do not ask.
        #[arg(long, short)]
        yes: bool,
    },
    /// Undo one path's worth of change inside a layer.
    Revert { name: String, path: PathBuf },
    /// Manage base root filesystems.
    #[command(subcommand)]
    Base(BaseCmd),
    /// Explain what a layer can and cannot do, and how to ask for more.
    Explain,
}

#[derive(Args)]
struct ShellArgs {
    /// Layer to open. Omitted means a fresh ephemeral layer.
    name: Option<String>,
    /// Base root filesystem to stack on.
    #[arg(long, default_value = "debian")]
    base: String,
    /// Stack on top of an existing layer instead of the bare base. Repeatable,
    /// base-most first.
    #[arg(long = "on", value_name = "LAYER")]
    on: Vec<String>,
    /// Keep the layer without having to decide at the end.
    #[arg(long, short)]
    keep: bool,
    /// No network inside.
    #[arg(long)]
    offline: bool,
    /// Expose a host path, read-only unless suffixed `:rw`.
    #[arg(long = "share", value_name = "PATH[:rw]")]
    share: Vec<String>,
    /// Print the sandbox that would be built, and do not run it.
    #[arg(long)]
    explain: bool,
    /// Command to run instead of an interactive shell.
    #[arg(last = true)]
    command: Vec<OsString>,
}

#[derive(Subcommand)]
enum BaseCmd {
    /// List available bases.
    List,
    /// Unpack a container image into a base root filesystem.
    Add {
        name: String,
        /// Image reference, e.g. `docker.io/library/debian:stable-slim`.
        #[arg(long)]
        from_image: String,
    },
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("tinker: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let store = Store::user()?;

    match cli.command.unwrap_or(Cmd::Shell(ShellArgs {
        name: None,
        base: "debian".into(),
        on: vec![],
        keep: false,
        offline: false,
        share: vec![],
        explain: false,
        command: vec![],
    })) {
        Cmd::Shell(args) => cmd_shell(&store, args),
        Cmd::List => cmd_list(&store),
        Cmd::Diff { name, all } => cmd_diff(&store, &name, all),
        Cmd::Keep { name, rename, note } => {
            cmd_keep(&store, &name, rename.as_deref(), note.as_deref())
        }
        Cmd::Discard { name, yes } => cmd_discard(&store, &name, yes),
        Cmd::Revert { name, path } => cmd_revert(&store, &name, &path),
        Cmd::Base(BaseCmd::List) => cmd_base_list(&store),
        Cmd::Base(BaseCmd::Add { name, from_image }) => cmd_base_add(&store, &name, &from_image),
        Cmd::Explain => {
            print!("{}", explain_text());
            Ok(())
        }
    }
}

/// Commands that read or write a layer's contents have to run inside the layer's
/// id mapping — outside it, files belonging to the layer's service accounts are
/// not even readable. Doing this transparently is the difference between a tool
/// that works and one that tells the user to go and find `podman unshare`.
fn with_mapping<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    idmap::ensure_mapped()?;
    f()
}

fn cmd_shell(store: &Store, args: ShellArgs) -> Result<()> {
    let base_path = store.base_path(&args.base);
    if !base_path.is_dir() {
        bail!(
            "no base named '{}'. Add one with:\n    tinker base add {} --from-image docker.io/library/debian:stable-slim",
            args.base,
            args.base
        );
    }

    // `--explain` answers "what would this do" and must not create anything, so
    // it is served before any state exists and without entering the mapping.
    if args.explain {
        let name = args.name.clone().unwrap_or_else(|| "scratch".to_string());
        let mut spec = SessionSpec::new(
            store.base_path(&args.base),
            store.upper_dir(&name),
            store.work_dir(&name),
            store.home_dir(&name),
            &name,
        );
        spec.lower_layers = args.on.iter().map(|l| store.upper_dir(l)).collect();
        spec.network = !args.offline;
        spec.command = args.command.clone();
        for s in &args.share {
            spec.shares.push(parse_share(s)?);
        }
        println!("{}", spec.explain());
        return Ok(());
    }

    // Enter the id mapping *before* creating anything. Re-execution restarts this
    // process from its own argv, so any layer created beforehand would be
    // abandoned and a second one made in the child.
    idmap::ensure_mapped()?;

    let name = if let Some(n) = &args.name {
        if store.load(n).is_err() {
            store.create(
                n,
                &args.base,
                if args.keep {
                    LayerKind::Kept
                } else {
                    LayerKind::Ephemeral
                },
                args.on.clone(),
            )?;
        }
        n.clone()
    } else {
        let n = next_scratch_name(store)?;
        store.create(
            &n,
            &args.base,
            if args.keep {
                LayerKind::Kept
            } else {
                LayerKind::Ephemeral
            },
            args.on.clone(),
        )?;
        n
    };

    let layer = store.load(&name)?;
    let mut spec = SessionSpec::new(
        store.base_path(&layer.base),
        store.upper_dir(&name),
        store.work_dir(&name),
        store.home_dir(&name),
        &name,
    );
    spec.lower_layers = layer
        .stacked_on
        .iter()
        .map(|l| store.upper_dir(l))
        .collect();
    spec.network = !args.offline;
    spec.command = args.command.clone();
    for s in &args.share {
        spec.shares.push(parse_share(s)?);
    }

    {
        let status = Command::new("bwrap")
            .args(spec.to_bwrap_args())
            .status()
            .context("could not run bwrap. Work layers are built with bubblewrap.")?;

        let layer = store.load(&name)?;
        if layer.kind == LayerKind::Ephemeral {
            // The one moment where the whole design pays off: the layer is still
            // on disk, so the offer to keep it is real and not a consolation
            // message about work already deleted.
            let d = diff::compute(&store.upper_dir(&name), Some(&store.base_path(&layer.base)))?;
            if d.is_empty() {
                store.discard(&name)?;
            } else {
                report_ephemeral_exit(&name, &d);
            }
        }
        if let Some(code) = status.code().filter(|_| !status.success()) {
            std::process::exit(code);
        }
        Ok(())
    }
}

/// What the user sees when a throwaway shell that actually changed something
/// exits. It must state the deadline and both commands, because an offer whose
/// expiry is unclear is an offer people resolve by never using the tool again.
fn report_ephemeral_exit(name: &str, d: &Diff) {
    let pkgs: Vec<&str> = d.packages.values().flatten().map(String::as_str).collect();
    eprintln!();
    eprintln!("Layer '{name}' is ephemeral and still on disk.");
    if !pkgs.is_empty() {
        eprintln!("  it added {} package(s): {}", pkgs.len(), pkgs.join(" "));
    }
    let other = d.substantive().count();
    if other > 0 {
        eprintln!("  and {other} change(s) no package accounts for — tinker diff {name}");
    }
    eprintln!();
    eprintln!("  keep it:     tinker keep {name} --as <a-better-name>");
    eprintln!("  throw away:  tinker discard {name}");
}

fn next_scratch_name(store: &Store) -> Result<String> {
    let existing = store.list()?;
    for i in 1..1000 {
        let candidate = format!("scratch{i}");
        if !existing.iter().any(|l| l.name == candidate) {
            return Ok(candidate);
        }
    }
    bail!("too many scratch layers; discard some with `tinker discard`")
}

fn parse_share(spec: &str) -> Result<Share> {
    let (path, writable) = match spec.strip_suffix(":rw") {
        Some(p) => (p, true),
        None => (spec.strip_suffix(":ro").unwrap_or(spec), false),
    };
    let host = PathBuf::from(path);
    if !host.is_absolute() {
        bail!("--share needs an absolute path, got '{path}'");
    }
    Ok(Share {
        guest: host.clone(),
        host,
        writable,
    })
}

fn cmd_list(store: &Store) -> Result<()> {
    let layers = store.list()?;
    if layers.is_empty() {
        println!("No layers. `tinker` opens one.");
        return Ok(());
    }
    println!("{:<20} {:<12} {:<10} NOTE", "LAYER", "BASE", "KIND");
    for l in layers {
        let stack = if l.stacked_on.is_empty() {
            String::new()
        } else {
            format!(" (on {})", l.stacked_on.join(", "))
        };
        println!(
            "{:<20} {:<12} {:<10} {}{}",
            l.name,
            l.base,
            l.kind.to_string(),
            l.note,
            stack
        );
    }
    Ok(())
}

fn cmd_diff(store: &Store, name: &str, all: bool) -> Result<()> {
    let layer = store.load(name)?;
    with_mapping(|| {
        let d = diff::compute(&store.upper_dir(name), Some(&store.base_path(&layer.base)))?;
        if d.is_empty() {
            println!(
                "Layer '{name}' has changed nothing from base '{}'.",
                layer.base
            );
            return Ok(());
        }
        if all {
            for (mgr, pkgs) in &d.packages {
                println!("packages ({mgr}): {}", pkgs.join(" "));
            }
            for c in &d.changes {
                println!("  {} {}", c.kind.marker(), c.path.display());
            }
        } else {
            print!("{d}");
        }
        println!(
            "\n{} changed path(s), {} bytes in the layer.",
            d.changes.len(),
            d.total_bytes()
        );
        println!(
            "Base '{}' is untouched; `tinker discard {name}` returns to it exactly.",
            layer.base
        );
        Ok(())
    })
}

fn cmd_keep(store: &Store, name: &str, rename: Option<&str>, note: Option<&str>) -> Result<()> {
    let layer = store.promote(name, rename, note)?;
    println!(
        "Layer '{}' is kept. `tinker shell {}` reopens it.",
        layer.name, layer.name
    );
    Ok(())
}

fn cmd_discard(store: &Store, name: &str, yes: bool) -> Result<()> {
    let layer = store.load(name)?;
    if !yes && layer.kind == LayerKind::Kept {
        // Ephemeral layers are throwaway by definition; a kept one was an
        // explicit decision, so undoing it asks.
        bail!("layer '{name}' is kept; pass --yes to discard it anyway");
    }
    with_mapping(|| {
        store.discard(name)?;
        println!(
            "Discarded '{name}'. Base '{}' was never modified.",
            layer.base
        );
        Ok(())
    })
}

fn cmd_revert(store: &Store, name: &str, path: &Path) -> Result<()> {
    with_mapping(|| {
        store.revert_path(name, path)?;
        println!(
            "Reverted {} in '{name}' — the base version shows through again.",
            path.display()
        );
        Ok(())
    })
}

fn cmd_base_list(store: &Store) -> Result<()> {
    let rd = if let Ok(rd) = std::fs::read_dir(store.bases_dir()) {
        rd
    } else {
        println!("No bases yet. Add one with:");
        println!("    tinker base add debian --from-image docker.io/library/debian:stable-slim");
        return Ok(());
    };
    for e in rd.filter_map(std::result::Result::ok) {
        println!("{}", e.file_name().to_string_lossy());
    }
    Ok(())
}

/// Unpack an OCI image into a base tree.
///
/// The tree has to end up owned by ids inside the user's own mapping, or the
/// layer's root cannot write to directories the image says belong to root — an
/// unprivileged namespace grants authority over mapped ids only, and host root is
/// not one of them. That single constraint is why a work layer stacks on an
/// unpacked base rather than on the host's own `/usr`.
fn cmd_base_add(store: &Store, name: &str, image: &str) -> Result<()> {
    let dest = store.base_path(name);
    if dest.exists() {
        bail!("base '{name}' already exists at {}", dest.display());
    }
    std::fs::create_dir_all(&dest)?;

    eprintln!("tinker: pulling {image}");
    let ok = Command::new("podman")
        .args(["pull", image])
        .status()?
        .success();
    if !ok {
        let _ = std::fs::remove_dir(&dest);
        bail!("could not pull {image}");
    }

    let out = Command::new("podman")
        .args(["create", image, "/bin/true"])
        .output()?;
    if !out.status.success() {
        bail!(
            "podman create failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let cid = String::from_utf8_lossy(&out.stdout).trim().to_string();

    eprintln!("tinker: unpacking into {}", dest.display());
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "podman export {} | tar -x -C {} --exclude='dev/*'",
            shell_quote(&cid),
            shell_quote(&dest.to_string_lossy())
        ))
        .status()?;
    let _ = Command::new("podman").args(["rm", &cid]).output();
    if !status.success() {
        bail!("unpacking {image} failed");
    }
    println!("Base '{name}' ready. `tinker --base {name}` opens a layer on it.");
    Ok(())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The discoverability surface. A tinkerer who cannot find out how something
/// works will conclude the system is hiding it from them, which is the precise
/// feeling this design exists to avoid — so the boundary is written down, in
/// full, including what it does not protect.
fn explain_text() -> &'static str {
    "\
A work layer is not a weaker root. It is a real root, over a filesystem that is
not the machine's.

Inside a layer you are uid 0. The package manager works. You can edit /etc,
replace binaries in /usr, and break the environment completely.

What that root can reach:
  - the base root filesystem, read-only underneath your layer
  - your layer's own writes, which is where every change lands
  - the layer's own home directory, at /root
  - the network, unless you passed --offline
  - host paths you named with --share, read-only unless you said :rw

What it cannot reach, and why:
  - the host's own /usr, /etc and boot chain. They are not mounted. Your uid 0
    is uid 0 in a user namespace whose map contains only ids delegated to you,
    so the kernel grants it no authority over anything the host owns.
  - your real home directory. It is not shared in. Name what you want with
    --share; nothing else follows.
  - kernel modules, the host firewall, raw devices, the clock, other users.
    A namespace does not grant those, and no amount of being root inside gets
    them.

Because the host filesystem is never written, a layer cannot affect the next
boot. There is no state to repair and nothing to roll back: `tinker discard`
is the repair.

What a layer does NOT protect you from:
  - the kernel. A user namespace is a large interface, and handing you one
    widens the attack surface of the machine, not just of the layer.
  - yourself. Code in a layer runs as you, and anything you --share is exposed
    to it for real. A persistent layer runs its own contents on every entry.
  - what you installed. Packages come from their repository's signing key, and
    that trust is the repository's, not this machine's.
  - resource exhaustion. A layer can fill the disk.

To change the host itself rather than a layer, that is a separate, deliberate
request — not something a layer can escalate into.
"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_defaults_to_read_only() {
        let s = parse_share("/data").unwrap();
        assert_eq!(s.host, PathBuf::from("/data"));
        assert_eq!(s.guest, PathBuf::from("/data"));
        assert!(!s.writable);
    }

    #[test]
    fn share_rw_suffix_is_honoured_and_stripped() {
        let s = parse_share("/data:rw").unwrap();
        assert_eq!(s.host, PathBuf::from("/data"));
        assert!(s.writable);
    }

    #[test]
    fn share_ro_suffix_is_accepted_explicitly() {
        let s = parse_share("/data:ro").unwrap();
        assert_eq!(s.host, PathBuf::from("/data"));
        assert!(!s.writable);
    }

    #[test]
    fn relative_shares_are_refused() {
        // A relative --share would resolve against whatever cwd bwrap happened
        // to have, which is not something a user can reason about.
        assert!(parse_share("data").is_err());
        assert!(parse_share("./data:rw").is_err());
    }

    #[test]
    fn scratch_names_do_not_collide() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::at(d.path());
        let first = next_scratch_name(&s).unwrap();
        s.create(&first, "debian", LayerKind::Ephemeral, vec![])
            .unwrap();
        let second = next_scratch_name(&s).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn explain_states_the_limits_not_only_the_powers() {
        let t = explain_text();
        for required in ["does NOT protect", "kernel", "--share", "discard"] {
            assert!(t.contains(required), "explain text omits {required:?}");
        }
    }

    #[test]
    fn shell_quoting_survives_a_quote() {
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }
}
