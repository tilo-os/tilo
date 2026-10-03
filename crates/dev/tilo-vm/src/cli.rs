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

impl Cli {
    /// `--slot <name>` and a positional slot mean the same thing; fold the flag
    /// into the positional so the rest of the tool sees one field.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        fn fold(slot: &mut Option<String>, flag: &mut Option<String>) {
            if slot.is_none() {
                *slot = flag.take();
            }
        }
        if let Some(
            Command::Rollback {
                slot, slot_flag, ..
            }
            | Command::Logs {
                slot, slot_flag, ..
            }
            | Command::Shot {
                slot, slot_flag, ..
            }
            | Command::Flight(FlightCommand::App {
                slot, slot_flag, ..
            })
            | Command::Seat(
                SeatCommand::Claim {
                    slot, slot_flag, ..
                }
                | SeatCommand::Release { slot, slot_flag },
            )
            | Command::Slot(
                SlotCommand::Create {
                    name: slot,
                    slot_flag,
                    ..
                }
                | SlotCommand::Restart {
                    name: slot,
                    slot_flag,
                }
                | SlotCommand::Rm {
                    name: slot,
                    slot_flag,
                }
                | SlotCommand::Show {
                    name: slot,
                    slot_flag,
                }
                | SlotCommand::Exec {
                    name: slot,
                    slot_flag,
                    ..
                }
                | SlotCommand::Apps(SlotAppsCommand::Install {
                    name: slot,
                    slot_flag,
                    ..
                }),
            ),
        ) = &mut self.command
        {
            fold(slot, slot_flag);
        }
        self
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the current VM (and why), your slot, its generation and session, and what to do next.
    #[command(
        after_help = "Examples:\n  tilo-vm status\n  tilo-vm status --slot review-a\n  tilo-vm status --json | jq .slot\n\nYour slot is named after your branch, unless TILO_VM_SLOT or `slot = \"<name>\"`\nin the worktree's .tilo-vm names another."
    )]
    Status {
        /// The slot to report on (default: your slot).
        #[arg(long)]
        slot: Option<String>,
    },
    /// Check host prerequisites, the VM and its daemon; print a fix for each failure.
    #[command(after_help = "Examples:\n  tilo-vm doctor\n  tilo-vm doctor --json")]
    Doctor,
    /// Print built-in documentation.
    #[command(
        after_help = "Examples:\n  tilo-vm guide\n  tilo-vm guide swapping\n  tilo-vm guide troubleshooting"
    )]
    Guide {
        /// quickstart, concepts, swapping, kernel, driving, scenarios, images, storage, platforms, troubleshooting, fedora-loop
        topic: Option<String>,
    },
    /// Manage dev VMs through the host's hypervisor.
    #[command(subcommand)]
    Vm(VmCommand),
    /// Build this worktree's dev image (and dev disk) into a directory, for vm upgrade --image and vm create --image.
    #[command(subcommand)]
    Image(ImageCommand),
    /// Manage slots: your user, session and layer stack inside a VM.
    #[command(subcommand)]
    Slot(SlotCommand),
    /// Put a build of a component (or an app) into a slot and restart what it needs.
    #[command(
        after_help = "Examples:\n  tilo-vm swap shell                      # your slot, built in the VM from this worktree\n  tilo-vm swap shell --profile dev        # faster compile, slower binary\n  tilo-vm swap --slot vm fs               # VM-wide component into the vm slot\n  tilo-vm swap app:notes                  # package and install one app; live at its next launch\n  tilo-vm swap shell --from ci:main-1a2b3c4   # the CI build of a published image\n  tilo-vm swap shell --from sha:1a2b3c4   # the CI build of a main commit\n  tilo-vm swap shell --builder host       # build on this Linux host instead\n  tilo-vm swap --slot review-a launcher --from layer:3f9c...\n  tilo-vm swap kernel                     # build os/kernel incrementally, install VM-wide, reboot\n  tilo-vm swap kernel --jobs 4            # fewer parallel compile jobs (default: half the CPUs)\n  tilo-vm swap kernel --direct            # boot the host build directly (libvirt), install nothing\n  tilo-vm swap kernel --direct --cmdline 'root=UUID=... ro console=ttyS0'   # when the VM is not up\n  tilo-vm rollback --slot vm              # back to the previous kernel (or off the direct boot)\n\nKernel: the build reuses ~/.cache/tilo-kernel/, so after the first (long) build\nonly what changed recompiles; see tilo-vm guide kernel.\n\nBuild profile: builds from this worktree use the component's manifest profile\n(release for almost all) with the manifest's features plus each package's\ndev-overrides feature, so speed is realistic and the harness variables work.\n--profile dev compiles much faster and runs slower; the layer records which."
    )]
    Swap {
        /// A component from os/components.toml (python3 os/scripts/install-components.py --list), or `app:<dir>` for an app under apps/.
        component: String,
        /// The slot to swap into (default: the one named after your branch;
        /// `vm` for VM-wide components).
        #[arg(long)]
        slot: Option<String>,
        /// `worktree` (build it), `layer:<hash>` (already in the VM),
        /// `sha:<commit>` or `ci:<tag>` (extract it from the published CI image).
        #[arg(long, default_value = "worktree")]
        from: String,
        /// Where `--from worktree` builds: `vm` (default; syncs the worktree
        /// into the VM) or `host` (Linux hosts only).
        #[arg(long, value_enum)]
        builder: Option<Builder>,
        /// Cargo profile for `--from worktree` builds (default: the manifest's,
        /// release for almost every component). `dev` compiles faster.
        #[arg(long)]
        profile: Option<String>,
        /// Kernel only: boot the host-built kernel straight from the host
        /// (libvirt direct kernel boot) instead of installing it in the VM.
        #[arg(long)]
        direct: bool,
        /// Kernel only: parallel compile jobs (default: half the CPUs, or
        /// `TILO_KERNEL_JOBS`).
        #[arg(long)]
        jobs: Option<u32>,
        /// With --direct: the kernel command line (default: the running
        /// VM's, read over SSH).
        #[arg(long, requires = "direct")]
        cmdline: Option<String>,
    },
    /// Keyboard, pointer, touch, pen and gamepad input on a headful slot's own seat (real kernel input path).
    #[command(
        after_help = "Examples:\n  tilo-vm input key ctrl+alt+t\n  tilo-vm input key super Return\n  tilo-vm input type 'hello world'\n  tilo-vm input move 400 300\n  tilo-vm input click left --at 400 300\n  tilo-vm input touch tap 400 300\n  tilo-vm input touch pinch 640 400 --from 100 --to 400\n  tilo-vm input pen drag 100 100 500 300\n  tilo-vm input gamepad press a\n  tilo-vm input gamepad stick left 0 -100\n  tilo-vm input --slot review-b click right\n\nEvery device is a uinput device plugged into the slot's seat, so the\ncompositor gets it through the kernel like real hardware (libinput; the\nseat's evdev access for game controllers). Keyboard and pointer exist from\nthe first input; the touchscreen, pen and gamepad appear the first time a\ncommand uses them. Headless slots have no seat: drive them with tilo-vm\ncontrol."
    )]
    Input {
        /// The slot whose seat gets the input (default: your slot).
        #[arg(long)]
        slot: Option<String>,
        #[command(subcommand)]
        action: InputCommand,
    },
    /// Switch a slot to another of its generations: back (the default), forward, or any by number.
    #[command(
        after_help = "Examples:\n  tilo-vm rollback              # the generation before the current one\n  tilo-vm rollback --forward    # the next newer one again, after a rollback\n  tilo-vm rollback --to 3       # any generation, older or newer\n  tilo-vm rollback --to 0       # the bare VM image\n  tilo-vm rollback --slot vm\n\nGenerations are kept until gc; tilo-vm slot show lists them."
    )]
    Rollback {
        /// The slot (default: your slot).
        slot: Option<String>,
        /// The slot, as a flag (same as the positional; every slot command takes `--slot`).
        #[arg(long = "slot", value_name = "SLOT", conflicts_with = "slot")]
        slot_flag: Option<String>,
        /// Generation to switch to, older or newer; default is the one before the current.
        #[arg(long)]
        to: Option<u64>,
        /// Switch to the next newer generation (undo a rollback).
        #[arg(long, conflicts_with = "to")]
        forward: bool,
    },
    /// Show a slot's session journal.
    #[command(after_help = "Examples:\n  tilo-vm logs\n  tilo-vm logs -n 500 review-a")]
    Logs {
        /// The slot (default: your slot).
        slot: Option<String>,
        /// The slot, as a flag (same as the positional; every slot command takes `--slot`).
        #[arg(long = "slot", value_name = "SLOT", conflicts_with = "slot")]
        slot_flag: Option<String>,
        /// Journal lines to show.
        #[arg(short = 'n', long, default_value_t = 100)]
        lines: u32,
    },
    /// Hand the VM's real display (seat0) to a slot, or give it back.
    #[command(subcommand)]
    Seat(SeatCommand),
    /// Save a PNG of the slot compositor's own frame (headless, headful, or on seat0).
    #[command(
        after_help = "Examples:\n  tilo-vm shot\n  tilo-vm shot -o target/after-swap.png review-a\n\nThe slot's compositor renders its current scene into a PNG (tilo-control\nsnapshot): the same picture for a headless slot, a headful slot on its vkms\nseat, and a slot that holds seat0. It does not read any display. For the\nVM's real display (seat0) as the hypervisor shows it: tilo-vm vm shot."
    )]
    Shot {
        /// The slot (default: your slot).
        slot: Option<String>,
        /// The slot, as a flag (same as the positional; every slot command takes `--slot`).
        #[arg(long = "slot", value_name = "SLOT", conflicts_with = "slot")]
        slot_flag: Option<String>,
        /// Where to write the PNG.
        #[arg(short, long, default_value = "target/tilo-vm-shot.png")]
        output: PathBuf,
    },
    /// Run the VM's tilo-control against a slot's session (the one driving story).
    #[command(
        after_help = "Examples:\n  tilo-vm control actions list\n  tilo-vm control actions invoke notes new-note\n  tilo-vm control --slot review-a state-dump --pretty"
    )]
    Control {
        /// The slot whose session tilo-control drives (default: your slot).
        #[arg(long)]
        slot: Option<String>,
        /// Arguments for tilo-control.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Run measured semantic acceptance flights against a live slot.
    #[command(subcommand)]
    Flight(FlightCommand),
    /// Cold-boot scenarios in a throwaway VM: boot components that swapping cannot replace.
    #[command(subcommand)]
    Scenario(ScenarioCommand),
    /// Delete generations and layers nothing needs any more, and evict old in-VM build dirs.
    #[command(
        after_help = "Examples:\n  tilo-vm gc --dry-run\n  tilo-vm gc --keep 2\n  tilo-vm gc --keep-targets 0   # evict every in-VM build dir not in use\n  tilo-vm gc --min-age 0        # also your own orphaned layers from the last hour\n\nUnreferenced layers younger than an hour stay by default: another agent's\nimport may still be waiting for its swap. gc names them and when they go."
    )]
    Gc {
        /// Say what would be removed (and what stays, and why) without removing anything.
        #[arg(long)]
        dry_run: bool,
        /// Keep unreferenced layers younger than this (e.g. 0, 30m, 1h, 2d; default 1h).
        #[arg(long, value_parser = parse_age)]
        min_age: Option<u64>,
        /// Generations to keep per slot besides the current one (default from vms.toml, else 5).
        #[arg(long)]
        keep: Option<u32>,
        /// In-VM build dirs to keep, most recently used first (default from vms.toml, else 2).
        #[arg(long)]
        keep_targets: Option<u32>,
    },
}

