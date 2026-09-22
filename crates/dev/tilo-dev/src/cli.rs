//! Command-line surface. Every subcommand carries a one-line purpose and
//! realistic examples, because agents learn the tool from `--help`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "tilo-dev",
    version,
    about = "Test Tilo builds in a shared dev VM: your own slot, one-command component swaps, rollback.",
    after_help = "Examples:\n  tilo-dev                     # current VM, slot, generation, session, next steps\n  tilo-dev swap shell          # build shell from this worktree into your slot\n  tilo-dev guide quickstart    # built-in docs\n\nWith no command, tilo-dev shows `status`."
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
    #[command(after_help = "Examples:\n  tilo-dev status\n  tilo-dev status --json | jq .slot")]
    Status,
    /// Check host prerequisites, the VM and its daemon; print a fix for each failure.
    #[command(after_help = "Examples:\n  tilo-dev doctor\n  tilo-dev doctor --json")]
    Doctor,
    /// Print built-in documentation.
    #[command(
        after_help = "Examples:\n  tilo-dev guide\n  tilo-dev guide swapping\n  tilo-dev guide troubleshooting"
    )]
    Guide {
        /// quickstart, concepts, swapping, driving, storage, platforms, troubleshooting
        topic: Option<String>,
    },
    /// Manage dev VMs through the host's hypervisor.
    #[command(subcommand)]
    Vm(VmCommand),
    /// Manage slots: your user, session and layer stack inside a VM.
    #[command(subcommand)]
    Slot(SlotCommand),
    /// Put a build of a component into a slot and restart what it needs.
    #[command(
        after_help = "Examples:\n  tilo-dev swap shell                      # your slot, built from this worktree\n  tilo-dev swap vm fs                      # VM-wide component into the vm slot\n  tilo-dev swap review-a launcher --from layer:3f9c..."
    )]
    Swap {
        /// `[SLOT] COMPONENT`; the slot defaults to the one named after your branch.
        #[arg(num_args = 1..=2, required = true, value_names = ["SLOT", "COMPONENT"])]
        target: Vec<String>,
        /// `worktree` (build on this host) or `layer:<hash>` (already in the VM).
        #[arg(long, default_value = "worktree")]
        from: String,
    },
    /// Switch a slot back to an earlier generation.
    #[command(
        after_help = "Examples:\n  tilo-dev rollback\n  tilo-dev rollback --to 0      # the bare VM image\n  tilo-dev rollback vm"
    )]
    Rollback {
        slot: Option<String>,
        /// Generation to switch to; default is the one before the current.
        #[arg(long)]
        to: Option<u64>,
    },
    /// Show a slot's session journal.
    #[command(after_help = "Examples:\n  tilo-dev logs\n  tilo-dev logs -n 500 review-a")]
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
        after_help = "Examples:\n  tilo-dev shot\n  tilo-dev shot -o target/after-swap.png review-a"
    )]
    Shot {
        slot: Option<String>,
        #[arg(short, long, default_value = "target/tilo-dev-shot.png")]
        output: PathBuf,
    },
    /// Run the VM's tilo-control against a slot's session (the one driving story).
    #[command(
        after_help = "Examples:\n  tilo-dev control actions list\n  tilo-dev control actions invoke notes new-note\n  tilo-dev control --slot review-a state-dump --pretty"
    )]
    Control {
        #[arg(long)]
        slot: Option<String>,
        /// Arguments for tilo-control.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Delete generations and layers nothing needs any more.
    #[command(after_help = "Examples:\n  tilo-dev gc --dry-run\n  tilo-dev gc --keep 2")]
    Gc {
        #[arg(long)]
        dry_run: bool,
        /// Generations to keep per slot besides the current one (default from vms.toml, else 5).
        #[arg(long)]
        keep: Option<u32>,
    },
}

