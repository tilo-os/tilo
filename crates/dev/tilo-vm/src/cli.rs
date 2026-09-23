//! Command-line surface. Every subcommand carries a one-line purpose and
//! realistic examples, because agents learn the tool from `--help`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "tilo-vm",
    version,
    about = "Test Tilo builds in a shared dev VM: your own slot, one-command component swaps, rollback.",
    after_help = "Examples:\n  tilo-vm                     # current VM, slot, generation, session, next steps\n  tilo-vm swap shell          # build shell from this worktree into your slot\n  tilo-vm guide quickstart    # built-in docs\n\nWith no command, tilo-vm shows `status`."
)]
pub struct Cli {
    /// Print one JSON object instead of text (errors too: {"error","hint","see"}).
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the current VM (and why), your slot, its generation and session, and what to do next.
    #[command(after_help = "Examples:\n  tilo-vm status\n  tilo-vm status --json | jq .slot")]
    Status,
    /// Check host prerequisites, the VM and its daemon; print a fix for each failure.
    #[command(after_help = "Examples:\n  tilo-vm doctor\n  tilo-vm doctor --json")]
    Doctor,
    /// Print built-in documentation.
    #[command(
        after_help = "Examples:\n  tilo-vm guide\n  tilo-vm guide swapping\n  tilo-vm guide troubleshooting"
    )]
    Guide {
        /// quickstart, concepts, swapping, driving, storage, platforms, troubleshooting, fedora-loop
        topic: Option<String>,
    },
    /// Manage dev VMs through the host's hypervisor.
    #[command(subcommand)]
    Vm(VmCommand),
    /// Manage slots: your user, session and layer stack inside a VM.
    #[command(subcommand)]
    Slot(SlotCommand),
    /// Put a build of a component (or an app) into a slot and restart what it needs.
    #[command(
        after_help = "Examples:\n  tilo-vm swap shell                      # your slot, built in the VM from this worktree\n  tilo-vm swap vm fs                      # VM-wide component into the vm slot\n  tilo-vm swap app:notes                  # package and install one app; live at its next launch\n  tilo-vm swap shell --from ci:main-1a2b3c4   # the CI build of a published image\n  tilo-vm swap shell --from sha:1a2b3c4   # the CI build of a main commit\n  tilo-vm swap shell --builder host       # build on this Linux host instead\n  tilo-vm swap review-a launcher --from layer:3f9c..."
    )]
    Swap {
        /// `[SLOT] COMPONENT`; the slot defaults to the one named after your
        /// branch. `app:<dir>` names an app under apps/.
        #[arg(num_args = 1..=2, required = true, value_names = ["SLOT", "COMPONENT"])]
        target: Vec<String>,
        /// `worktree` (build it), `layer:<hash>` (already in the VM),
        /// `sha:<commit>` or `ci:<tag>` (extract it from the published CI image).
        #[arg(long, default_value = "worktree")]
        from: String,
        /// Where `--from worktree` builds: `vm` (default; syncs the worktree
        /// into the VM) or `host` (Linux hosts only).
        #[arg(long, value_enum)]
        builder: Option<Builder>,
    },
    /// Type, press keys, move and click on a headful slot's own seat (real libinput path).
    #[command(
        after_help = "Examples:\n  tilo-vm input key ctrl+alt+t\n  tilo-vm input key super Return\n  tilo-vm input type 'hello world'\n  tilo-vm input move 400 300\n  tilo-vm input click left --at 400 300\n  tilo-vm input --slot review-b click right\n\nHeadless slots have no seat: drive them with tilo-vm control."
    )]
    Input {
        #[arg(long)]
        slot: Option<String>,
        #[command(subcommand)]
        action: InputCommand,
    },
    /// Switch a slot back to an earlier generation.
    #[command(
        after_help = "Examples:\n  tilo-vm rollback\n  tilo-vm rollback --to 0      # the bare VM image\n  tilo-vm rollback vm"
    )]
    Rollback {
        slot: Option<String>,
        /// Generation to switch to; default is the one before the current.
        #[arg(long)]
        to: Option<u64>,
    },
    /// Show a slot's session journal.
    #[command(after_help = "Examples:\n  tilo-vm logs\n  tilo-vm logs -n 500 review-a")]
    Logs {
        slot: Option<String>,
        #[arg(short = 'n', long, default_value_t = 100)]
        lines: u32,
    },
    /// Hand the VM's real display (seat0) to a slot, or give it back.
    #[command(subcommand)]
    Seat(SeatCommand),
    /// Save a PNG of a slot's live scene.
    #[command(
        after_help = "Examples:\n  tilo-vm shot\n  tilo-vm shot -o target/after-swap.png review-a"
    )]
    Shot {
        slot: Option<String>,
        #[arg(short, long, default_value = "target/tilo-vm-shot.png")]
        output: PathBuf,
    },
    /// Run the VM's tilo-control against a slot's session (the one driving story).
    #[command(
        after_help = "Examples:\n  tilo-vm control actions list\n  tilo-vm control actions invoke notes new-note\n  tilo-vm control --slot review-a state-dump --pretty"
    )]
    Control {
        #[arg(long)]
        slot: Option<String>,
        /// Arguments for tilo-control.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Delete generations and layers nothing needs any more, and evict old in-VM build dirs.
    #[command(
        after_help = "Examples:\n  tilo-vm gc --dry-run\n  tilo-vm gc --keep 2\n  tilo-vm gc --keep-targets 0   # evict every in-VM build dir not in use"
    )]
    Gc {
        #[arg(long)]
        dry_run: bool,
        /// Generations to keep per slot besides the current one (default from vms.toml, else 5).
        #[arg(long)]
        keep: Option<u32>,
        /// In-VM build dirs to keep, most recently used first (default from vms.toml, else 2).
        #[arg(long)]
        keep_targets: Option<u32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Builder {
    /// Sync the worktree into the VM and build there with the dev image's toolchain.
    Vm,
    /// Build on this host with its cargo (Linux only).
    Host,
}

#[derive(Debug, Subcommand)]
pub enum InputCommand {
    /// Press and release key combos in order.
    #[command(
        after_help = "Examples:\n  tilo-vm input key ctrl+alt+t\n  tilo-vm input key super\n  tilo-vm input key ctrl+l Return"
    )]
    Key {
        /// Combos like ctrl+alt+t, shift+Tab, super, Return, F5.
        #[arg(required = true)]
        combos: Vec<String>,
    },
    /// Type text on a US keyboard layout.
    #[command(after_help = "Examples:\n  tilo-vm input type 'hello world'")]
    Type { text: String },
    /// Move the pointer to output pixel X, Y.
    #[command(after_help = "Examples:\n  tilo-vm input move 400 300")]
    Move { x: u32, y: u32 },
    /// Click a button (left, right, middle), optionally at X, Y.
    #[command(
        after_help = "Examples:\n  tilo-vm input click\n  tilo-vm input click right --at 400 300"
    )]
    Click {
        #[arg(default_value = "left")]
        button: String,
        #[arg(long, num_args = 2, value_names = ["X", "Y"])]
        at: Option<Vec<u32>>,
    },
}