#[derive(Debug, Subcommand)]
pub enum FlightCommand {
    /// Launch an app through the desktop action model, focus it, type into it, and prove new frames present.
    #[command(
        after_help = "Examples:\n  tilo-vm flight app org.gnome.TextEditor org.gnome.TextEditor\n  tilo-vm flight app chatgpt chatgpt --type-text 'flight input' --timeout-seconds 20\n\nDESKTOP_ID is the host-applications action id. APP_ID is the Wayland/X11 app id the compositor reports."
    )]
    App {
        /// Desktop action id exported by the host-applications provider.
        desktop_id: String,
        /// App id expected on the presented foreign surface.
        app_id: String,
        /// The slot to test (default: your slot).
        slot: Option<String>,
        /// The slot, as a flag (same as the positional slot used by other slot commands).
        #[arg(long = "slot", value_name = "SLOT", conflicts_with = "slot")]
        slot_flag: Option<String>,
        /// Text injected after the app presents and receives focus.
        #[arg(long, default_value = "tilo-vm-flight")]
        type_text: String,
        /// Maximum time from action invocation until a real app frame presents.
        #[arg(long, default_value_t = 15_000)]
        maximum_launch_ms: u64,
        /// Maximum acceptable latency for any compositor control round trip during the flight.
        #[arg(long, default_value_t = 250)]
        maximum_control_ms: u64,
        /// Absolute deadline for the entire flight.
        #[arg(long, default_value_t = 20)]
        timeout_seconds: u64,
    },
}

#[derive(Debug, Subcommand)]
pub enum ScenarioCommand {
    /// Boot an installer medium, install through its published actions, cold-boot the result and verify first boot.
    #[command(
        after_help = "Examples:\n  tilo-vm scenario install\n  tilo-vm scenario install --medium os/output/installer/qcow2/disk.qcow2\n  tilo-vm scenario install --layout guided-alongside\n  tilo-vm scenario install --layout guided-alongside --target-disk C:\\VMs\\tilo-source.raw --graphics software\n  tilo-vm scenario install --layout prepared-alongside --graphics software\n  TILO_SECBOOT_KEYS=/path/to/release-vars tilo-vm scenario install --encrypt tpm-pin\n  tilo-vm scenario install --dry-run --json\n\nNo slot and no dev VM: the scenario boots its own throwaway QEMU VM with\nos/dev/vm.sh, reaches the installer on its console channel (the tilo-control\naction protocol over a virtio-serial port only this VM has), installs, powers\noff through the installer's Finish action, cold-boots the target, types the\nunlock secret at its boot prompt, and verifies the greeter runs the image the\ninstaller wrote. Build a medium first: ./os/build.sh --local, then\n./os/dev/build-custom-installer.sh. Details: tilo-vm guide scenarios."
    )]
    Install(InstallScenarioArgs),
}