#[derive(Debug, Subcommand)]
pub enum VmCommand {
    /// List registered VMs and which one this worktree uses.
    #[command(after_help = "Examples:\n  tilo-dev vm list")]
    List,
    /// Choose the VM for this worktree (writes .tilo-dev), or for everyone with --global.
    #[command(after_help = "Examples:\n  tilo-dev vm use dev\n  tilo-dev vm use shared --global")]
    Use {
        name: String,
        #[arg(long)]
        global: bool,
    },
    /// Open a shell in the VM, or run one command there. Not for deploying.
    #[command(
        after_help = "Examples:\n  tilo-dev vm ssh\n  tilo-dev vm ssh -- journalctl -b -p warning\n  tilo-dev vm ssh --raw -- touch /var/tmp/x"
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
    #[command(after_help = "Examples:\n  tilo-dev vm status\n  tilo-dev vm status shared")]
    Status { name: Option<String> },
    /// Register a VM and create it from a dev image with the host's hypervisor.
    #[command(
        after_help = "Examples:\n  tilo-dev vm create dev --image ~/images/tilo-dev.qcow2 --disk ~/vms/dev.qcow2\n  tilo-dev vm create mac --hypervisor utm --template 'Tilo Dev'\n  tilo-dev vm create arm --arch aarch64 --emulated --image arm.qcow2 --disk arm-dev.qcow2"
    )]
    Create(CreateArgs),
    /// Boot the VM and wait until SSH answers.
    #[command(after_help = "Examples:\n  tilo-dev vm start")]
    Start { name: Option<String> },
    /// Shut the VM down cleanly.
    #[command(after_help = "Examples:\n  tilo-dev vm stop")]
    Stop { name: Option<String> },
    /// Move the VM to a newer dev image (bootc); slots survive, they live in /var.
    #[command(
        after_help = "Examples:\n  tilo-dev vm upgrade\n  tilo-dev vm upgrade --image code.podesta.ai/qos/tilo-dev:2026-09-22"
    )]
    Upgrade {
        name: Option<String>,
        #[arg(long)]
        image: Option<String>,
    },
    /// Attach to the VM's serial console (works when SSH does not).
    #[command(after_help = "Examples:\n  tilo-dev vm serial")]
    Serial { name: Option<String> },
    /// Screenshot the VM's own display through the hypervisor.
    #[command(after_help = "Examples:\n  tilo-dev vm shot -o target/vm.png")]
    Shot {
        name: Option<String>,
        #[arg(short, long, default_value = "target/tilo-dev-vm.png")]
        output: PathBuf,
    },
    /// Hard-reset the VM.
    #[command(after_help = "Examples:\n  tilo-dev vm reset")]
    Reset { name: Option<String> },
    /// Pass arguments to the hypervisor's native control tool (virsh, QEMU monitor, utmctl, PowerShell).
    #[command(
        after_help = "Examples:\n  tilo-dev vm control dominfo dev\n  tilo-dev vm control --vm ci info block"
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
    /// Published dev image to create the VM from.
    #[arg(long)]
    pub image: Option<String>,
    /// The VM's own disk.
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
        after_help = "Examples:\n  tilo-dev slot create --headless\n  tilo-dev slot create review-a --headful"
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
    #[command(after_help = "Examples:\n  tilo-dev slot list\n  tilo-dev slot list --json")]
    List,
    /// Restart a slot's session on its current generation (also repairs an interrupted swap).
    #[command(after_help = "Examples:\n  tilo-dev slot restart\n  tilo-dev slot restart vm")]
    Restart { name: Option<String> },
    /// Remove a slot: its session, user and generations (layers stay until gc).
    #[command(after_help = "Examples:\n  tilo-dev slot rm review-a")]
    Rm { name: Option<String> },
}

#[derive(Debug, Subcommand)]
pub enum SeatCommand {
    /// Move a slot's session onto seat0, the display the hypervisor window shows.
    #[command(
        after_help = "Examples:\n  tilo-dev seat claim\n  tilo-dev seat claim review-a --steal"
    )]
    Claim {
        slot: Option<String>,
        /// Take seat0 even if another slot holds it.
        #[arg(long)]
        steal: bool,
    },
    /// Give seat0 back to greetd; the slot returns to its own seat.
    #[command(after_help = "Examples:\n  tilo-dev seat release")]
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
                        help.contains("Examples:\n  tilo-dev"),
                        "{name} has no examples"
                    );
                }
                walk(sub, &name);
            }
        }
        Cli::command().debug_assert();
        walk(&Cli::command(), "tilo-dev");
    }

    #[test]
    fn swap_takes_optional_slot() {
        let cli = Cli::try_parse_from(["tilo-dev", "swap", "shell"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Swap { ref target, .. }) if target == &["shell"])
        );
        let cli = Cli::try_parse_from([
            "tilo-dev", "--json", "swap", "vm", "fs", "--from", "layer:ab",
        ])
        .unwrap();
        assert!(cli.json);
        assert!(
            matches!(cli.command, Some(Command::Swap { ref target, ref from }) if target.len() == 2 && from == "layer:ab")
        );
        let cli = Cli::try_parse_from(["tilo-dev", "control", "click", "--x", "1"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Control { ref args, .. }) if args == &["click", "--x", "1"])
        );
        assert!(Cli::try_parse_from(["tilo-dev"]).unwrap().command.is_none());
    }
}