#[derive(Debug, Subcommand)]
pub enum VmCommand {
    /// List registered VMs and which one this worktree uses.
    #[command(after_help = "Examples:\n  tilo-vm vm list")]
    List,
    /// Choose the VM for this worktree (writes .tilo-vm), or for everyone with --global.
    #[command(after_help = "Examples:\n  tilo-vm vm use dev\n  tilo-vm vm use shared --global")]
    Use {
        name: String,
        #[arg(long)]
        global: bool,
    },
    /// Open a shell in the VM, or run one command there. Not for deploying.
    #[command(
        after_help = "Examples:\n  tilo-vm vm ssh\n  tilo-vm vm ssh -- journalctl -b -p warning\n  tilo-vm vm ssh --raw -- touch /var/tmp/x"
    )]
    Ssh {
        /// Run commands that look like deploys anyway.
        #[arg(long)]
        raw: bool,
        #[arg(long)]
        vm: Option<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Is the VM up, reachable, and running a compatible daemon?
    #[command(after_help = "Examples:\n  tilo-vm vm status\n  tilo-vm vm status shared")]
    Status { name: Option<String> },
    /// Download the published dev disk (or use a local one), create the VM with the host's hypervisor, and register it.
    #[command(
        after_help = "Examples:\n  tilo-vm vm create dev                       # newest published disk (needs FORGEJO_TOKEN, or fgj/tea login)\n  tilo-vm vm create dev --tag main-1a2b3c4\n  tilo-vm vm create dev --image ~/images/tilo-vm.qcow2 --disk ~/vms/dev.qcow2\n  tilo-vm vm create mac --hypervisor utm --template 'Tilo Dev'\n  tilo-vm vm create arm --arch aarch64 --emulated --image arm.qcow2 --disk arm-dev.qcow2"
    )]
    Create(CreateArgs),
    /// Boot the VM and wait until SSH answers.
    #[command(after_help = "Examples:\n  tilo-vm vm start")]
    Start { name: Option<String> },
    /// Shut the VM down cleanly, once no swap, gc or upgrade is running in it.
    #[command(
        after_help = "Examples:\n  tilo-vm vm stop\n  tilo-vm vm stop --force   # even while another command runs"
    )]
    Stop {
        name: Option<String>,
        /// Do not wait for the VM lock.
        #[arg(long)]
        force: bool,
    },
    /// Move the VM to a newer dev image (bootc); slots survive, they live in /var.
    #[command(
        after_help = "Examples:\n  tilo-vm vm upgrade\n  tilo-vm vm upgrade --image code.podesta.ai/qos/tilo-vm:2026-09-22"
    )]
    Upgrade {
        name: Option<String>,
        #[arg(long)]
        image: Option<String>,
    },
    /// Attach to the VM's serial console (works when SSH does not).
    #[command(after_help = "Examples:\n  tilo-vm vm serial")]
    Serial { name: Option<String> },
    /// Screenshot the VM's own display through the hypervisor.
    #[command(after_help = "Examples:\n  tilo-vm vm shot -o target/vm.png")]
    Shot {
        name: Option<String>,
        #[arg(short, long, default_value = "target/tilo-vm-vm.png")]
        output: PathBuf,
    },
    /// Hard-reset the VM.
    #[command(after_help = "Examples:\n  tilo-vm vm reset")]
    Reset { name: Option<String> },
    /// Pass arguments to the hypervisor's native control tool (virsh, QEMU monitor, utmctl, PowerShell).
    #[command(
        after_help = "Examples:\n  tilo-vm vm control dominfo dev\n  tilo-vm vm control --vm ci info block"
    )]
    Control {
        #[arg(long)]
        vm: Option<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    pub name: String,
    /// libvirt, qemu, utm or hyperv (default: the host's native one).
    #[arg(long)]
    pub hypervisor: Option<String>,
    /// A local dev disk to create the VM from, instead of downloading one.
    #[arg(long, conflicts_with = "tag")]
    pub image: Option<String>,
    /// Published dev disk tag to download (default: the newest main or release build).
    #[arg(long)]
    pub tag: Option<String>,
    /// The VM's own disk (default: in the platform data dir).
    #[arg(long)]
    pub disk: Option<String>,
    /// Guest architecture (default: the host's).
    #[arg(long)]
    pub arch: Option<String>,
    /// Allow a guest architecture other than the host's, emulated (slow).
    #[arg(long)]
    pub emulated: bool,
    #[arg(long)]
    pub memory_mb: Option<u32>,
    #[arg(long)]
    pub cpus: Option<u32>,
    /// `[user@]host[:port]` when the adapter cannot discover it.
    #[arg(long)]
    pub ssh: Option<String>,
    /// Host port forwarded to the guest's SSH (libvirt, qemu).
    #[arg(long)]
    pub ssh_port: Option<u16>,
    /// UTM VM to clone.
    #[arg(long)]
    pub template: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum SlotCommand {
    /// Create a slot (default: named after your branch) and start its session.
    #[command(
        after_help = "Examples:\n  tilo-vm slot create --headless\n  tilo-vm slot create review-a --headful"
    )]
    Create {
        name: Option<String>,
        /// Offscreen compositor, driven by control and shot (default).
        #[arg(long, conflicts_with = "headful")]
        headless: bool,
        /// Own virtual display and seat: the real DRM and input paths.
        #[arg(long)]
        headful: bool,
    },
    /// List the VM's slots with owner, generation and session state.
    #[command(after_help = "Examples:\n  tilo-vm slot list\n  tilo-vm slot list --json")]
    List,
    /// Restart a slot's session on its current generation (also repairs an interrupted swap).
    #[command(after_help = "Examples:\n  tilo-vm slot restart\n  tilo-vm slot restart vm")]
    Restart { name: Option<String> },
    /// Remove a slot: its session, user and generations (layers stay until gc).
    #[command(after_help = "Examples:\n  tilo-vm slot rm review-a")]
    Rm { name: Option<String> },
}