#[derive(Debug, Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these independent Clap flags are part of the install command interface"
)]
pub struct InstallScenarioArgs {
    /// The installer medium (default: os/output/installer/qcow2/disk.qcow2, what os/dev/build-custom-installer.sh writes). Never modified: the VM boots a throwaway overlay.
    #[arg(long)]
    pub medium: Option<PathBuf>,
    /// Existing sparse raw target file for a Windows WHPX guided-alongside cycle. The file is written in place and retained after the run.
    #[arg(long)]
    pub target_disk: Option<PathBuf>,
    /// Stop after a verified encrypted install into a reviewed Windows raw target. For board-specific images that cannot boot on QEMU virt; requires a separate generic full-cycle proof before physical copy.
    #[arg(long)]
    pub skip_target_boot: bool,
    /// Account username for a retained install. Requires --credentials-stdin.
    #[arg(long, requires = "credentials_stdin")]
    pub username: Option<String>,
    /// Installed hostname (retained and checked when resuming).
    #[arg(long, default_value = "scenario-tilo")]
    pub hostname: String,
    /// Installer keyboard display name, e.g. "German (QWERTZ)". Omitted preserves the medium's choice.
    #[arg(long)]
    pub keyboard_layout: Option<String>,
    /// Installed IANA timezone, e.g. Europe/Berlin. Omitted preserves the medium's choice.
    #[arg(long)]
    pub timezone: Option<String>,
    /// Installed locale from the installer's catalog, e.g. de_DE.UTF-8. Omitted preserves its choice.
    #[arg(long)]
    pub locale: Option<String>,
    /// Read distinct `account_password` and `disk_passphrase` from one JSON object on stdin. Requires --username; unavailable for dry runs.
    #[arg(long, requires = "username", conflicts_with = "dry_run")]
    pub credentials_stdin: bool,
    /// The target disk before the install: erase (blank disk, used whole), prepared-alongside or guided-alongside (existing systems kept byte-for-byte).
    #[arg(long, value_enum, default_value = "erase")]
    pub layout: crate::scenario::Layout,
    /// Installed root filesystem; omitted leaves the installer's default unchanged.
    #[arg(long, value_parser = ["btrfs", "ext4", "xfs"])]
    pub filesystem: Option<String>,
    /// Resume verification of a completed retained Windows raw-target install in --out. Does not reinstall.
    #[arg(long, requires_all = ["out", "target_disk", "keep"], conflicts_with = "dry_run")]
    pub resume: bool,
    /// How the always-on disk encryption unlocks: passphrase (no TPM) or tpm-pin (software TPM; the installer offers it only for a verified boot chain).
    #[arg(long, value_enum, default_value = "passphrase")]
    pub encrypt: crate::scenario::Unlock,
    /// Graphics for both boots: gl (virgl, what the installed desktop needs) or software.
    #[arg(long, value_enum, default_value = "gl")]
    pub graphics: crate::scenario::Graphics,
    /// Artifact directory (default: `target/tilo-vm-scenarios/scenario-install-<time>`).
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Longest the install itself may take, in seconds.
    #[arg(long, default_value_t = 3600)]
    pub install_timeout: u64,
    /// Keep the target disk and the VM's runtime state afterwards.
    #[arg(long)]
    pub keep: bool,
    /// Print the plan (every command, the console socket, the artifact directory) without booting anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// Does this command leave a receipt? Only commands that change state or
/// run long do; looking around (status, doctor, guide, lists, logs, slot
/// show and exec, vm ssh without `--raw`, a scenario's dry run) does not.
pub fn writes_receipt(command: Option<&Command>) -> bool {
    !matches!(
        command,
        None | Some(
            Command::Status { .. }
                | Command::Doctor
                | Command::Guide { .. }
                | Command::Logs { .. }
                | Command::Vm(
                    VmCommand::List | VmCommand::Status { .. } | VmCommand::Ssh { raw: false, .. }
                )
                | Command::Slot(
                    SlotCommand::List
                        | SlotCommand::Show { .. }
                        | SlotCommand::Exec { .. }
                        | SlotCommand::Apps(SlotAppsCommand::List)
                )
                | Command::Scenario(ScenarioCommand::Install(InstallScenarioArgs {
                    dry_run: true,
                    ..
                }))
        )
    )
}

/// `--min-age`: seconds, from `0`, `90s`, `30m`, `1h` or `2d`.
///
/// # Errors
/// Anything else.
pub fn parse_age(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (number, unit) = text.split_at(
        text.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len()),
    );
    let value: u64 = number
        .parse()
        .map_err(|_| format!("'{text}' is not an age like 0, 90s, 30m, 1h or 2d"))?;
    let scale = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(format!("'{text}': the unit is s, m, h or d")),
    };
    Ok(value.saturating_mul(scale))
}

/// `--resolution`: `WIDTHxHEIGHT`, each 320 to 16384 pixels.
///
/// # Errors
/// Anything else.
pub fn parse_resolution(text: &str) -> Result<String, String> {
    let size = |part: &str| {
        part.parse::<u16>()
            .ok()
            .filter(|n| (320..=16_384).contains(n))
    };
    match text.trim().split_once('x') {
        Some((w, h)) if size(w).is_some() && size(h).is_some() => Ok(format!("{w}x{h}")),
        _ => Err(format!(
            "'{text}' is not WIDTHxHEIGHT with each side 320 to 16384, e.g. 5120x2880"
        )),
    }
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
    Type {
        /// The text to type.
        text: String,
    },
    /// Move the pointer to desktop pixel X, Y.
    #[command(
        after_help = "Examples:\n  tilo-vm input move 400 300\n\nOn a slot with one output, X and Y are that output's pixels. On a slot made\nwith --outputs they are logical desktop pixels, outputs left to right: with\n2560x1600@2,1920x1080 the second monitor starts at x 1280."
    )]
    Move {
        /// Pixels from the desktop's left edge.
        x: u32,
        /// Pixels from the desktop's top edge.
        y: u32,
    },
    /// Click a button (left, right, middle), optionally at X, Y.
    #[command(
        after_help = "Examples:\n  tilo-vm input click\n  tilo-vm input click right --at 400 300"
    )]
    Click {
        /// left, right or middle.
        #[arg(default_value = "left")]
        button: String,
        /// Click at output pixel X, Y (default: where the pointer is).
        #[arg(long, num_args = 2, value_names = ["X", "Y"])]
        at: Option<Vec<u32>>,
    },
    /// Touchscreen gestures: tap, hold (long-press), drag, pinch.
    #[command(
        subcommand,
        after_help = "Examples:\n  tilo-vm input touch tap 400 300\n  tilo-vm input touch hold 400 300\n  tilo-vm input touch drag 400 600 400 100\n  tilo-vm input touch pinch 640 400 --from 100 --to 400\n\nX and Y are pixels of the slot's first output (its mode, before scaling):\nthe compositor maps a touchscreen or pen that names no output onto the\nprimary output, which is the first --outputs entry."
    )]
    Touch(TouchCommand),
    /// Pen (stylus) on the screen: tap, drag.
    #[command(
        subcommand,
        after_help = "Examples:\n  tilo-vm input pen tap 400 300\n  tilo-vm input pen drag 100 100 500 300 --pressure 80\n\nX and Y are pixels of the slot's first output (its mode, before scaling):\nthe compositor maps a touchscreen or pen that names no output onto the\nprimary output, which is the first --outputs entry."
    )]
    Pen(PenCommand),
    /// Game controller: buttons, d-pad, sticks.
    #[command(subcommand)]
    Gamepad(GamepadCommand),
}

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum TouchCommand {
    /// One finger taps X, Y.
    #[command(after_help = "Examples:\n  tilo-vm input touch tap 400 300")]
    Tap {
        /// Pixels from the output's left edge.
        x: u32,
        /// Pixels from the output's top edge.
        y: u32,
    },
    /// One finger presses X, Y and stays down (long-press).
    #[command(
        after_help = "Examples:\n  tilo-vm input touch hold 400 300\n  tilo-vm input touch hold 400 300 --duration 1500"
    )]
    Hold {
        /// Pixels from the output's left edge.
        x: u32,
        /// Pixels from the output's top edge.
        y: u32,
        /// How long the finger stays down, in milliseconds.
        #[arg(long, default_value_t = 800)]
        duration: u32,
    },
    /// One finger drags from X1, Y1 to X2, Y2 (swipe, scroll, move).
    #[command(
        after_help = "Examples:\n  tilo-vm input touch drag 400 600 400 100\n  tilo-vm input touch drag 0 400 600 400 --duration 150   # fast edge swipe"
    )]
    Drag {
        /// Start, pixels from the left edge.
        x1: u32,
        /// Start, pixels from the top edge.
        y1: u32,
        /// End, pixels from the left edge.
        x2: u32,
        /// End, pixels from the top edge.
        y2: u32,
        /// How long the movement takes, in milliseconds.
        #[arg(long, default_value_t = 300)]
        duration: u32,
    },
    /// Two fingers on a horizontal line through X, Y spread (zoom in) or close (zoom out).
    #[command(
        after_help = "Examples:\n  tilo-vm input touch pinch 640 400                        # zoom in: 100 -> 300 px apart\n  tilo-vm input touch pinch 640 400 --from 400 --to 100    # zoom out"
    )]
    Pinch {
        /// Centre between the fingers.
        x: u32,
        /// Pixels from the output's top edge.
        y: u32,
        /// Finger distance at the start, in pixels.
        #[arg(long, default_value_t = 100)]
        from: u32,
        /// Finger distance at the end, in pixels.
        #[arg(long, default_value_t = 300)]
        to: u32,
        /// How long the pinch takes, in milliseconds.
        #[arg(long, default_value_t = 400)]
        duration: u32,
    },
}

