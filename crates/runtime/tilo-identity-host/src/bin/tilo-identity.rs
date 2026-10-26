//! `tilo-identity`: this account's Tilo identity from a shell, for headless
//! devices, SSH sessions and agents.
//!
//! It is another door to the steps onboarding, Settings and the shell's
//! `identity.*` actions take, never a second implementation: `create` mints
//! through [`provisioning::ensure_collaboration_identity`](tilo_identity_host::provisioning::ensure_collaboration_identity) and writes the
//! backup onboarding writes; `pair` runs `tilo-enroll pair-device` as
//! first-use onboarding does; `approve` discovers and admits a device with
//! the programs and arguments of [`device_admin`](tilo_identity_host::device_admin); `asking`, `approve` and
//! `decline` answer a device another of the person's devices found through
//! the session's `identity.device.pair.asking` and `.answer` actions, as its
//! notification does; `devices` and `revoke` are the shell's identity steps.
//!
//! Every command takes `--json` and then prints one JSON object on stdout,
//! errors included (`{"error": ..., "next": ...}`).

#[cfg(unix)]
mod unix {
    use std::io::{BufRead as _, Read as _, Write as _};
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode, Stdio};

    use anyhow::Context as _;
    use clap::{Parser, Subcommand, ValueEnum};
    use serde_json::{Value, json};
    use tilo_actions::remote::identity_admin::FleetDevice;
    use tilo_identity_host::device_admin::{self, Approval, PairRole, PersonPaths};
    use tilo_identity_host::device_fleet::DeviceFleetStore;
    use tilo_identity_host::provisioning::{self, IdentityStorage, ProvisionOutcome};
    use tilo_identity_host::registry::{IdentityProfile, IdentityRegistryStore};
    use tilo_user_identity::{DeviceCert, IdentityPaths};

    /// How long `pair` and `approve` wait at most, as the pairing window
    /// `tilo-enroll` keeps open.
    const PAIRING_WINDOW_SECS: u64 = 270;

    #[derive(Parser)]
    #[command(
        name = "tilo-identity",
        about = "Manage this account's Tilo identity and the devices that share it",
        long_about = "Manage this account's Tilo identity and the devices that share it.\n\n\
            New device, no identity anywhere yet:   tilo-identity create ...\n\
            New device, identity on another device: tilo-identity pair     (here)\n\
            \x20                                       tilo-identity approve  (there)\n\
            Another of your devices asks you:       tilo-identity asking\n\
            Check:                                  tilo-identity status\n\n\
            Run it as the account that owns the identity, not with sudo; a step that\n\
            needs an administrator asks for one itself."
    )]
    struct Cli {
        /// Print one JSON object on stdout instead of text.
        #[arg(long, global = true)]
        json: bool,
        #[command(subcommand)]
        command: Cmd,
    }

    #[derive(Subcommand)]
    enum Cmd {
        /// This account's identity, this device and the identity's devices.
        Status,
        /// Create a new identity here, as first-use onboarding does.
        ///
        /// Nobody can recover an identity: losing every device that holds
        /// it and its backup loses it for good. Pass
        /// --acknowledge-no-recovery-service to confirm, and either
        /// --backup PATH or --no-backup-i-understand.
        Create {
            /// Confirms that nobody can recover the identity for you.
            #[arg(long)]
            acknowledge_no_recovery_service: bool,
            /// Write the passphrase-encrypted backup onboarding offers here.
            /// The file must not exist yet.
            #[arg(long, value_name = "PATH", conflicts_with = "no_backup_i_understand")]
            backup: Option<PathBuf>,
            /// A file holding the backup passphrase (12 characters or more);
            /// `-` reads it from stdin. Without it the terminal asks.
            #[arg(long, value_name = "FILE", requires = "backup")]
            passphrase_file: Option<PathBuf>,
            /// Create the identity without a backup.
            #[arg(long)]
            no_backup_i_understand: bool,
        },
        /// Write the passphrase-encrypted backup of this identity.
        Backup {
            /// Where to write it; the file must not exist yet.
            #[arg(long, value_name = "PATH")]
            to: PathBuf,
            /// A file holding the passphrase; `-` reads it from stdin.
            #[arg(long, value_name = "FILE")]
            passphrase_file: Option<PathBuf>,
        },
        /// Join an identity another device on this network holds.
        ///
        /// Shows a code and waits; on the other device run
        /// `tilo-identity approve --code CODE`.
        Pair {
            /// How this device is listed on the identity's other devices.
            #[arg(long, default_value = "Tilo device")]
            label: String,
            /// Seconds to wait for the approval.
            #[arg(long, default_value_t = PAIRING_WINDOW_SECS)]
            timeout: u64,
        },
        /// Approve the device waiting in `tilo-identity pair`: one another of
        /// your devices found and asks you about (see `asking`), or else one
        /// on this network.
        Approve {
            /// Approve only if the waiting device shows this code.
            #[arg(long, value_name = "CODE")]
            code: Option<String>,
            /// Approve only this device: its key, or the start of it.
            #[arg(long, value_name = "KEY")]
            device: Option<String>,
            /// Seconds to listen for the waiting device.
            #[arg(long, default_value_t = 15)]
            timeout: u64,
            /// How to ask for an administrator.
            #[arg(long, value_enum, default_value_t = Elevate::Auto)]
            elevate: Elevate,
        },
        /// Decline a device another of your devices found and asks you about.
        Decline {
            /// Decline the device that shows this code.
            #[arg(long, value_name = "CODE")]
            code: Option<String>,
            /// Decline this device: its key, or the start of it.
            #[arg(long, value_name = "KEY")]
            device: Option<String>,
        },
        /// The new devices your other devices found, asking you here to let
        /// them be you.
        Asking,
        /// The devices this identity trusts.
        Devices,
        /// Revoke a device: it can no longer act as this identity.
        Revoke {
            /// The device's key, or the start of it, as `devices` lists it.
            device: String,
            /// lost, stolen, retired or compromised.
            #[arg(long, default_value = "retired")]
            reason: String,
            /// Confirms the revocation, which cannot be undone.
            #[arg(long)]
            yes: bool,
        },
    }

    #[derive(Clone, Copy, ValueEnum)]
    enum Elevate {
        /// polkit in a desktop session, sudo anywhere else.
        Auto,
        Sudo,
        Pkexec,
    }

    /// An error that says what to do about it.
    #[derive(Debug)]
    struct Guided {
        message: String,
        next: String,
    }

    impl std::fmt::Display for Guided {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.message)
        }
    }

    impl std::error::Error for Guided {}

    fn guided(message: impl Into<String>, next: impl Into<String>) -> anyhow::Error {
        Guided {
            message: message.into(),
            next: next.into(),
        }
        .into()
    }

    const CREATE_OR_PAIR: &str = "create one with `tilo-identity create`, or join one from \
                                  another device with `tilo-identity pair`";

    /// Where this account's identity and this device's keys live.
    struct Context {
        json: bool,
        person: PersonPaths,
        device: IdentityPaths,
    }

    impl Context {
        fn fleets(&self) -> DeviceFleetStore {
            DeviceFleetStore::at(&self.person.dir, self.device.clone())
        }

        fn emit(&self, value: &Value, text: &str) {
            if self.json {
                println!("{value}");
            } else {
                println!("{text}");
            }
        }

        /// The identity this account acts as, if it has one.
        fn identity(&self) -> anyhow::Result<Option<IdentityProfile>> {
            let registry = IdentityRegistryStore::at(&self.person.dir)
                .load_reconciled()
                .with_context(|| format!("read the identities in {}", self.person.dir.display()))?;
            Ok(registry
                .default_identity
                .as_ref()
                .and_then(|id| registry.identities.get(id))
                .cloned())
        }

        fn require_identity(&self) -> anyhow::Result<IdentityProfile> {
            self.identity()?
                .ok_or_else(|| guided("this account has no identity", CREATE_OR_PAIR))
        }

        /// This device's valid certificate for `identity`, if it holds one.
        fn device_cert(&self, identity: &IdentityProfile) -> Option<DeviceCert> {
            tilo_user_identity::persist::read_device_cert(
                &self.device.device_cert_for_user(&identity.user_public_key),
            )
            .ok()
            .filter(|cert| {
                cert.user_public_key == identity.user_public_key
                    && tilo_user_identity::verify_device_cert_at(cert, tilo_clock::unix_now_ns())
                        .is_ok()
            })
        }

        fn devices(&self, identity: &IdentityProfile) -> anyhow::Result<Vec<FleetDevice>> {
            device_admin::trusted_devices(
                &self.fleets(),
                &self.device,
                &identity.id,
                identity.user_public_key,
            )
            .map_err(anyhow::Error::msg)
            .context("read the identity's devices")
        }
    }

    pub fn main() -> ExitCode {
        let cli = Cli::parse();
        let json = cli.json;
        let result = refuse_root()
            .and_then(|()| PersonPaths::from_env().map_err(anyhow::Error::msg))
            .and_then(|person| {
                let context = Context {
                    json,
                    person,
                    device: provisioning::collaboration_identity_paths(),
                };
                run(&context, cli.command)
            });
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let next = error
                    .downcast_ref::<Guided>()
                    .map(|guided| guided.next.clone());
                if json {
                    println!("{}", json!({ "error": format!("{error:#}"), "next": next }));
                } else {
                    eprintln!("error: {error:#}");
                    if let Some(next) = next {
                        eprintln!("next: {next}");
                    }
                }
                ExitCode::FAILURE
            }
        }
    }

    fn run(context: &Context, command: Cmd) -> anyhow::Result<()> {
        match command {
            Cmd::Status => status(context),
            Cmd::Create {
                acknowledge_no_recovery_service,
                backup,
                passphrase_file,
                no_backup_i_understand,
            } => create(
                context,
                acknowledge_no_recovery_service,
                backup.as_deref(),
                passphrase_file.as_deref(),
                no_backup_i_understand,
            ),
            Cmd::Backup {
                to,
                passphrase_file,
            } => {
                let identity = context.require_identity()?;
                let passphrase = backup_passphrase(passphrase_file.as_deref())?;
                write_backup(&identity, &to, &passphrase)?;
                context.emit(
                    &json!({ "backup": to, "identity": identity.id }),
                    &format!(
                        "Wrote the encrypted backup to {}. Keep a copy off this device.",
                        to.display()
                    ),
                );
                Ok(())
            }
            Cmd::Pair { label, timeout } => pair(context, &label, timeout),
            Cmd::Approve {
                code,
                device,
                timeout,
                elevate,
            } => {
                if answer_asking(context, code.as_deref(), device.as_deref())? {
                    return Ok(());
                }
                approve(
                    context,
                    code.as_deref(),
                    device.as_deref(),
                    timeout,
                    elevate,
                )
            }
            Cmd::Decline { code, device } => {
                let mut session = Session::connect()?;
                let ask = pick(session.asking()?, code.as_deref(), device.as_deref())?.ok_or_else(
                    || {
                        guided(
                            "no device asks you here",
                            "see who asks with `tilo-identity asking`",
                        )
                    },
                )?;
                session.answer(context, &ask, false)
            }
            Cmd::Asking => asking(context),
            Cmd::Devices => {
                let identity = context.require_identity()?;
                let devices = context.devices(&identity)?;
                context.emit(
                    &json!({ "identity": identity.id, "devices": devices }),
                    &devices.iter().map(describe).collect::<Vec<_>>().join("\n"),
                );
                Ok(())
            }
            Cmd::Revoke {
                device,
                reason,
                yes,
            } => revoke(context, &device, &reason, yes),
        }
    }

    fn refuse_root() -> anyhow::Result<()> {
        if rustix::process::geteuid().is_root() {
            return Err(guided(
                "this runs as the account that owns the identity, not as root",
                "run it again as that account, without sudo; it asks for an administrator \
                 itself when a step needs one",
            ));
        }
        Ok(())
    }

    fn describe(device: &FleetDevice) -> String {
        let state = if device.revoked {
            "revoked"
        } else if device.current {
            "this device"
        } else if device.can_host {
            "home server"
        } else {
            "device"
        };
        format!("  {:<24} {}  {state}", device.label, device.key_hex)
    }

    fn status(context: &Context) -> anyhow::Result<()> {
        let Some(identity) = context.identity()? else {
            context.emit(
                &json!({
                    "identity": null,
                    "identity_dir": context.person.dir,
                    "next": [CREATE_OR_PAIR],
                }),
                &format!(
                    "No identity in {}.\nnext: {CREATE_OR_PAIR}",
                    context.person.dir.display()
                ),
            );
            return Ok(());
        };
        let cert = context.device_cert(&identity);
        let devices = context.devices(&identity)?;
        let mut next = Vec::new();
        if cert.is_none() {
            next.push(
                "this device holds no valid certificate for the identity; log in again so the \
                 session takes it up, or pair this device with `tilo-identity pair`",
            );
        }
        let this_device = cert.as_ref().map(|cert| {
            json!({
                "device": hex::encode(cert.device_public_key),
                "label": cert.device_label,
                "can_approve_devices": cert.capabilities.can_admit_devices,
            })
        });
        let mut text = vec![
            format!(
                "Identity  {}{}",
                identity.id,
                if identity.display_name.is_empty() {
                    String::new()
                } else {
                    format!("  ({})", identity.display_name)
                }
            ),
            cert.as_ref().map_or_else(
                || "Device    not part of this identity".to_owned(),
                |cert| {
                    format!(
                        "Device    {}  {}{}",
                        cert.device_label,
                        hex::encode(cert.device_public_key),
                        if cert.capabilities.can_admit_devices {
                            "  (approves new devices)"
                        } else {
                            ""
                        }
                    )
                },
            ),
            "Devices".to_owned(),
        ];
        text.extend(devices.iter().map(describe));
        text.extend(next.iter().map(|step| format!("next: {step}")));
        context.emit(
            &json!({
                "identity": {
                    "id": identity.id,
                    "display_name": identity.display_name,
                    "identity_dir": context.person.dir,
                },
                "this_device": this_device,
                "devices": devices,
                "next": next,
            }),
            &text.join("\n"),
        );
        Ok(())
    }

    fn create(
        context: &Context,
        acknowledged: bool,
        backup: Option<&Path>,
        passphrase_file: Option<&Path>,
        no_backup: bool,
    ) -> anyhow::Result<()> {
        if !acknowledged {
            return Err(guided(
                "nobody can recover a Tilo identity: losing every device that holds it and its \
                 backup loses it for good",
                "pass --acknowledge-no-recovery-service once you accept that",
            ));
        }
        if backup.is_none() && !no_backup {
            return Err(guided(
                "choose what happens to the backup",
                "pass --backup PATH to write the encrypted backup (keep a copy off this \
                 device), or --no-backup-i-understand",
            ));
        }
        if let Some(existing) = context.identity()? {
            return Err(guided(
                format!("this account already has the identity {}", existing.id),
                "see it with `tilo-identity status`",
            ));
        }
        if let Some(destination) = backup
            && destination.exists()
        {
            return Err(guided(
                format!("{} already exists", destination.display()),
                "choose a new path; a backup never replaces a file",
            ));
        }
        if tilo_user_identity::persist::read_device_key(&context.device.device_privkey).is_err() {
            return Err(guided(
                format!(
                    "this device's key at {} is missing or this account cannot read it",
                    context.device.device_privkey.display()
                ),
                "make sure this account is in the tilo-identity group (`id -nG`; log in again \
                 after adding it)",
            ));
        }
        // Ask for the passphrase before minting, so a typo cannot leave an
        // identity without the backup that was asked for.
        let passphrase = backup
            .map(|_| backup_passphrase(passphrase_file))
            .transpose()?;
        let outcome = provisioning::ensure_collaboration_identity(
            &context.person.dir,
            &context.device,
            IdentityStorage::for_session(),
        )
        .context("create the identity")?;
        let ProvisionOutcome::Created { pubkey_hex } = outcome else {
            return Err(guided(
                "this account already has an identity",
                "see it with `tilo-identity status`",
            ));
        };
        if let (Some(destination), Some(passphrase)) = (backup, passphrase.as_deref()) {
            let identity = context.require_identity()?;
            write_backup(&identity, destination, passphrase).map_err(|error| {
                guided(
                    format!("the identity was created, but its backup failed: {error:#}"),
                    "write it now with `tilo-identity backup --to PATH`",
                )
            })?;
        }
        let next = "log in again so the session runs as this identity; then add your other \
                    devices with `tilo-identity pair` there and `tilo-identity approve` here";
        context.emit(
            &json!({ "identity": pubkey_hex, "backup": backup, "next": next }),
            &format!(
                "Created the identity {pubkey_hex}.\n{}next: {next}",
                backup.map_or_else(
                    || "No backup was written.\n".to_owned(),
                    |path| format!(
                        "Wrote the encrypted backup to {}. Keep a copy off this device.\n",
                        path.display()
                    )
                )
            ),
        );
        Ok(())
    }

    fn write_backup(
        identity: &IdentityProfile,
        destination: &Path,
        passphrase: &str,
    ) -> anyhow::Result<()> {
        runtime_identity::export_portable_backup(
            &identity.record_path,
            destination,
            passphrase.as_bytes(),
        )
        .with_context(|| format!("write the backup to {}", destination.display()))?;
        Ok(())
    }

    fn backup_passphrase(file: Option<&Path>) -> anyhow::Result<String> {
        let passphrase = if let Some(path) = file {
            read_passphrase_file(path)?
        } else {
            let first = prompt_hidden("Backup passphrase (12 characters or more): ")?;
            if prompt_hidden("The passphrase again: ")? != first {
                return Err(guided("the passphrases differ", "run the command again"));
            }
            first
        };
        if passphrase.chars().count() < 12 {
            return Err(guided(
                "the backup passphrase needs 12 characters or more",
                "choose a longer one; it is the only way to open the backup",
            ));
        }
        Ok(passphrase)
    }

    fn read_passphrase_file(path: &Path) -> anyhow::Result<String> {
        let mut text = String::new();
        if path == Path::new("-") {
            std::io::stdin().take(4096).read_to_string(&mut text)?;
        } else {
            std::fs::File::open(path)
                .with_context(|| format!("open {}", path.display()))?
                .take(4096)
                .read_to_string(&mut text)?;
        }
        Ok(text.trim_end_matches(['\r', '\n']).to_owned())
    }

    /// Read one line from the terminal without showing it.
    fn prompt_hidden(prompt: &str) -> anyhow::Result<String> {
        use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};

        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|_| {
                guided(
                    "there is no terminal to ask for the backup passphrase",
                    "pass --passphrase-file FILE, or `-` to read it from stdin",
                )
            })?;
        let shown = tcgetattr(&tty).context("read the terminal's settings")?;
        let mut hidden = shown.clone();
        hidden.local_modes.remove(LocalModes::ECHO);
        tcsetattr(&tty, OptionalActions::Now, &hidden).context("hide the passphrase")?;
        let mut line = String::new();
        let read = write!(&tty, "{prompt}")
            .and_then(|()| (&tty).flush())
            .and_then(|()| std::io::BufReader::new(&tty).read_line(&mut line));
        tcsetattr(&tty, OptionalActions::Now, &shown).context("restore the terminal")?;
        writeln!(&tty)?;
        read?;
        Ok(line.trim_end_matches(['\r', '\n']).to_owned())
    }

    fn pair(context: &Context, label: &str, timeout: u64) -> anyhow::Result<()> {
        if let Some(identity) = context.identity()?
            && context.device_cert(&identity).is_some()
        {
            return Err(guided(
                format!(
                    "this device already belongs to the identity {}",
                    identity.id
                ),
                "see it with `tilo-identity status`",
            ));
        }
        provisioning::prepare_identity_directory(&context.person.dir)
            .with_context(|| format!("prepare {}", context.person.dir.display()))?;
        // The status file is what first-use onboarding completes from, too.
        let status_file = context.person.dir.join(".nearby-enrollment-status.json");
        let mut command = Command::new(device_admin::ENROLL);
        command
            .args(["pair-device", "--headless", "--identity-root"])
            .arg(&context.person.root)
            .arg("--identity-dir")
            .arg(&context.person.dir)
            .args(["--label", label, "--timeout"])
            .arg(timeout.min(PAIRING_WINDOW_SECS).to_string())
            .arg("--status-file")
            .arg(&status_file);
        if !context.json {
            eprintln!(
                "On a device that already has the identity, on this network, run:\n  \
                 tilo-identity approve --code <the code below>"
            );
        }
        let ran = forward(&mut command, device_admin::ENROLL);
        let status: Value = std::fs::read(&status_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null);
        let _ = std::fs::remove_file(&status_file);
        ran.map_err(|detail| {
            guided(
                format!("pairing failed: {detail}"),
                "run `tilo-identity approve` on a device that has the identity while this \
                 waits; both must be on one network with their clocks about right",
            )
        })?;
        let identity = status.get("identity").and_then(Value::as_str).unwrap_or("");
        let next = "log in again so the session runs as the joined identity; then check with \
                    `tilo-identity status`";
        context.emit(
            &json!({
                "identity": identity,
                "code": status.get("verification_code"),
                "next": next,
            }),
            &format!("Joined the identity {identity}.\nnext: {next}"),
        );
        Ok(())
    }

    fn approve(
        context: &Context,
        code: Option<&str>,
        device: Option<&str>,
        timeout: u64,
        elevate: Elevate,
    ) -> anyhow::Result<()> {
        let identity = context.require_identity()?;
        let holding = device_admin::key_holding(&context.person)
            .map_err(|error| guided(error, "check this account with `tilo-identity status`"))?;
        if holding != device_admin::KeyHolding::Whole {
            return approve_through_session(context, code, device);
        }
        if !context
            .device_cert(&identity)
            .is_some_and(|cert| cert.capabilities.can_admit_devices)
        {
            return Err(guided(
                "this device cannot approve new devices",
                "approve from the device that created the identity",
            ));
        }
        if !context.json {
            eprintln!("Looking for a device waiting in `tilo-identity pair`...");
        }
        let found = Command::new(device_admin::ENROLL)
            .arg(PairRole::PersonalDevice.discover_command())
            .arg("--timeout")
            .arg(timeout.clamp(1, PAIRING_WINDOW_SECS).to_string())
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("run {}", device_admin::ENROLL))?;
        let candidate =
            device_admin::discovered(found.status.success(), &found.stdout, &found.stderr)
                .map_err(|error| {
                    guided(
                        error,
                        "start `tilo-identity pair` on the new device first, on this network, and \
                     run this while it waits",
                    )
                })?;
        refuse_another(&candidate, code, device)?;
        if !context.json {
            eprintln!(
                "Approving '{}' ({}), code {}",
                candidate.label, candidate.device_node_id, candidate.verification_code
            );
        }
        let approval = Approval {
            role: PairRole::PersonalDevice,
            device: candidate.device_node_id.clone(),
            service: candidate.relay_service_node_id.clone(),
            workspace_ids: None,
        };
        let pkexec = match elevate {
            Elevate::Auto => desktop_session(),
            Elevate::Pkexec => true,
            Elevate::Sudo => false,
        };
        let launcher = if pkexec { "pkexec" } else { "sudo" };
        let mut command = Command::new(launcher);
        command.args(device_admin::approval_args(&approval, &context.person, &[]));
        forward(&mut command, launcher).map_err(|detail| {
            guided(
                format!("'{}' was not approved: {detail}", candidate.label),
                "check both devices with `tilo-identity status`, then try again; trying again \
                 is safe",
            )
        })?;
        let next = format!(
            "on '{}', log in again so its session runs as the identity",
            candidate.label
        );
        context.emit(
            &json!({
                "device": candidate.device_node_id,
                "label": candidate.label,
                "code": candidate.verification_code,
                "next": next,
            }),
            &format!(
                "Approved '{}' ({}).\nnext: {next}",
                candidate.label, candidate.device_node_id
            ),
        );
        Ok(())
    }

    /// This account's Tilo session, reached through its control socket.
    struct Session(tilo_control::client::Desktop);

    /// A device another of the person's devices found, asking here.
    struct Ask {
        id: String,
        device: String,
        label: String,
        code: String,
        via: String,
    }

    impl Session {
        fn connect() -> anyhow::Result<Self> {
            use tilo_control::client::{Desktop, doors_from_env};
            Desktop::connect(&doors_from_env()).map(Self).map_err(|_| {
                guided(
                    "the Tilo session is not running here",
                    "start the Tilo session here (`tilo-board-headless-session@$USER` on a \
                     board), then run this again",
                )
            })
        }

        /// Run the session's action `id` with `args`; its output.
        fn run(&mut self, id: &str, args: &Value) -> Result<Value, String> {
            use tilo_control::client::{Unreachable, invoke_wire};
            let answer = self
                .0
                .request(
                    &invoke_wire("actions", id, args, None, None)
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| match error {
                    Unreachable::NoDesktop => "the session is not running".to_string(),
                    Unreachable::Refused { message, .. } => message,
                    Unreachable::Io { error, .. } => format!("the session broke off: {error}"),
                })?;
            if let Some(message) = answer.pointer("/err/message").and_then(Value::as_str) {
                return Err(message.to_string());
            }
            let update = answer
                .pointer("/answer/receipt/update")
                .cloned()
                .unwrap_or_default();
            match update.get("state").and_then(Value::as_str) {
                Some("failed" | "cancelled") => Err(update
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("the session did not say why")
                    .to_string()),
                _ => Ok(update.pointer("/output/value").cloned().unwrap_or_default()),
            }
        }

        /// The devices asking here, oldest first.
        fn asking(&mut self) -> anyhow::Result<Vec<Ask>> {
            let listed = self
                .run("identity.device.pair.asking", &json!({}))
                .map_err(anyhow::Error::msg)
                .context("ask the session who asks")?;
            Ok(listed
                .get("asking")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|asking| {
                    let text = |key: &str| {
                        asking
                            .get(key)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    };
                    Ask {
                        id: text("ask"),
                        device: text("device"),
                        label: text("label"),
                        code: text("code"),
                        via: text("via"),
                    }
                })
                .collect())
        }

        /// Approve or decline `ask`, as its notification's buttons do.
        fn answer(&mut self, context: &Context, ask: &Ask, approve: bool) -> anyhow::Result<()> {
            if approve && !context.json {
                eprintln!(
                    "Approving '{}', found by '{}', code {}",
                    ask.label, ask.via, ask.code
                );
            }
            self.run(
                "identity.device.pair.answer",
                &json!({"ask": ask.id, "approve": approve, "confirm": approve}),
            )
            .map_err(|detail| {
                guided(
                    format!("'{}' was not answered: {detail}", ask.label),
                    "see who still asks with `tilo-identity asking`",
                )
            })?;
            let (verb, next) = if approve {
                (
                    "Approved",
                    format!(
                        "'{}' joins once '{}' finishes pairing it",
                        ask.label, ask.via
                    ),
                )
            } else {
                ("Declined", format!("'{}' does not join", ask.label))
            };
            context.emit(
                &json!({
                    "label": ask.label,
                    "code": ask.code,
                    "via": ask.via,
                    "approved": approve,
                    "next": next,
                }),
                &format!("{verb} '{}'.\nnext: {next}", ask.label),
            );
            Ok(())
        }
    }

    /// The one of `asking` that shows `code`, or whose key starts with
    /// `device`; without either, the only one. `None` when none does.
    fn pick(
        asking: Vec<Ask>,
        code: Option<&str>,
        device: Option<&str>,
    ) -> anyhow::Result<Option<Ask>> {
        let wanted = device.map(|device| device.trim().to_ascii_lowercase());
        let mut matching: Vec<Ask> = asking
            .into_iter()
            .filter(|ask| code.is_none_or(|code| digits(code) == digits(&ask.code)))
            .filter(|ask| {
                wanted
                    .as_deref()
                    .is_none_or(|wanted| ask.device.starts_with(wanted))
            })
            .collect();
        if matching.len() > 1 {
            return Err(guided(
                "more than one device asks you",
                "see them with `tilo-identity asking`, then pass --code CODE",
            ));
        }
        Ok(matching.pop())
    }

    /// Approve the device asking here that `code` or `device` names, if
    /// one does; whether one did. Without a session nobody can ask here,
    /// and a device nearby still can be approved.
    fn answer_asking(
        context: &Context,
        code: Option<&str>,
        device: Option<&str>,
    ) -> anyhow::Result<bool> {
        if let Ok(mut session) = Session::connect()
            && let Ok(asking) = session.asking()
            && let Some(ask) = pick(asking, code, device)?
        {
            session.answer(context, &ask, true)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn asking(context: &Context) -> anyhow::Result<()> {
        let asking = Session::connect()?.asking()?;
        let listed: Vec<Value> = asking
            .iter()
            .map(|ask| {
                json!({"device": ask.device, "label": ask.label, "code": ask.code, "via": ask.via})
            })
            .collect();
        let text = if asking.is_empty() {
            "No device asks you here.".to_owned()
        } else {
            asking
                .iter()
                .map(|ask| {
                    format!(
                        "  {:<24} {}  code {}  found by '{}'",
                        ask.label, ask.device, ask.code, ask.via
                    )
                })
                .chain(std::iter::once(
                    "next: when the new device shows the same code, `tilo-identity approve \
                     --code CODE`; else `tilo-identity decline --code CODE`"
                        .to_owned(),
                ))
                .collect::<Vec<_>>()
                .join("\n")
        };
        context.emit(&json!({ "asking": listed }), &text);
        Ok(())
    }

    /// `approve` on a device that holds none of the identity's key, or a
    /// share of it: the session finds the device and has the devices that
    /// hold the key certify it, each asking the person
    /// (`identity.device.pair.*`, as Settings pairs).
    fn approve_through_session(
        context: &Context,
        code: Option<&str>,
        device: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut session = Session::connect().map_err(|_| {
            guided(
                "this device adds devices through its Tilo session, which asks your other \
                 devices to approve them, and the session is not running",
                "start the Tilo session here (`tilo-board-headless-session@$USER` on a board), \
                 then run this again",
            )
        })?;
        let mut run = |id: &str, args: Value| session.run(id, &args);
        if !context.json {
            eprintln!("Looking for a device waiting in `tilo-identity pair`...");
        }
        let found = run(
            "identity.device.pair.discover",
            json!({"role": PairRole::PersonalDevice.as_str()}),
        )
        .map_err(|error| {
            guided(
                error,
                "start `tilo-identity pair` on the new device first, on this network, and run \
                 this while it waits",
            )
        })?;
        let text = |key: &str| found.get(key).and_then(Value::as_str).unwrap_or_default();
        let (key, label, shown) = (text("device"), text("label"), text("verification_code"));
        let mismatch =
            if device.is_some_and(|wanted| !key.starts_with(&wanted.trim().to_ascii_lowercase())) {
                Some(format!(
                    "'{label}' ({key}) is waiting, not the device you named"
                ))
            } else if code.is_some_and(|code| digits(code) != digits(shown)) {
                Some(format!(
                    "'{label}' shows the code {shown}, not {}",
                    code.unwrap_or_default().trim()
                ))
            } else {
                None
            };
        if let Some(mismatch) = mismatch {
            let _ = run("identity.device.pair.reject", json!({"device": key}));
            return Err(guided(
                mismatch,
                "nothing was approved; make sure you approve the device you meant to",
            ));
        }
        if !context.json {
            eprintln!(
                "'{label}' shows {shown}. Your other devices ask you to approve it: approve \
                 it there when it shows the same code."
            );
        }
        run(
            "identity.device.pair.approve",
            json!({
                "role": PairRole::PersonalDevice.as_str(),
                "device": key,
                "service": text("service"),
                "confirm": true,
            }),
        )
        .map_err(|detail| {
            guided(
                format!("'{label}' was not approved: {detail}"),
                "check both devices with `tilo-identity status`, then try again; trying again \
                 is safe",
            )
        })?;
        let next = format!("on '{label}', log in again so its session runs as the identity");
        context.emit(
            &json!({"device": key, "label": label, "code": shown, "next": next}),
            &format!("Approved '{label}' ({key}).\nnext: {next}"),
        );
        Ok(())
    }

    /// Refuse `candidate` unless it is the device `device` names and shows
    /// `code`.
    fn refuse_another(
        candidate: &device_admin::Candidate,
        code: Option<&str>,
        device: Option<&str>,
    ) -> anyhow::Result<()> {
        if let Some(wanted) = device
            && !candidate
                .device_node_id
                .starts_with(&wanted.trim().to_ascii_lowercase())
        {
            return Err(guided(
                format!(
                    "'{}' ({}) is waiting, not the device you named",
                    candidate.label, candidate.device_node_id
                ),
                "nothing was approved; run this again once the device you meant is waiting",
            ));
        }
        if let Some(code) = code
            && digits(code) != digits(&candidate.verification_code)
        {
            return Err(guided(
                format!(
                    "'{}' shows the code {}, not {}",
                    candidate.label,
                    candidate.verification_code,
                    code.trim()
                ),
                "nothing was approved; make sure you approve the device you meant to",
            ));
        }
        Ok(())
    }

    fn digits(code: &str) -> String {
        code.chars().filter(char::is_ascii_digit).collect()
    }

    /// Whether this runs in a desktop session, where polkit can ask.
    fn desktop_session() -> bool {
        ["WAYLAND_DISPLAY", "DISPLAY"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
    }

    /// Run `command` with the terminal for its prompts, show its output as
    /// progress on stderr, and answer why it failed.
    fn forward(command: &mut Command, what: &str) -> Result<(), String> {
        let output = command
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                let progress = child.stdout.take().map(|stdout| {
                    std::thread::spawn(move || {
                        for line in std::io::BufReader::new(stdout)
                            .lines()
                            .map_while(Result::ok)
                        {
                            eprintln!("{line}");
                        }
                    })
                });
                let output = child.wait_with_output();
                if let Some(progress) = progress {
                    let _ = progress.join();
                }
                output
            })
            .map_err(|error| format!("could not run {what}: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(device_admin::command_error(
                &output.stderr,
                &format!("{what} ended without saying why"),
            ))
        }
    }

    fn revoke(context: &Context, device: &str, reason: &str, yes: bool) -> anyhow::Result<()> {
        let identity = context.require_identity()?;
        let devices = context.devices(&identity)?;
        let wanted = device.trim().to_ascii_lowercase();
        let matches: Vec<&FleetDevice> = devices
            .iter()
            .filter(|known| !wanted.is_empty() && known.key_hex.starts_with(&wanted))
            .collect();
        let target = match matches.as_slice() {
            [target] => *target,
            [] => {
                return Err(guided(
                    format!("no device of this identity starts with {device}"),
                    "list them with `tilo-identity devices`",
                ));
            }
            _ => {
                return Err(guided(
                    format!("{device} matches several devices"),
                    "give more of the device's key",
                ));
            }
        };
        if target.current {
            return Err(guided(
                "that is this device",
                "revoke it from another of your devices",
            ));
        }
        if !yes {
            return Err(guided(
                format!("revoking '{}' cannot be undone", target.label),
                "pass --yes to revoke it",
            ));
        }
        let revoked =
            device_admin::revoke(&context.fleets(), &identity.id, &target.key_hex, reason)
                .map_err(anyhow::Error::msg)
                .context("revoke the device")?;
        context.emit(
            &json!({ "device": revoked, "label": target.label, "revoked": true }),
            &format!("Revoked '{}'.", target.label),
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_code_matches_however_it_is_grouped() {
            assert_eq!(digits(" 123-456 "), digits("123456"));
            assert_ne!(digits("123-457"), digits("123456"));
        }

        #[test]
        fn every_error_names_its_next_step() {
            let error = guided("no identity", CREATE_OR_PAIR);
            assert_eq!(
                error
                    .downcast_ref::<Guided>()
                    .map(|guided| guided.next.as_str()),
                Some(CREATE_OR_PAIR)
            );
        }
    }
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("tilo-identity runs on Tilo's Linux system");
    std::process::ExitCode::FAILURE
}