#[derive(Debug, Subcommand)]
pub enum SeatCommand {
    /// Move a slot's session onto seat0, the display the hypervisor window shows.
    #[command(
        after_help = "Examples:\n  tilo-vm seat claim\n  tilo-vm seat claim review-a --steal"
    )]
    Claim {
        slot: Option<String>,
        /// Take seat0 even if another slot holds it.
        #[arg(long)]
        steal: bool,
    },
    /// Give seat0 back to greetd; the slot returns to its own seat.
    #[command(after_help = "Examples:\n  tilo-vm seat release")]
    Release { slot: Option<String> },
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn every_subcommand_has_a_purpose_and_examples() {
        fn walk(command: &clap::Command, path: &str) {
            for sub in command.get_subcommands() {
                let name = format!("{path} {}", sub.get_name());
                assert!(sub.get_about().is_some(), "{name} has no one-line purpose");
                if sub.get_subcommands().next().is_none() {
                    let help = sub
                        .get_after_help()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    assert!(
                        help.contains("Examples:\n  tilo-vm"),
                        "{name} has no examples"
                    );
                }
                walk(sub, &name);
            }
        }
        Cli::command().debug_assert();
        walk(&Cli::command(), "tilo-vm");
    }

    #[test]
    fn swap_takes_optional_slot() {
        let cli = Cli::try_parse_from(["tilo-vm", "swap", "shell"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Swap { ref target, .. }) if target == &["shell"])
        );
        let cli = Cli::try_parse_from([
            "tilo-vm", "--json", "swap", "vm", "fs", "--from", "layer:ab",
        ])
        .unwrap();
        assert!(cli.json);
        assert!(
            matches!(cli.command, Some(Command::Swap { ref target, ref from, builder: None }) if target.len() == 2 && from == "layer:ab")
        );
        let cli =
            Cli::try_parse_from(["tilo-vm", "swap", "app:notes", "--builder", "host"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Swap {
                builder: Some(Builder::Host),
                ..
            })
        ));
        let cli = Cli::try_parse_from([
            "tilo-vm", "input", "--slot", "b", "click", "right", "--at", "4", "5",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Input { action: InputCommand::Click { ref button, at: Some(ref at) }, .. }) if button == "right" && at == &[4, 5]
        ));
        assert!(Cli::try_parse_from(["tilo-vm", "input", "key"]).is_err());
        let cli = Cli::try_parse_from(["tilo-vm", "control", "click", "--x", "1"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Control { ref args, .. }) if args == &["click", "--x", "1"])
        );
        assert!(Cli::try_parse_from(["tilo-vm"]).unwrap().command.is_none());
    }
}