#[derive(Debug, Subcommand)]
pub enum PenCommand {
    /// The pen touches X, Y and lifts.
    #[command(
        after_help = "Examples:\n  tilo-vm input pen tap 400 300\n  tilo-vm input pen tap 400 300 --pressure 90"
    )]
    Tap {
        /// Pixels from the output's left edge.
        x: u32,
        /// Pixels from the output's top edge.
        y: u32,
        /// Tip pressure, 1-100 percent.
        #[arg(long, default_value_t = 50)]
        pressure: u8,
    },
    /// The pen draws a line from X1, Y1 to X2, Y2.
    #[command(after_help = "Examples:\n  tilo-vm input pen drag 100 100 500 300")]
    Drag {
        /// Start, pixels from the left edge.
        x1: u32,
        /// Start, pixels from the top edge.
        y1: u32,
        /// End, pixels from the left edge.
        x2: u32,
        /// End, pixels from the top edge.
        y2: u32,
        /// How long the stroke takes, in milliseconds.
        #[arg(long, default_value_t = 300)]
        duration: u32,
        /// Tip pressure, 1-100 percent.
        #[arg(long, default_value_t = 50)]
        pressure: u8,
    },
}

#[derive(Debug, Subcommand)]
pub enum GamepadCommand {
    /// Press and release buttons in order; `+` holds several at once.
    #[command(
        after_help = "Examples:\n  tilo-vm input gamepad press a\n  tilo-vm input gamepad press guide           # quick settings\n  tilo-vm input gamepad press guide+rb b\n  tilo-vm input gamepad press rt              # triggers pull fully\n\nNames are the Xbox-style printed legends: a b x y lb rb lt rt start select\n(back) guide (home) l3 r3, the d-pad up down left right, and south east west\nnorth for the positional buttons."
    )]
    Press {
        /// Buttons or combos like a, guide+lb, up.
        #[arg(required = true)]
        combos: Vec<String>,
    },
    /// Press d-pad directions in order.
    #[command(after_help = "Examples:\n  tilo-vm input gamepad dpad down down right")]
    Dpad {
        /// up, down, left or right.
        #[arg(required = true)]
        directions: Vec<String>,
    },
    /// Push a stick to X, Y percent (-100..100, +Y is down), hold, let go.
    #[command(
        after_help = "Examples:\n  tilo-vm input gamepad stick left 0 -100               # up\n  tilo-vm input gamepad stick right 100 0 --duration 900"
    )]
    Stick {
        /// left or right.
        stick: String,
        /// -100 (left) to 100 (right).
        #[arg(allow_negative_numbers = true)]
        x: i8,
        /// -100 (up) to 100 (down).
        #[arg(allow_negative_numbers = true)]
        y: i8,
        /// How long the stick is held, in milliseconds.
        #[arg(long, default_value_t = 300)]
        duration: u32,
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
        /// A VM from tilo-vm vm list.
        name: String,
        /// Make it the default for every worktree (vms.toml), not just this one.
        #[arg(long)]
        global: bool,
    },
    /// Open a shell in the VM, or run one command there. Not for deploying.
    #[command(
        after_help = "Examples:\n  tilo-vm vm ssh\n  tilo-vm vm ssh dev\n  tilo-vm vm ssh -- journalctl -b -p warning\n  tilo-vm vm ssh dev -- systemctl --failed\n  tilo-vm vm ssh --raw -- touch /var/tmp/x"
    )]
    Ssh {
        /// Run a command that writes into /usr, /etc, /boot or tilo-vm state anyway (leaves a receipt).
        #[arg(long)]
        raw: bool,
        /// The VM (default: the one this worktree uses).
        name: Option<String>,
        #[arg(long, hide = true, conflicts_with = "name")]
        vm: Option<String>,
        /// The command to run, after `--`; without one, an interactive shell.
        #[arg(last = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Is the VM up, reachable, and running a compatible daemon?
    #[command(after_help = "Examples:\n  tilo-vm vm status\n  tilo-vm vm status shared")]
    Status {
        /// The VM (default: the one this worktree uses).
        name: Option<String>,
    },
    /// Download the published dev disk (or use a local one), create the VM with the host's hypervisor, and register it.
    #[command(
        after_help = "Checks the hypervisor and its tools, --image, the published disk and free space\nbefore it downloads or creates anything, and names the missing piece with its fix.\n\nExamples:\n  tilo-vm vm create dev                       # newest published disk (needs FORGEJO_TOKEN, or fgj/tea login)\n  tilo-vm vm create dev --tag main-1a2b3c4\n  tilo-vm vm create dev --image ~/images/tilo-vm.qcow2 --disk ~/vms/dev.qcow2\n  tilo-vm vm create try1 --image os/output/disk/disk.raw   # a raw disk from os/dev/disk-image.sh --raw\n  tilo-vm vm create esp1 --image base.raw --boot-esp override.raw   # boot a candidate ESP (os/dev/make-boot-esp.sh) first\n  tilo-vm vm create mac --hypervisor utm --template 'Tilo Dev'\n  tilo-vm vm create arm --arch aarch64 --emulated --image arm.qcow2 --disk arm-dev.qcow2\n  tilo-vm vm create 5k --heads 2 --resolution 5120x2880   # 5K first monitor, 1080p-capable second\n  tilo-vm vm create gpu --gl                   # virgl: render on the host GPU"
    )]
    Create(CreateArgs),
    /// Boot the VM and wait until SSH answers.
    #[command(after_help = "Examples:\n  tilo-vm vm start")]
    Start {
        /// The VM (default: the one this worktree uses).
        name: Option<String>,
    },
    /// Shut the VM down cleanly, wait until it is off, start it, and wait for tilo-vmd.
    #[command(after_help = "Examples:\n  tilo-vm vm restart\n  tilo-vm vm restart shared")]
    Restart {
        /// The VM (default: the one this worktree uses).
        name: Option<String>,
    },
    /// Shut the VM down cleanly, once no swap, gc or upgrade is running in it.
    #[command(
        after_help = "Examples:\n  tilo-vm vm stop\n  tilo-vm vm stop --force   # even while another command runs"
    )]
    Stop {
        /// The VM (default: the one this worktree uses).
        name: Option<String>,
        /// Do not wait for the VM lock.
        #[arg(long)]
        force: bool,
    },
    /// Move the VM onto another dev image with bootc and reboot into it; slots survive in /var, and the previous image stays for rollback-image.
    #[command(
        after_help = "Examples:\n  tilo-vm vm upgrade                          # the newest build of the image the VM follows\n  tilo-vm vm upgrade --image registry.tilo.so/tilo/tilo-vm:main-1a2b3c4\n  tilo-vm vm upgrade --image target/tilo-vm/image  # what tilo-vm image build wrote, here or elsewhere\n  tilo-vm vm upgrade --from local             # build this worktree's dev image (cached steps skipped), move onto it\n  tilo-vm vm upgrade --from sha:1a2b3c4       # a CI build of main: no local OS build\n  tilo-vm vm upgrade --from local --rebuild-base\n  tilo-vm vm upgrade --from local:localhost/tilo-vm:mine   # an image already built, no build\n  tilo-vm vm upgrade --from local --no-reboot  # stage only; it takes effect at the next boot\n  tilo-vm vm rollback-image                   # back to the image before\n\n--from local runs os/dev/build-dev-image.sh (TILO_DEV_DISK=0), rebuilds the\nproduction base localhost/tilo:latest only when its inputs changed, sends only\nthe layers the VM lacks (TILO_LOCAL_TAG replaces latest in both image names), then bootc switch and a reboot. --from ci:<tag> or\nsha:<commit> switches to CI's published dev image of that build, or builds one\nhere FROM its integration image (tilo-ci). For Containerfile, unit, /etc and\npackage changes: tilo-vm guide images."
    )]
    Upgrade(UpgradeArgs),
    /// Boot the VM's previous image again (bootc rollback) and reboot; slots survive in /var.
    #[command(
        after_help = "Examples:\n  tilo-vm vm rollback-image\n  tilo-vm vm rollback-image --no-reboot   # takes effect at the next boot\n\nbootc keeps one previous image: the one the VM ran before its last upgrade.\nRolling back twice returns to the newer one."
    )]
    RollbackImage {
        /// The VM (default: the one this worktree uses).
        name: Option<String>,
        /// Queue the rollback for the next boot instead of rebooting now.
        #[arg(long)]
        no_reboot: bool,
    },
    /// Attach to the VM's serial console (works when SSH does not).
    #[command(after_help = "Examples:\n  tilo-vm vm serial")]
    Serial {
        /// The VM (default: the one this worktree uses).
        name: Option<String>,
    },
    /// Screenshot seat0, the VM's real display, through the hypervisor.
    #[command(
        after_help = "Examples:\n  tilo-vm vm shot -o target/vm.png\n\nThis is what the hypervisor window shows: greetd's greeter, or the slot that\nclaimed seat0 (tilo-vm seat claim). Slots on their own seats (headless, or\nheadful on a vkms card) never appear here; use tilo-vm shot for them.\nWorks without SSH."
    )]
    Shot {
        /// The VM (default: the one this worktree uses).
        name: Option<String>,
        /// Where to write the PNG.
        #[arg(short, long, default_value = "target/tilo-vm-vm.png")]
        output: PathBuf,
    },
    /// Delete a VM: unregister it from the hypervisor and remove its disk, firmware variables, logs and key.
    #[command(
        after_help = "Examples:\n  tilo-vm vm rm try1\n  tilo-vm vm rm try1 --force   # power it off first if it is running\n\nThe image it was created from (a published disk in the cache, or --image) is\nkept. A VM tilo-vm did not create (registered by hand) is only\nunregistered."
    )]
    Rm {
        /// The VM to delete (always named: there is no default).
        name: String,
        /// Power the VM off first if it is running.
        #[arg(long)]
        force: bool,
    },
    /// Hard-reset the VM.
    #[command(after_help = "Examples:\n  tilo-vm vm reset")]
    Reset {
        /// The VM (default: the one this worktree uses).
        name: Option<String>,
    },
    /// Pass arguments to the hypervisor's native control tool (virsh, QEMU monitor, utmctl, `PowerShell`).
    #[command(
        after_help = "Examples:\n  tilo-vm vm control dominfo dev\n  tilo-vm vm control --vm ci info block"
    )]
    Control {
        /// The VM (default: the one this worktree uses).
        #[arg(long)]
        vm: Option<String>,
        /// Arguments for the hypervisor's control tool.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// The name to register the new VM under.
    pub name: String,
    /// Default: the host's native one, which its UI shows: libvirt's
    /// `qemu:///session` on Linux (virt-manager, Boxes), utm on macOS, hyperv on
    /// Windows with Hyper-V, and whpx on Windows ARM64 Home. `qemu` is the headless os/dev/vm.sh adapter for CI and
    /// scenario tests.
    #[arg(long)]
    pub hypervisor: Option<String>,
    /// A local disk to create the VM from, instead of downloading one: a
    /// qcow2 dev disk or a raw disk with a GPT. Never written: the VM's disk
    /// is a copy-on-write child. WHPX imports one on a network path
    /// (`\\wsl.localhost\...`) once into the cache first.
    #[arg(long, conflicts_with = "tag")]
    pub image: Option<String>,
    /// A raw ESP disk (os/dev/make-boot-esp.sh) to attach read-only ahead of
    /// the VM's disk: the VM boots its loader and UKI (libvirt, whpx).
    #[arg(long)]
    pub boot_esp: Option<String>,
    /// WHPX: boot a local disk from this companion JSON bundle. Published
    /// disks fetch and verify their matching bundle automatically.
    #[arg(long, conflicts_with = "boot_esp")]
    pub direct_boot_bundle: Option<String>,
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
    /// Guest memory in MiB (default: the adapter's).
    #[arg(long)]
    pub memory_mb: Option<u32>,
    /// WHPX only: omit the virtual display and USB devices for SSH-only development.
    #[arg(long)]
    pub headless: bool,
    /// libvirt only: virtio-gpu outputs, e.g. 2 for a two-monitor layout.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=16))]
    pub heads: Option<u8>,
    /// libvirt only: virgl 3D acceleration on the host GPU, so the desktop
    /// renders on a GPU instead of in software.
    #[arg(long)]
    pub gl: bool,
    /// libvirt only: the first output's preferred mode, `WIDTHxHEIGHT` (e.g. `5120x2880`).
    #[arg(long, value_parser = parse_resolution)]
    pub resolution: Option<String>,
    /// Guest CPUs (default: the adapter's).
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
pub enum ImageCommand {
    /// Build the dev image with os/dev/build-dev-image.sh and write it as an OCI layout (and with --disk a dev disk).
    #[command(
        after_help = "Examples:\n  tilo-vm image build                  # target/tilo-vm/image, the dev image as an OCI layout\n  tilo-vm image build --disk           # and target/tilo-vm/disk.qcow2\n  tilo-vm vm upgrade --image target/tilo-vm/image\n  tilo-vm vm create dev --image target/tilo-vm/disk.qcow2\n\nOn another machine instead of here: run image build there in a synced copy of\nthe worktree (SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) when it has no git),\nbring target/tilo-vm back, then:\n  tilo-vm vm upgrade --image target/tilo-vm/image\n  tilo-vm image disk                   # a disk from it, written here (where loop devices are)\n\nThe build is the one vm upgrade --from local runs: the production base is\nrebuilt only when its inputs changed, podman's layer cache and the builder\nstage's cargo cache skip unchanged steps, and a failed step reruns on its own.\nScratch goes to <out>-build (next to the output). tilo-vm guide images."
    )]
    Build {
        /// Also write the dev disk, <out>/disk.qcow2.
        #[arg(long)]
        disk: bool,
        /// Output directory, relative to the worktree.
        #[arg(long, default_value = "target/tilo-vm")]
        out: std::path::PathBuf,
    },
    /// Write the dev disk from an image image build left, without building: <layout dir>/../disk.qcow2.
    #[command(
        after_help = "Examples:\n  tilo-vm image disk                          # from target/tilo-vm/image\n  tilo-vm vm create dev --image target/tilo-vm/disk.qcow2\n\nFor an image built elsewhere, such as on another machine (tilo-vm image\nbuild --help): the layout goes into this host's podman storage as\nlocalhost/tilo-vm:latest and os/dev/build-image.sh (TILO_SKIP_BUILD=1) installs\nit to a disk, the disk step of the build itself. It needs passwordless sudo\n(loop device and mounts)."
    )]
    Disk {
        /// The OCI layout image build wrote.
        #[arg(long, default_value = "target/tilo-vm/image")]
        image: std::path::PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct UpgradeArgs {
    /// The VM to upgrade (default: the one this worktree uses).
    pub name: Option<String>,
    /// A published dev image to switch to (default: the image the VM follows),
    /// or an OCI layout directory `tilo-vm image build` wrote (target/tilo-vm/image).
    #[arg(long, conflicts_with = "from")]
    pub image: Option<String>,
    /// `local`: build this worktree's dev image (localhost/tilo-vm:latest, the
    /// tag from `TILO_LOCAL_TAG` when set) and move onto it. `local:<image>`:
    /// an image already in podman's storage.
    /// `ci:<tag>` or `sha:<commit>`: a CI build of main (tag `main-<sha7>`): its
    /// published dev image, or a dev image built here FROM its integration image.
    #[arg(long)]
    pub from: Option<String>,
    /// With `--from local`: build FROM this production image as it is,
    /// instead of localhost/tilo:latest rebuilt when its inputs changed.
    #[arg(long, requires = "from", conflicts_with = "rebuild_base")]
    pub base: Option<String>,
    /// With `--from local`: rebuild the production base even if its inputs look unchanged.
    #[arg(long, requires = "from")]
    pub rebuild_base: bool,
    /// Go ahead when the new image's production base is older than the VM's.
    #[arg(long)]
    pub allow_older_base: bool,
    /// Stage the image for the next boot instead of rebooting now.
    #[arg(long)]
    pub no_reboot: bool,
}

#[derive(Debug, Subcommand)]
pub enum SlotCommand {
    /// Create a slot (default: named after your branch) and start its session.
    #[command(
        after_help = "Examples:\n  tilo-vm slot create --headless\n  tilo-vm slot create review-a --headful\n  tilo-vm slot create apps --headful --identity\n  tilo-vm slot create dual --headful --outputs 2560x1600@2,1920x1080@1\n\n--identity mints a throwaway test identity inside the new session, through\na control op only dev-image shells carry, and restarts the session with no\ndesktop layout, so apps open at once. It never replaces a real identity.\n\n--outputs gives a headful slot one virtual monitor per entry, left to right,\ntop-aligned, the first one primary. Each is WIDTHxHEIGHT[@SCALE]. Like a\nmonitor without an EDID, a vkms output offers the standard VESA (DMT) 60 Hz\nmodes, e.g. 1280x800, 1920x1080, 1920x1200, 2560x1600, 4096x2160; a size it\ndoes not offer fails with the list it does. SCALE is 0.5 to 4 (default 1). The mode and scale go\nthrough the display settings exactly as a confirmed change in Settings does,\nand are remembered for that set of monitors. Outputs are fixed when the slot\nis created; rerunning the same create applies the layout again.\n\ntilo-vm input move X Y then takes desktop coordinates in logical pixels:\nwith 2560x1600@2,1920x1080 the first monitor is x 0-1279, the second\nx 1280-3199."
    )]
    Create {
        /// The slot (default: your slot).
        name: Option<String>,
        /// The slot, as a flag (same as the positional; every slot command takes `--slot`).
        #[arg(long = "slot", value_name = "SLOT", conflicts_with = "name")]
        slot_flag: Option<String>,
        /// Offscreen compositor, driven by control and shot (default).
        #[arg(long, conflicts_with = "headful")]
        headless: bool,
        /// Own virtual display and seat: the real DRM and input paths.
        #[arg(long)]
        headful: bool,
        /// Headful only: virtual monitors left to right, each width x height with an optional @scale, e.g. 2560x1600@2,1920x1080.
        #[arg(
            long,
            requires = "headful",
            value_name = "LIST",
            value_delimiter = ',',
            num_args = 1
        )]
        outputs: Vec<tilo_vm_protocol::OutputSpec>,
        /// Give the slot's user a throwaway test identity, so apps open
        /// without the first-login flow, and start the session with fresh
        /// desktop state. Needs a dev image's shell.
        #[arg(long)]
        identity: bool,
    },
    /// List the VM's slots with owner, generation and session state.
    #[command(after_help = "Examples:\n  tilo-vm slot list\n  tilo-vm slot list --json")]
    List,
    /// Show a slot's generations and every file its layers add, with SHA-256 and source layer.
    #[command(
        after_help = "Examples:\n  tilo-vm slot show\n  tilo-vm slot show --slot review-a\n  tilo-vm slot show --json | jq '.files[] | select(.path == \"/usr/bin/shell\")'"
    )]
    Show {
        /// The slot (default: your slot).
        name: Option<String>,
        /// The slot, as a flag (same as the positional; every slot command takes `--slot`).
        #[arg(long = "slot", value_name = "SLOT", conflicts_with = "name")]
        slot_flag: Option<String>,
    },
    /// Run a command as the slot sees the system: its layers over /usr, as its user.
    #[command(
        after_help = "Examples:\n  tilo-vm slot exec -- sha256sum /usr/bin/shell\n  tilo-vm slot exec --slot review-a -- shell --version\n  tilo-vm slot exec -- sh -c 'ls -l /usr/lib/tilo | head'\n\nThe command runs in the mount namespace of the slot's running session, as\ntds-<slot>, so it sees exactly what the session sees. Output streams; the\ncommand is stopped after --timeout seconds (exit 124) or 1 MiB of output\n(exit 125). Its exit status is passed on."
    )]
    Exec {
        /// The slot (default: your slot).
        name: Option<String>,
        /// The slot, as a flag (same as the positional; every slot command takes `--slot`).
        #[arg(long = "slot", value_name = "SLOT", conflicts_with = "name")]
        slot_flag: Option<String>,
        /// Seconds before the command is stopped.
        #[arg(long, default_value_t = tilo_vm_protocol::EXEC_TIMEOUT_SECONDS)]
        timeout: u64,
        /// The command, after `--`.
        #[arg(last = true, required = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Restart a slot's session on its current generation (also repairs an interrupted swap).
    #[command(after_help = "Examples:\n  tilo-vm slot restart\n  tilo-vm slot restart --slot vm")]
    Restart {
        /// The slot (default: your slot).
        name: Option<String>,
        /// The slot, as a flag (same as the positional; every slot command takes `--slot`).
        #[arg(long = "slot", value_name = "SLOT", conflicts_with = "name")]
        slot_flag: Option<String>,
    },
    /// Third-party apps for checking desktop features: X11 apps, GTK, dialogs, tray, MIME handlers.
    #[command(subcommand)]
    Apps(SlotAppsCommand),
    /// Remove a slot: its session, user and generations (layers stay until gc).
    #[command(
        after_help = "Examples:\n  tilo-vm slot rm review-a\n  tilo-vm slot rm --slot review-a"
    )]
    Rm {
        /// The slot (default: your slot).
        name: Option<String>,
        /// The slot, as a flag (same as the positional; every slot command takes `--slot`).
        #[arg(long = "slot", value_name = "SLOT", conflicts_with = "name")]
        slot_flag: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum SlotAppsCommand {
    /// List the app sets and what each app checks.
    #[command(after_help = "Examples:\n  tilo-vm slot apps list\n  tilo-vm slot apps list --json")]
    List,
    /// Install an app set with the VM's package manager, then restart the slot's session so it sees it.
    #[command(
        after_help = "Examples:\n  tilo-vm slot apps install --set verification\n  tilo-vm slot apps install --set verification --slot review-a\n\nThe packages come from the distribution, not from Tilo: tilo-vmd runs the\nVM's own install line (dnf install --transient on the dev image), so they\nland in a transient overlay on /usr that the next VM reboot discards; run\nthis again after a reboot. No image ever carries them. The VM needs network\naccess to the distribution's mirrors. Every slot on the VM can use them\nafter its session restarts; this command restarts the slot's session.\n\nThe sets and their apps are listed by tilo-vm slot apps list (the data is\ncrates/dev/tilo-vm/data/app-sets.toml)."
    )]
    Install {
        /// The slot whose session restarts to see the apps (default: your slot).
        name: Option<String>,
        /// The slot, as a flag (same as the positional; every slot command takes `--slot`).
        #[arg(long = "slot", value_name = "SLOT", conflicts_with = "name")]
        slot_flag: Option<String>,
        /// The app set to install (tilo-vm slot apps list).
        #[arg(long, value_name = "SET")]
        set: String,
        /// Seconds before the install is stopped.
        #[arg(long, default_value_t = tilo_vm_protocol::INSTALL_TIMEOUT_SECONDS)]
        timeout: u64,
    },
}

#[derive(Debug, Subcommand)]
pub enum SeatCommand {
    /// Move a slot's session onto seat0, the display the hypervisor window shows.
    #[command(
        after_help = "Examples:\n  tilo-vm seat claim\n  tilo-vm seat claim review-a --steal"
    )]
    Claim {
        /// The slot (default: your slot).
        slot: Option<String>,
        /// The slot, as a flag (same as the positional; every slot command takes `--slot`).
        #[arg(long = "slot", value_name = "SLOT", conflicts_with = "slot")]
        slot_flag: Option<String>,
        /// Take seat0 even if another slot holds it.
        #[arg(long)]
        steal: bool,
    },
    /// Give seat0 back to greetd; the slot returns to its own seat.
    #[command(after_help = "Examples:\n  tilo-vm seat release")]
    Release {
        /// The slot (default: your slot).
        slot: Option<String>,
        /// The slot, as a flag (same as the positional; every slot command takes `--slot`).
        #[arg(long = "slot", value_name = "SLOT", conflicts_with = "slot")]
        slot_flag: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn slot_create_takes_an_opt_in_identity() {
        let identity = |args: &[&str]| {
            let cli = Cli::try_parse_from(args).unwrap();
            let Some(Command::Slot(SlotCommand::Create { identity, .. })) = cli.command else {
                panic!("slot create did not parse");
            };
            identity
        };
        assert!(identity(&[
            "tilo-vm",
            "slot",
            "create",
            "--headful",
            "--identity"
        ]));
        assert!(!identity(&["tilo-vm", "slot", "create", "--headful"]));
    }

    #[test]
    fn outputs_need_a_headful_slot_and_parse_left_to_right() {
        let cli = Cli::try_parse_from([
            "tilo-vm",
            "slot",
            "create",
            "--headful",
            "--outputs",
            "2560x1600@2,1920x1080",
        ])
        .unwrap();
        let Some(Command::Slot(SlotCommand::Create { outputs, .. })) = cli.command else {
            panic!("slot create did not parse");
        };
        assert_eq!(
            tilo_vm_protocol::display::format_outputs(&outputs),
            "2560x1600@2,1920x1080@1"
        );
        assert!(
            Cli::try_parse_from(["tilo-vm", "slot", "create", "--outputs", "800x600"]).is_err()
        );
        let error = Cli::try_parse_from([
            "tilo-vm",
            "slot",
            "create",
            "--headful",
            "--outputs",
            "800x600@9",
        ])
        .unwrap_err()
        .to_string();
        assert!(error.contains("outside 0.5 to 4"), "{error}");
    }

    /// Every slot command takes the slot flag, every argument says what it is, ages read like durations,
    /// every subcommand has a purpose and examples, and only state-changing or long commands write receipts.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn every_command_documents_its_arguments_purpose_and_receipts() {
        // Phase: every_slot_command_takes_the_slot_flag.
        {
            for args in [
                &["tilo-vm", "rollback", "--slot", "a"][..],
                &["tilo-vm", "logs", "--slot", "a"],
                &["tilo-vm", "shot", "--slot", "a"],
                &["tilo-vm", "seat", "claim", "--slot", "a"],
                &["tilo-vm", "seat", "release", "--slot", "a"],
                &["tilo-vm", "slot", "create", "--slot", "a"],
                &["tilo-vm", "slot", "restart", "--slot", "a"],
                &["tilo-vm", "slot", "rm", "--slot", "a"],
                &["tilo-vm", "slot", "show", "--slot", "a"],
                &["tilo-vm", "slot", "exec", "--slot", "a", "--", "true"],
                &[
                    "tilo-vm", "slot", "apps", "install", "--set", "v", "--slot", "a",
                ],
                &["tilo-vm", "slot", "show", "a"],
            ] {
                let cli = Cli::try_parse_from(args).unwrap().normalized();
                let slot = match cli.command {
                    Some(
                        Command::Rollback { slot, .. }
                        | Command::Logs { slot, .. }
                        | Command::Shot { slot, .. }
                        | Command::Seat(
                            SeatCommand::Claim { slot, .. } | SeatCommand::Release { slot, .. },
                        )
                        | Command::Slot(
                            SlotCommand::Create { name: slot, .. }
                            | SlotCommand::Restart { name: slot, .. }
                            | SlotCommand::Rm { name: slot, .. }
                            | SlotCommand::Show { name: slot, .. }
                            | SlotCommand::Exec { name: slot, .. }
                            | SlotCommand::Apps(SlotAppsCommand::Install { name: slot, .. }),
                        ),
                    ) => slot,
                    other => panic!("{other:?}"),
                };
                assert_eq!(slot.as_deref(), Some("a"), "{args:?}");
            }
            assert!(Cli::try_parse_from(["tilo-vm", "rollback", "a", "--slot", "b"]).is_err());
            assert!(Cli::try_parse_from(["tilo-vm", "slot", "rm", "a", "--slot", "b"]).is_err());
            for args in [
                &["tilo-vm", "swap", "--slot", "a", "shell"][..],
                &["tilo-vm", "input", "--slot", "a", "key", "super"],
                &["tilo-vm", "control", "--slot", "a", "actions", "list"],
                &["tilo-vm", "status", "--slot", "a"],
            ] {
                Cli::try_parse_from(args).unwrap();
            }
        }

        // Phase: every_argument_says_what_it_is.
        {
            fn walk(command: &clap::Command, path: &str) {
                for arg in command.get_arguments() {
                    let id = arg.get_id().as_str();
                    if ["help", "version"].contains(&id) || arg.is_hide_set() {
                        continue;
                    }
                    assert!(
                        arg.get_help().is_some() || arg.get_long_help().is_some(),
                        "{path}: argument {id} has no description"
                    );
                }
                for sub in command.get_subcommands() {
                    walk(sub, &format!("{path} {}", sub.get_name()));
                }
            }
            walk(&Cli::command(), "tilo-vm");
        }

        // Phase: ages_read_like_durations.
        {
            assert_eq!(parse_age("0"), Ok(0));
            assert_eq!(parse_age("90s"), Ok(90));
            assert_eq!(parse_age("30m"), Ok(1800));
            assert_eq!(parse_age("1h"), Ok(3600));
            assert_eq!(parse_age("2d"), Ok(172_800));
            assert!(parse_age("1w").is_err());
            assert!(parse_age("soon").is_err());
            assert_eq!(parse_resolution("5120x2880"), Ok("5120x2880".into()));
            assert!(parse_resolution("5120").is_err());
            assert!(parse_resolution("100x100").is_err());
            assert!(parse_resolution("5120X2880").is_err());
            let cli = Cli::try_parse_from(["tilo-vm", "gc", "--min-age", "0"]).unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::Gc {
                    min_age: Some(0),
                    ..
                })
            ));
        }

        // Phase: every_subcommand_has_a_purpose_and_examples.
        {
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

        // Phase: only_state_changing_or_long_commands_write_receipts.
        {
            let parse = |args: &[&str]| {
                let mut all = vec!["tilo-vm"];
                all.extend_from_slice(args);
                Cli::try_parse_from(all).unwrap().command
            };
            for quiet in [
                &[][..],
                &["status"],
                &["doctor"],
                &["guide", "quickstart"],
                &["vm", "list"],
                &["vm", "status"],
                &["slot", "list"],
                &["slot", "show"],
                &["slot", "exec", "--", "true"],
                &["logs"],
                &["vm", "ssh"],
                &["vm", "ssh", "--", "systemctl", "status", "tilo-vmd"],
                &["scenario", "install", "--dry-run"],
            ] {
                assert!(!writes_receipt(parse(quiet).as_ref()), "{quiet:?}");
            }
            // --raw is the one vm ssh that changes the VM behind tilo-vm.
            assert!(writes_receipt(
                parse(&["vm", "ssh", "--raw", "--", "podman", "login"]).as_ref()
            ));
            for loud in [
                &["swap", "shell"][..],
                &["vm", "create", "dev"],
                &["vm", "start"],
                &["slot", "create"],
                &["rollback"],
                &["gc"],
                &["seat", "claim"],
                &["shot"],
                &["vm", "restart"],
                &["scenario", "install"],
            ] {
                assert!(writes_receipt(parse(loud).as_ref()), "{loud:?}");
            }
        }
    }

    /// Swap usage shows the slot as an option and swap takes an optional slot; the install scenario takes
    /// a medium, a layout and an unlock; upgrade takes one source and base options only with local; vm ssh
    /// takes the VM like every other VM command.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn command_shapes_take_exactly_their_documented_options() {
        // Phase: swap_usage_shows_the_slot_as_an_option.
        {
            let mut command = Cli::command();
            let swap = command.find_subcommand_mut("swap").unwrap();
            let usage = swap.render_usage().to_string();
            assert!(usage.contains("[OPTIONS] <COMPONENT>"), "{usage}");
            assert!(!usage.contains("<SLOT>"), "{usage}");
            let help = swap.render_long_help().to_string();
            assert!(help.contains("--slot <SLOT>"), "{help}");
            assert!(
                help.contains("--profile dev compiles much faster"),
                "{help}"
            );
            assert!(help.contains("dev-overrides"), "{help}");
        }

        // Phase: swap_takes_an_optional_slot.
        {
            let cli = Cli::try_parse_from(["tilo-vm", "swap", "shell"]).unwrap();
            assert!(
                matches!(cli.command, Some(Command::Swap { ref component, slot: None, .. }) if component == "shell")
            );
            let cli = Cli::try_parse_from([
                "tilo-vm", "--json", "swap", "--slot", "vm", "fs", "--from", "layer:ab",
            ])
            .unwrap();
            assert!(cli.json);
            assert!(
                matches!(cli.command, Some(Command::Swap { ref component, slot: Some(ref slot), ref from, builder: None, profile: None, direct: false, jobs: None, cmdline: None }) if component == "fs" && slot == "vm" && from == "layer:ab")
            );
            assert!(Cli::try_parse_from(["tilo-vm", "swap", "vm", "fs"]).is_err());
            let cli =
                Cli::try_parse_from(["tilo-vm", "swap", "shell", "--profile", "dev"]).unwrap();
            assert!(
                matches!(cli.command, Some(Command::Swap { profile: Some(ref p), .. }) if p == "dev")
            );
            assert!(Cli::try_parse_from(["tilo-vm", "swap", "kernel", "--cmdline", "ro"]).is_err());
            let cli = Cli::try_parse_from(["tilo-vm", "swap", "kernel", "--direct", "--jobs", "4"])
                .unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::Swap {
                    direct: true,
                    jobs: Some(4),
                    ..
                })
            ));
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

        // Phase: the_install_scenario_takes_a_medium_a_layout_and_an_unlock.
        {
            let cli = Cli::try_parse_from([
                "tilo-vm",
                "scenario",
                "install",
                "--medium",
                "/m/installer.qcow2",
                "--layout",
                "guided-alongside",
                "--encrypt",
                "tpm-pin",
                "--graphics",
                "software",
                "--filesystem",
                "ext4",
                "--skip-target-boot",
            ])
            .unwrap();
            let Some(Command::Scenario(ScenarioCommand::Install(args))) = cli.command else {
                panic!("not the install scenario");
            };
            assert_eq!(
                args.medium.as_deref(),
                Some(std::path::Path::new("/m/installer.qcow2"))
            );
            assert_eq!(args.layout, crate::scenario::Layout::GuidedAlongside);
            assert_eq!(args.encrypt, crate::scenario::Unlock::TpmPin);
            assert_eq!(args.graphics, crate::scenario::Graphics::Software);
            assert_eq!(args.filesystem.as_deref(), Some("ext4"));
            assert!(args.skip_target_boot);
            assert_eq!(args.install_timeout, 3600);
            let defaults = Cli::try_parse_from(["tilo-vm", "scenario", "install"]).unwrap();
            let Some(Command::Scenario(ScenarioCommand::Install(args))) = defaults.command else {
                panic!("not the install scenario");
            };
            assert_eq!(args.layout, crate::scenario::Layout::Erase);
            assert_eq!(args.encrypt, crate::scenario::Unlock::Passphrase);
            assert!(args.filesystem.is_none());
            assert!(!args.skip_target_boot);
            assert!(
                Cli::try_parse_from(["tilo-vm", "scenario", "install", "--layout", "shrink"])
                    .is_err()
            );
            let retained = Cli::try_parse_from([
                "tilo-vm",
                "scenario",
                "install",
                "--username",
                "bob",
                "--credentials-stdin",
            ])
            .unwrap();
            let Some(Command::Scenario(ScenarioCommand::Install(args))) = retained.command else {
                panic!("not the install scenario");
            };
            assert_eq!(args.username.as_deref(), Some("bob"));
            assert!(args.credentials_stdin);
            for invalid in [
                vec!["tilo-vm", "scenario", "install", "--username", "bob"],
                vec!["tilo-vm", "scenario", "install", "--credentials-stdin"],
                vec![
                    "tilo-vm",
                    "scenario",
                    "install",
                    "--username",
                    "bob",
                    "--credentials-stdin",
                    "--dry-run",
                ],
            ] {
                assert!(Cli::try_parse_from(invalid).is_err());
            }
        }

        // Phase: upgrade_takes_one_source_and_base_options_only_with_local.
        {
            let cli = Cli::try_parse_from([
                "tilo-vm",
                "vm",
                "upgrade",
                "--from",
                "local",
                "--rebuild-base",
            ])
            .unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::Vm(VmCommand::Upgrade(UpgradeArgs { ref from, rebuild_base: true, no_reboot: false, .. }))) if from.as_deref() == Some("local")
            ));
            assert!(
                Cli::try_parse_from([
                    "tilo-vm", "vm", "upgrade", "--from", "local", "--image", "x"
                ])
                .is_err()
            );
            assert!(Cli::try_parse_from(["tilo-vm", "vm", "upgrade", "--rebuild-base"]).is_err());
            assert!(
                Cli::try_parse_from([
                    "tilo-vm",
                    "vm",
                    "upgrade",
                    "--from",
                    "local",
                    "--base",
                    "b",
                    "--rebuild-base"
                ])
                .is_err()
            );
            let cli =
                Cli::try_parse_from(["tilo-vm", "vm", "rollback-image", "dev", "--no-reboot"])
                    .unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::Vm(VmCommand::RollbackImage { ref name, no_reboot: true })) if name.as_deref() == Some("dev")
            ));
            let mut command = Cli::command();
            let help = command
                .find_subcommand_mut("vm")
                .and_then(|vm| vm.find_subcommand_mut("upgrade"))
                .unwrap()
                .render_long_help()
                .to_string();
            assert!(help.contains("tilo-vm guide images"), "{help}");
            assert!(help.contains("--from sha:1a2b3c4"), "{help}");
            let cli =
                Cli::try_parse_from(["tilo-vm", "vm", "upgrade", "--from", "ci:main-1a2b3c4"])
                    .unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::Vm(VmCommand::Upgrade(UpgradeArgs { ref from, .. }))) if from.as_deref() == Some("ci:main-1a2b3c4")
            ));
        }

        // Phase: vm_ssh_takes_the_vm_like_every_other_vm_command.
        {
            let words = |w: &[&str]| w.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
            assert_eq!(
                vm_ssh(&["probe", "--", "true"]),
                (Some("probe".into()), None, words(&["true"]))
            );
            assert_eq!(
                vm_ssh(&["--", "journalctl", "-b"]),
                (None, None, words(&["journalctl", "-b"]))
            );
            assert_eq!(vm_ssh(&[]), (None, None, vec![]));
            assert_eq!(
                vm_ssh(&["--vm", "ci", "--", "true"]),
                (None, Some("ci".into()), words(&["true"]))
            );
            // A command needs `--`, so a VM name is never run as one.
            assert!(Cli::try_parse_from(["tilo-vm", "vm", "ssh", "probe", "true"]).is_err());
        }
    }

    fn vm_ssh(words: &[&str]) -> (Option<String>, Option<String>, Vec<String>) {
        let mut line = vec!["tilo-vm", "vm", "ssh"];
        line.extend_from_slice(words);
        match Cli::try_parse_from(line).unwrap().command {
            Some(Command::Vm(VmCommand::Ssh {
                name, vm, command, ..
            })) => (name, vm, command),
            other => panic!("{other:?}"),
        }
    }
}
