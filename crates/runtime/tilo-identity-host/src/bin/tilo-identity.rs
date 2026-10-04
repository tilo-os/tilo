//! `tilo-identity` — manage this account's Tilo identity from a shell.
//!
//! The command-line door to the same identity, device-enrollment and fleet
//! code that onboarding and Settings use, for headless machines, SSH sessions
//! and agents. It never reimplements a ceremony: `create` mints through
//! [`provisioning::ensure_collaboration_identity`] and writes the portable
//! backup onboarding writes; `pair` and `approve` drive `tilo-enroll` and its
//! privileged helper, exactly as first-use and Settings do; `revoke` signs
//! through [`DeviceFleetStore::revoke`].
//!
//! Every command takes `--json` and then prints one JSON object on stdout,
//! including errors (`{"error": ..., "next": ...}`).

#[cfg(unix)]
mod unix {
    use std::io::{BufRead as _, Read as _, Write as _};
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode, Stdio};

    use anyhow::{Context as _, anyhow, bail};
    use clap::{Parser, Subcommand, ValueEnum};
    use serde_json::{Value, json};
    use tilo_identity_host::device_fleet::DeviceFleetStore;
    use tilo_identity_host::provisioning::{self, IdentityStorage, ProvisionOutcome};
    use tilo_relay_protocol::personal_devices;
    use tilo_user_identity::{DeviceCert, IdentityPaths, RevocationReason};

    const ENROLL: &str = "/usr/bin/tilo-enroll";
    const HELPER: &str = "/usr/libexec/tilo-home-peer-control";
    const BOARD_SESSION: &str = "tilo-board-headless-session";
    /// The compositor unit the board session starts in the account's manager.
    const BOARD_SESSION_BACKEND: &str = "tilo-board-session";
    /// Onboarding's minimum backup passphrase length.
    const MIN_PASSPHRASE: usize = 12;
    /// A presence snapshot older than this means the session stopped.
    const PRESENCE_STALE_MS: u64 = 90_000;

    #[derive(Parser)]
    #[command(
        name = "tilo-identity",
        about = "Manage this account's Tilo identity and the devices that share it",
        long_about = "Manage this account's Tilo identity and the devices that share it.\n\n\
            New machine, no identity anywhere yet:   tilo-identity create ...\n\
            New machine, identity on another device: tilo-identity pair   (here)\n\
            \x20                                        tilo-identity approve (there)\n\
            Check everything:                        tilo-identity status\n\n\
            Run as the account that owns the identity, not as root; commands that need\n\
            root ask for it through sudo (or polkit in a desktop session)."
    )]
    struct Cli {
        /// Print one JSON object on stdout instead of text.
        #[arg(long, global = true)]
        json: bool,
        /// This account's identity directory.
        #[arg(long, global = true, value_name = "DIR")]
        identity_dir: Option<PathBuf>,
        #[command(subcommand)]
        command: Cmd,
    }

    #[derive(Subcommand)]
    enum Cmd {
        /// Who this account is, this device's enrollment, and the person's
        /// other devices with whether each is online now.
        Status,
        /// Create a new identity here, exactly as first-use onboarding does.
        ///
        /// There is no escrow or reset service: losing every device and the
        /// backup loses the identity. Pass --acknowledge-no-recovery-service
        /// to confirm, and either --backup PATH or --no-backup-i-understand.
        Create {
            /// Confirms there is no escrow or reset service for the identity.
            #[arg(long)]
            acknowledge_no_recovery_service: bool,
            /// Write the passphrase-encrypted recovery backup here, the file
            /// onboarding's backup step writes. Must not exist yet.
            #[arg(long, value_name = "PATH", conflicts_with = "no_backup_i_understand")]
            backup: Option<PathBuf>,
            /// File holding the backup passphrase (at least 12 characters);
            /// `-` reads it from stdin. Without it, a terminal is prompted.
            #[arg(long, value_name = "FILE", requires = "backup")]
            passphrase_file: Option<PathBuf>,
            /// Create the identity without a backup.
            #[arg(long)]
            no_backup_i_understand: bool,
            /// Restart this account's headless board session afterwards, so
            /// it runs as the new identity.
            #[arg(long)]
            restart_session: bool,
        },
        /// Write the passphrase-encrypted recovery backup of this identity.
        Backup {
            /// Destination; must not exist yet.
            #[arg(long, value_name = "PATH")]
            to: PathBuf,
            /// File holding the passphrase; `-` reads stdin.
            #[arg(long, value_name = "FILE")]
            passphrase_file: Option<PathBuf>,
        },
        /// Join an identity that lives on another device on this network.
        ///
        /// Prints a six-digit code and waits; on the other device run
        /// `tilo-identity approve`. Needs sudo for the device key.
        Pair {
            /// How this device is listed on the person's other devices.
            #[arg(long, default_value = "Tilo device")]
            label: String,
            /// Seconds to wait for approval.
            #[arg(long, default_value_t = 270)]
            timeout: u64,
            /// Restart this account's headless board session afterwards, so
            /// it runs as the joined identity.
            #[arg(long)]
            restart_session: bool,
        },
        /// Approve the device waiting in `tilo-identity pair` on this network.
        ///
        /// Finds it, checks the code if given, certifies it, sends it this
        /// identity's devices, home servers and workspaces, and records it so
        /// both devices reconnect to each other later.
        Approve {
            /// Only approve this device (its id or a unique prefix).
            #[arg(long, value_name = "ID")]
            device: Option<String>,
            /// Only approve if the waiting device shows this code.
            #[arg(long, value_name = "CODE")]
            code: Option<String>,
            /// Seconds to look for the waiting device and complete approval.
            #[arg(long, default_value_t = 60)]
            timeout: u64,
            /// How to reach the privileged helper.
            #[arg(long, value_enum, default_value_t = Elevate::Auto)]
            elevate: Elevate,
        },
        /// Restart this account's headless board session so it runs as the
        /// current identity, and wait for the new compositor.
        RestartSession,
        /// List the devices certified for this identity.
        Devices,
        /// Revoke a device: it can no longer act as this identity.
        Revoke {
            /// The device id (or a unique prefix), from `tilo-identity devices`.
            device: String,
            #[arg(long, value_enum)]
            reason: Reason,
            /// Confirms the revocation; it cannot be undone.
            #[arg(long)]
            yes: bool,
        },
    }

    #[derive(Clone, Copy, ValueEnum)]
    enum Elevate {
        /// polkit in a graphical session, sudo otherwise.
        Auto,
        Sudo,
        Pkexec,
    }

    #[derive(Clone, Copy, ValueEnum)]
    enum Reason {
        Lost,
        Stolen,
        Retired,
        Compromised,
    }

    impl From<Reason> for RevocationReason {
        fn from(reason: Reason) -> Self {
            match reason {
                Reason::Lost => Self::Lost,
                Reason::Stolen => Self::Stolen,
                Reason::Retired => Self::Retired,
                Reason::Compromised => Self::Compromised,
            }
        }
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

    struct Context {
        json: bool,
        identity_dir: PathBuf,
        paths: IdentityPaths,
    }

    impl Context {
        fn identity_root(&self) -> PathBuf {
            identity_root(&self.paths)
        }
    }

    fn identity_root(paths: &IdentityPaths) -> PathBuf {
        paths
            .user_pubkey
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("/var/lib/tilo"), Path::to_path_buf)
    }

    pub fn main() -> ExitCode {
        let cli = Cli::parse();
        let json = cli.json;
        let result = (|| {
            let identity_dir = match cli.identity_dir {
                Some(dir) => dir,
                None => runtime_identity::identity_dir_from_env()
                    .context("neither XDG_CONFIG_HOME nor HOME names a config directory")?,
            };
            let context = Context {
                json,
                identity_dir,
                paths: provisioning::collaboration_identity_paths(),
            };
            run(&context, cli.command)
        })();
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
                restart_session,
            } => create(
                context,
                &CreateOptions {
                    acknowledged: acknowledge_no_recovery_service,
                    backup: backup.as_deref(),
                    passphrase_file: passphrase_file.as_deref(),
                    no_backup: no_backup_i_understand,
                    restart_session,
                },
            ),
            Cmd::Backup {
                to,
                passphrase_file,
            } => {
                refuse_root()?;
                let record = active_record(&context.identity_dir)?;
                let passphrase = backup_passphrase(passphrase_file.as_deref())?;
                let info = write_backup(&record, &to, &passphrase)?;
                emit(
                    context,
                    &json!({ "backup": to, "identity": hex::encode(info.group_public_key) }),
                    &format!("Wrote the encrypted backup to {}.", to.display()),
                );
                Ok(())
            }
            Cmd::Pair {
                label,
                timeout,
                restart_session,
            } => pair(context, &label, timeout, restart_session),
            Cmd::Approve {
                device,
                code,
                timeout,
                elevate,
            } => approve(
                context,
                device.as_deref(),
                code.as_deref(),
                timeout,
                elevate,
            ),
            Cmd::RestartSession => {
                refuse_root()?;
                restart_board_session()?;
                emit(
                    context,
                    &json!({ "session_restarted": true, "next": "check with `tilo-identity status`" }),
                    "Restarted the session.\nnext: check with `tilo-identity status`",
                );
                Ok(())
            }
            Cmd::Devices => devices(context),
            Cmd::Revoke {
                device,
                reason,
                yes,
            } => revoke(context, &device, reason, yes),
        }
    }

    fn emit(context: &Context, value: &Value, text: &str) {
        if context.json {
            println!("{value}");
        } else {
            println!("{text}");
        }
    }

    fn refuse_root() -> anyhow::Result<()> {
        // SAFETY: geteuid has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            return Err(guided(
                "this runs as the account that owns the identity, not as root",
                "run it again as that account, without sudo; it asks for sudo itself when needed",
            ));
        }
        Ok(())
    }

    fn now_ns() -> u64 {
        tilo_clock::try_unix_now_ns().unwrap_or_default()
    }

    fn now_ms() -> u64 {
        now_ns() / 1_000_000
    }

    struct ActiveIdentity {
        id: String,
        public_key: [u8; 32],
        display_name: String,
        protection: runtime_identity::KeyProtection,
    }

    fn active_identity(identity_dir: &Path) -> anyhow::Result<Option<ActiveIdentity>> {
        if !identity_dir.exists() {
            return Ok(None);
        }
        let identities = runtime_identity::list_identities(identity_dir)
            .with_context(|| format!("read identities in {}", identity_dir.display()))?;
        Ok(identities
            .into_iter()
            .find(|identity| identity.active)
            .map(|identity| ActiveIdentity {
                id: hex::encode(identity.public_key),
                public_key: identity.public_key,
                display_name: identity.display_name,
                protection: identity.protection,
            }))
    }

    fn active_record(identity_dir: &Path) -> anyhow::Result<PathBuf> {
        runtime_identity::resolve_active_identity(identity_dir).ok_or_else(|| {
            guided(
                format!("{} holds no active identity", identity_dir.display()),
                "create one with `tilo-identity create`, or join one with `tilo-identity pair`",
            )
        })
    }

    /// This device's certificate for `user`, when it is enrolled to them.
    fn device_cert(paths: &IdentityPaths, user: [u8; 32]) -> Option<DeviceCert> {
        tilo_user_identity::persist::read_device_cert(&paths.device_cert_for_user(&user))
            .or_else(|_| tilo_user_identity::persist::read_device_cert(&paths.device_cert))
            .ok()
            .filter(|cert| {
                cert.user_public_key == user
                    && tilo_user_identity::verify_device_cert_at(cert, now_ns()).is_ok()
            })
    }

    fn presence() -> (Option<personal_devices::PresenceSnapshot>, bool) {
        let snapshot = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .and_then(|dir| {
                personal_devices::read_presence(&personal_devices::presence_path(&dir)).ok()
            })
            .flatten();
        let running = snapshot.as_ref().is_some_and(|snapshot| {
            now_ms().saturating_sub(snapshot.refreshed_unix_ms) < PRESENCE_STALE_MS
        });
        (snapshot, running)
    }

    #[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
    #[serde(rename_all = "kebab-case")]
    enum Role {
        HomeServer,
        Primary,
        Member,
    }

    impl Role {
        fn of(cert: &DeviceCert) -> Self {
            if cert.capabilities.can_act_as_host {
                Self::HomeServer
            } else if cert.capabilities.can_admit_devices {
                Self::Primary
            } else {
                Self::Member
            }
        }

        fn label(self) -> &'static str {
            match self {
                Self::HomeServer => "home server",
                Self::Primary => "primary",
                Self::Member => "member",
            }
        }
    }

    /// One certified device of the identity, with what this machine knows
    /// about reaching it.
    #[derive(serde::Serialize)]
    struct DeviceRow {
        device: String,
        label: String,
        role: Role,
        this_device: bool,
        revoked: bool,
        direct_peer: bool,
        /// `None` when no running session reports presence.
        online: Option<bool>,
        shared_rooms: Option<u32>,
        last_seen_unix_ms: Option<u64>,
    }

    impl DeviceRow {
        /// A device of the person that this machine reaches only through a
        /// home server.
        fn unlinked(&self) -> bool {
            !self.this_device && !self.revoked && self.role != Role::HomeServer && !self.direct_peer
        }

        fn describe(&self) -> String {
            let short = self.device.get(..12).unwrap_or(&self.device);
            let state = if self.revoked {
                "revoked".to_string()
            } else if self.this_device {
                "this device".to_string()
            } else {
                match self.online {
                    Some(true) => format!(
                        "online in {} room(s)",
                        self.shared_rooms.unwrap_or_default()
                    ),
                    Some(false) => self.last_seen_unix_ms.map_or_else(
                        || "offline, never seen".to_string(),
                        |seen| format!("offline, last seen {}", ago(seen)),
                    ),
                    None => "online state unknown (session not running)".to_string(),
                }
            };
            let route = if self.direct_peer || self.this_device {
                ""
            } else if self.role == Role::HomeServer {
                ", via home-peer registry"
            } else {
                ", not a direct peer"
            };
            format!(
                "  {:<24} {short}  {:<11} {state}{route}",
                self.label,
                self.role.label()
            )
        }
    }

    fn device_rows(context: &Context, identity: &ActiveIdentity) -> anyhow::Result<Vec<DeviceRow>> {
        let fleet = DeviceFleetStore::at(&context.identity_dir, context.paths.clone())
            .load(&identity.id)
            .context("read this identity's device fleet")?;
        let local =
            device_cert(&context.paths, identity.public_key).map(|cert| cert.device_public_key);
        let direct = personal_devices::read_direct_peers(
            &context
                .identity_root()
                .join(personal_devices::DIRECT_PEERS_FILE),
        )
        .unwrap_or_default();
        let (snapshot, running) = presence();
        let mut certificates = fleet.certificates.clone();
        certificates.sort_by(|left, right| left.device_label.cmp(&right.device_label));
        Ok(certificates
            .into_iter()
            .map(|cert| {
                let device = hex::encode(cert.device_public_key);
                let seen = snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.devices.iter().find(|row| row.device == device));
                let this_device = local == Some(cert.device_public_key);
                DeviceRow {
                    label: cert.device_label.clone(),
                    role: Role::of(&cert),
                    this_device,
                    revoked: fleet.is_revoked(&cert.device_public_key),
                    direct_peer: direct.contains(&cert.device_public_key),
                    online: if this_device {
                        Some(true)
                    } else if running {
                        Some(seen.is_some_and(|row| row.online))
                    } else {
                        None
                    },
                    shared_rooms: seen.map(|row| row.shared_rooms),
                    last_seen_unix_ms: seen.and_then(|row| row.last_seen_unix_ms),
                    device,
                }
            })
            .collect())
    }

    fn ago(unix_ms: u64) -> String {
        let seconds = now_ms().saturating_sub(unix_ms) / 1000;
        match seconds {
            0..=89 => format!("{seconds}s ago"),
            90..=5399 => format!("{}m ago", seconds / 60),
            5400..=129_599 => format!("{}h ago", seconds / 3600),
            _ => format!("{}d ago", seconds / 86_400),
        }
    }

    fn status_next_steps(
        identity: &ActiveIdentity,
        cert: Option<&DeviceCert>,
        session: Option<&personal_devices::PresenceSnapshot>,
        rows: &[DeviceRow],
    ) -> Vec<String> {
        let running = session.is_some();
        let mut next = Vec::new();
        if let Some(session) = session
            && session.identity.as_deref() != Some(identity.id.as_str())
        {
            next.push(format!(
                "the running session {}; {}",
                session.identity.as_ref().map_or_else(
                    || "has no identity yet".to_string(),
                    |other| format!("runs as another identity ({other})")
                ),
                restart_hint()
            ));
        }
        if cert.is_none() {
            next.push(format!(
                "this device holds no valid certificate for the identity; {}",
                restart_hint()
            ));
        }
        if !running {
            next.push(
                "no running session reported presence; start or restart this account's Tilo \
                 session to see which devices are online"
                    .to_string(),
            );
        }
        if rows.iter().any(DeviceRow::unlinked) {
            next.push(
                "devices that are not direct peers are only reachable through a home server; \
                 approve new devices with `tilo-identity approve` so both sides record each other"
                    .to_string(),
            );
        }
        next
    }

    /// Where the identity's signing key lives, as seen from this device. A
    /// device another device enrolled holds only the public record and signs
    /// with its own certified device key.
    fn key_custody(identity: &ActiveIdentity, cert: Option<&DeviceCert>) -> &'static str {
        if identity.protection == runtime_identity::KeyProtection::LegacyPlaintext
            && cert.is_some_and(|cert| !cert.capabilities.can_admit_devices)
        {
            "kept on the devices that can approve; this device signs with its certified device key"
        } else {
            identity.protection.label()
        }
    }

    fn status_text(
        identity: &ActiveIdentity,
        cert: Option<&DeviceCert>,
        running: bool,
        rows: &[DeviceRow],
        next: &[String],
    ) -> String {
        let mut lines = vec![
            format!(
                "Identity  {}{}",
                identity.id,
                if identity.display_name.is_empty() {
                    String::new()
                } else {
                    format!("  ({})", identity.display_name)
                }
            ),
            format!("Key       {}", key_custody(identity, cert)),
            cert.map_or_else(
                || "Device    not enrolled to this identity".to_string(),
                |cert| {
                    format!(
                        "Device    {}  {}{}",
                        cert.device_label,
                        hex::encode(cert.device_public_key),
                        if cert.capabilities.can_admit_devices {
                            "  (can approve devices)"
                        } else {
                            ""
                        }
                    )
                },
            ),
            format!(
                "Session   {}",
                if running { "running" } else { "not running" }
            ),
            "Devices".to_string(),
        ];
        lines.extend(rows.iter().map(DeviceRow::describe));
        lines.extend(next.iter().map(|step| format!("next: {step}")));
        lines.join("\n")
    }

    fn status(context: &Context) -> anyhow::Result<()> {
        let Some(identity) = active_identity(&context.identity_dir)? else {
            let next = "create an identity with `tilo-identity create`, or join one from another \
                        device with `tilo-identity pair`";
            emit(
                context,
                &json!({ "identity": null, "identity_dir": context.identity_dir, "next": [next] }),
                &format!(
                    "No identity in {}.\nnext: {next}",
                    context.identity_dir.display()
                ),
            );
            return Ok(());
        };
        let cert = device_cert(&context.paths, identity.public_key);
        let rows = device_rows(context, &identity)?;
        let (snapshot, running) = presence();
        let rooms = personal_devices::read_rooms(
            &context.identity_root().join(personal_devices::ROOMS_FILE),
        )
        .map(|rooms| rooms.len())
        .unwrap_or_default();
        let next = status_next_steps(
            &identity,
            cert.as_ref(),
            snapshot.as_ref().filter(|_| running),
            &rows,
        );
        let value = json!({
            "identity": {
                "id": identity.id,
                "display_name": identity.display_name,
                "protection": key_custody(&identity, cert.as_ref()),
                "identity_dir": context.identity_dir,
            },
            "this_device": cert.as_ref().map(|cert| json!({
                "device": hex::encode(cert.device_public_key),
                "label": cert.device_label,
                "can_approve_devices": cert.capabilities.can_admit_devices,
            })),
            "session_running": running,
            "session_identity": snapshot.as_ref().filter(|_| running).and_then(|snapshot| snapshot.identity.clone()),
            "presence_refreshed_unix_ms": snapshot.as_ref().map(|snapshot| snapshot.refreshed_unix_ms),
            "shared_rooms_received": rooms,
            "devices": rows,
            "next": next,
        });
        emit(
            context,
            &value,
            &status_text(&identity, cert.as_ref(), running, &rows, &next),
        );
        Ok(())
    }

    fn devices(context: &Context) -> anyhow::Result<()> {
        let identity = active_identity(&context.identity_dir)?.ok_or_else(|| {
            guided(
                "this account has no identity",
                "create one with `tilo-identity create`, or join one with `tilo-identity pair`",
            )
        })?;
        let rows = device_rows(context, &identity)?;
        let text = rows
            .iter()
            .map(DeviceRow::describe)
            .collect::<Vec<_>>()
            .join("\n");
        emit(
            context,
            &json!({ "identity": identity.id, "devices": rows }),
            &text,
        );
        Ok(())
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
        Ok(text.trim_end_matches(['\r', '\n']).to_string())
    }

    /// Read one line from the terminal without echoing it.
    fn prompt_hidden(prompt: &str) -> anyhow::Result<String> {
        use std::os::fd::AsRawFd as _;
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|_| {
                guided(
                    "no terminal to ask for the backup passphrase",
                    "pass --passphrase-file FILE (or `-` to read it from stdin)",
                )
            })?;
        let fd = tty.as_raw_fd();
        // SAFETY: `termios` is plain data; tcgetattr fills it for a valid fd.
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: fd is an open terminal and `original` is writable.
        if unsafe { libc::tcgetattr(fd, &raw mut original) } != 0 {
            bail!("cannot read the terminal's settings");
        }
        let mut hidden = original;
        hidden.c_lflag &= !libc::ECHO;
        // SAFETY: as above; `hidden` is a copy of valid settings.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const hidden) };
        let mut writer = &tty;
        let _ = write!(writer, "{prompt}");
        let _ = writer.flush();
        let mut line = String::new();
        let read = std::io::BufReader::new(&tty).read_line(&mut line);
        // SAFETY: restores the settings read above on the same fd.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const original) };
        let _ = writeln!(writer);
        read?;
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    }

    fn backup_passphrase(file: Option<&Path>) -> anyhow::Result<String> {
        let passphrase = if let Some(path) = file {
            read_passphrase_file(path)?
        } else {
            let first = prompt_hidden("Backup passphrase (12+ characters): ")?;
            let second = prompt_hidden("Repeat the passphrase: ")?;
            if first != second {
                return Err(guided("the passphrases differ", "run the command again"));
            }
            first
        };
        if passphrase.chars().count() < MIN_PASSPHRASE {
            return Err(guided(
                format!("the backup passphrase needs at least {MIN_PASSPHRASE} characters"),
                "choose a longer passphrase; it is the only way to open the backup",
            ));
        }
        Ok(passphrase)
    }

    fn write_backup(
        record: &Path,
        destination: &Path,
        passphrase: &str,
    ) -> anyhow::Result<runtime_identity::PortableBackupInfo> {
        if destination.exists() {
            return Err(guided(
                format!("{} already exists", destination.display()),
                "choose a new path; a backup never overwrites a file",
            ));
        }
        let info =
            runtime_identity::export_portable_backup(record, destination, passphrase.as_bytes())
                .with_context(|| format!("write the backup to {}", destination.display()))?;
        runtime_identity::verify_portable_backup(destination, passphrase.as_bytes())
            .context("the backup was written but does not open again")?;
        Ok(info)
    }

    #[derive(Clone, Copy)]
    struct CreateOptions<'a> {
        acknowledged: bool,
        backup: Option<&'a Path>,
        passphrase_file: Option<&'a Path>,
        no_backup: bool,
        restart_session: bool,
    }

    /// Everything `create` refuses before it touches anything.
    fn create_preflight(context: &Context, options: &CreateOptions<'_>) -> anyhow::Result<()> {
        let CreateOptions {
            acknowledged,
            backup,
            no_backup,
            restart_session,
            ..
        } = *options;
        refuse_root()?;
        if restart_session && !board_session_installed() {
            return Err(guided(
                "--restart-session restarts a headless board session, and this machine has none",
                "leave it out and log out and back in after creating the identity",
            ));
        }
        if !acknowledged {
            return Err(guided(
                "a Tilo identity has no escrow or reset service: losing every device that holds \
                 it and its backup loses it for good",
                "pass --acknowledge-no-recovery-service once you accept that",
            ));
        }
        if backup.is_none() && !no_backup {
            return Err(guided(
                "choose what happens to the recovery backup",
                "pass --backup PATH to write the encrypted backup onboarding offers (keep it off \
                 this device), or --no-backup-i-understand",
            ));
        }
        if let Some(existing) = active_identity(&context.identity_dir)? {
            return Err(guided(
                format!("this account already has the identity {}", existing.id),
                "see it with `tilo-identity status`; one account holds one active identity",
            ));
        }
        if tilo_user_identity::persist::read_device_key(&context.paths.device_privkey).is_err() {
            return Err(guided(
                format!(
                    "this device's key at {} is missing or unreadable by this account",
                    context.paths.device_privkey.display()
                ),
                "run `sudo systemctl start tilo-device-key-init.service`, and make sure this \
                 account is in the tilo-identity group (`id -nG`; log in again after adding it)",
            ));
        }
        Ok(())
    }

    fn create(context: &Context, options: &CreateOptions<'_>) -> anyhow::Result<()> {
        create_preflight(context, options)?;
        let CreateOptions {
            backup,
            passphrase_file,
            restart_session,
            ..
        } = *options;
        // Collect the passphrase before minting, so a typo cannot leave an
        // identity without the backup that was asked for.
        let passphrase = backup
            .map(|destination| {
                if destination.exists() {
                    return Err(guided(
                        format!("{} already exists", destination.display()),
                        "choose a new path; a backup never overwrites a file",
                    ));
                }
                backup_passphrase(passphrase_file)
            })
            .transpose()?;
        let outcome = provisioning::ensure_collaboration_identity(
            &context.identity_dir,
            &context.paths,
            IdentityStorage::Platform,
        )
        .context("create the identity")?;
        let ProvisionOutcome::Created { pubkey_hex } = outcome else {
            return Err(guided(
                "this device was already enrolled to an identity",
                "see it with `tilo-identity status`",
            ));
        };
        let backed_up = match (backup, passphrase.as_deref()) {
            (Some(destination), Some(passphrase)) => {
                let record = active_record(&context.identity_dir)?;
                write_backup(&record, destination, passphrase).map_err(|error| {
                    guided(
                        format!("the identity {pubkey_hex} was created, but its backup failed: {error:#}"),
                        "write it now with `tilo-identity backup --to PATH`",
                    )
                })?;
                Some(destination.to_path_buf())
            }
            _ => None,
        };
        if restart_session {
            restart_board_session()
                .context("the identity was created, but the session restart failed")?;
        }
        let next = if restart_session {
            "check with `tilo-identity status`; then pair your other devices".to_string()
        } else {
            restart_hint()
        };
        emit(
            context,
            &json!({
                "identity": pubkey_hex,
                "backup": backed_up,
                "session_restarted": restart_session,
                "next": next,
            }),
            &format!(
                "Created the identity {pubkey_hex}.\n{}next: {next}",
                backed_up.as_ref().map_or_else(
                    || "No backup was written.\n".to_string(),
                    |path| format!(
                        "Wrote the encrypted backup to {}; keep a copy off this device.\n",
                        path.display()
                    )
                )
            ),
        );
        Ok(())
    }

    fn account_name() -> String {
        std::env::var("USER")
            .ok()
            .filter(|user| !user.is_empty())
            .unwrap_or_else(|| "$USER".into())
    }

    fn board_session_unit() -> String {
        format!("{BOARD_SESSION}@{}.service", account_name())
    }

    fn board_session_installed() -> bool {
        Path::new(&format!("/usr/lib/systemd/system/{BOARD_SESSION}@.service")).exists()
    }

    fn restart_hint() -> String {
        if board_session_installed() {
            "restart the session so it runs as this identity: tilo-identity restart-session".into()
        } else {
            "log out and back in so the session runs as this identity".into()
        }
    }

    /// The main process of this account's running board compositor, or 0.
    fn board_compositor_pid() -> u32 {
        Command::new("systemctl")
            .args(["--user", "show", "--property=MainPID", "--value"])
            .arg(format!("{BOARD_SESSION_BACKEND}.service"))
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .and_then(|pid| pid.trim().parse().ok())
            .unwrap_or_default()
    }

    /// Restart this account's headless board session and wait until a new
    /// compositor runs. The compositor is a unit of the account's own
    /// manager; it is stopped first so a restart of the system unit cannot
    /// leave the previous compositor (and its identity) running beside it.
    fn restart_board_session() -> anyhow::Result<()> {
        if !board_session_installed() {
            return Err(guided(
                "this machine has no headless board session to restart",
                "log out and back in so the session runs as this identity",
            ));
        }
        let before = board_compositor_pid();
        let _ = Command::new("systemctl")
            .args(["--user", "stop"])
            .arg(format!("{BOARD_SESSION_BACKEND}.service"))
            .status();
        let status = Command::new("sudo")
            .args(["systemctl", "restart", &board_session_unit()])
            .status()
            .context("run systemctl")?;
        if !status.success() {
            return Err(guided(
                format!("restarting {} failed", board_session_unit()),
                format!(
                    "check `systemctl status {}` and `journalctl -u {}`",
                    board_session_unit(),
                    board_session_unit()
                ),
            ));
        }
        let restarted_ms = (now_ms() / 1000).saturating_mul(1000);
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(30))
            .context("clock overflow")?;
        while std::time::Instant::now() < deadline {
            let now = board_compositor_pid();
            // The new compositor has started and published its presence,
            // so `status` right after this reports what it runs as.
            if now != 0
                && now != before
                && presence()
                    .0
                    .is_some_and(|snapshot| snapshot.refreshed_unix_ms >= restarted_ms)
            {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Err(guided(
            "the board session did not start a new compositor within 30s",
            format!("check `journalctl -u {}`", board_session_unit()),
        ))
    }

    fn pair(
        context: &Context,
        label: &str,
        timeout: u64,
        restart_session: bool,
    ) -> anyhow::Result<()> {
        refuse_root()?;
        if let Some(identity) = active_identity(&context.identity_dir)?
            && device_cert(&context.paths, identity.public_key).is_some()
        {
            return Err(guided(
                format!(
                    "this device already belongs to the identity {}",
                    identity.id
                ),
                "see it with `tilo-identity status`",
            ));
        }
        if restart_session && !board_session_installed() {
            return Err(guided(
                "--restart-session restarts a headless board session, and this machine has none",
                "leave it out and log out and back in after pairing",
            ));
        }
        provisioning::prepare_identity_directory(&context.identity_dir)
            .with_context(|| format!("prepare {}", context.identity_dir.display()))?;
        let mut command = Command::new("sudo");
        command
            .arg(ENROLL)
            .args(["pair-device", "--headless", "--identity-root"])
            .arg(context.identity_root())
            .arg("--identity-dir")
            .arg(&context.identity_dir)
            .args(["--label", label, "--timeout", &timeout.to_string()]);
        if !context.json {
            eprintln!(
                "Waiting up to {timeout}s. On a device that already has this identity, on the \
                 same network, run:\n  tilo-identity approve --code <the code below>"
            );
        }
        let result = run_forwarding(&mut command, "pairing")?;
        let joined = result
            .lines
            .iter()
            .rev()
            .find_map(|line| serde_json::from_str::<Value>(line).ok())
            .unwrap_or(Value::Null);
        if restart_session {
            restart_board_session().context("the device joined, but the session restart failed")?;
        }
        let next = if restart_session {
            "check the link with `tilo-identity status`".to_string()
        } else {
            format!("{}; then check with `tilo-identity status`", restart_hint())
        };
        emit(
            context,
            &json!({
                "joined": joined.get("identity"),
                "label": joined.get("label"),
                "code": result.code,
                "session_restarted": restart_session,
                "next": next,
            }),
            &format!(
                "Joined the identity {}.\nnext: {next}",
                joined
                    .get("identity")
                    .and_then(Value::as_str)
                    .unwrap_or("(unknown)")
            ),
        );
        Ok(())
    }

    struct Forwarded {
        lines: Vec<String>,
        code: Option<String>,
    }

    /// Run `command`, passing its progress through to stderr as it happens,
    /// and keep its stdout lines for the result.
    fn run_forwarding(command: &mut Command, what: &str) -> anyhow::Result<Forwarded> {
        let mut child = command
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("start {what}"))?;
        let stderr = child.stderr.take().context("child stderr")?;
        let errors = std::thread::spawn(move || {
            let mut kept = String::new();
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                eprintln!("{line}");
                kept.push_str(&line);
                kept.push('\n');
            }
            kept
        });
        let mut lines = Vec::new();
        let mut code = None;
        if let Some(stdout) = child.stdout.take() {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if line.contains("code")
                    && let Some(found) = line
                        .split_whitespace()
                        .last()
                        .filter(|word| word.len() == 6 && word.bytes().all(|b| b.is_ascii_digit()))
                {
                    code = Some(found.to_string());
                }
                if !line.trim_start().starts_with('{') {
                    eprintln!("{line}");
                }
                lines.push(line);
            }
        }
        let status = child.wait()?;
        let stderr = errors.join().unwrap_or_default();
        if !status.success() {
            return Err(failure_guidance(what, stderr.trim()));
        }
        Ok(Forwarded { lines, code })
    }

    fn failure_guidance(what: &str, detail: &str) -> anyhow::Error {
        let next = if detail.contains("no approving Tilo device answered")
            || detail.contains("no trusted Tilo device approved")
        {
            "run `tilo-identity approve` on a device that has the identity while this waits; both \
             must be on one network segment (UDP broadcast, port 23831) with clocks roughly right"
        } else if detail.contains("no matching unprovisioned Tilo device") {
            "start `tilo-identity pair` on the new device first and run this while it waits"
        } else if detail.contains("does not hold the identity root") {
            "approve from the account and session that created the identity"
        } else if detail.contains("not authorised to approve") {
            "approve from a device that can approve devices (`tilo-identity status` shows it)"
        } else if detail.contains("sudo") || detail.contains("password") {
            "this step needs sudo; run it in a terminal where sudo can ask for the password"
        } else {
            "check both devices with `tilo-identity status`, then retry; retrying is safe"
        };
        guided(
            if detail.is_empty() {
                format!("{what} failed")
            } else {
                format!("{what} failed: {detail}")
            },
            next,
        )
    }

    #[derive(serde::Deserialize)]
    struct Candidate {
        device_node_id: String,
        relay_service_node_id: String,
        label: String,
        verification_code: String,
    }

    fn discover(device: Option<&str>, timeout: u64) -> anyhow::Result<Candidate> {
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(timeout.max(1)))
            .context("the timeout is too large")?;
        let mut other = None;
        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let output = Command::new(ENROLL)
                .args(["discover-device", "--timeout"])
                .arg(remaining.as_secs().clamp(1, 15).to_string())
                .output()
                .with_context(|| format!("run {ENROLL}"))?;
            if !output.status.success() {
                continue;
            }
            let Some(candidate) = String::from_utf8_lossy(&output.stdout)
                .lines()
                .find_map(|line| serde_json::from_str::<Candidate>(line).ok())
            else {
                continue;
            };
            if device.is_none_or(|wanted| candidate.device_node_id.starts_with(wanted)) {
                return Ok(candidate);
            }
            other = Some(format!(
                "'{}' ({}) is waiting instead",
                candidate.label, candidate.device_node_id
            ));
        }
        Err(guided(
            other.map_or_else(
                || "no device is waiting to pair on this network".to_string(),
                |other| format!("the chosen device did not show up; {other}"),
            ),
            "start `tilo-identity pair` on the new device (it waits 270s), on the same network \
             segment, then run this again",
        ))
    }

    fn graphical_session() -> bool {
        ["WAYLAND_DISPLAY", "DISPLAY"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
    }

    fn approve(
        context: &Context,
        device: Option<&str>,
        code: Option<&str>,
        timeout: u64,
        elevate: Elevate,
    ) -> anyhow::Result<()> {
        refuse_root()?;
        let identity = active_identity(&context.identity_dir)?.ok_or_else(|| {
            guided(
                "this account has no identity to approve devices into",
                "create one with `tilo-identity create`",
            )
        })?;
        let cert = device_cert(&context.paths, identity.public_key).ok_or_else(|| {
            guided(
                "this device holds no valid certificate for the identity",
                restart_hint(),
            )
        })?;
        if !cert.capabilities.can_admit_devices {
            return Err(guided(
                "this device cannot approve devices",
                "approve from the device that created the identity",
            ));
        }
        if !context.json {
            eprintln!("Looking for a device waiting in `tilo-identity pair`...");
        }
        let started = std::time::Instant::now();
        let candidate = discover(device, timeout.min(60))?;
        if let Some(code) = code
            && code.trim() != candidate.verification_code
        {
            return Err(guided(
                format!(
                    "'{}' shows the code {}, not {}",
                    candidate.label,
                    candidate.verification_code,
                    code.trim()
                ),
                "make sure you are approving the device you meant to; nothing was approved",
            ));
        }
        if !context.json {
            eprintln!(
                "Approving '{}' ({}) with code {}",
                candidate.label, candidate.device_node_id, candidate.verification_code
            );
        }
        let remaining = timeout
            .saturating_sub(started.elapsed().as_secs())
            .clamp(15, 270);
        let use_pkexec = match elevate {
            Elevate::Auto => graphical_session(),
            Elevate::Pkexec => true,
            Elevate::Sudo => false,
        };
        let mut command = Command::new(if use_pkexec { "pkexec" } else { "sudo" });
        command
            .arg(HELPER)
            .args(["approve-device", "--identity-root"])
            .arg(context.identity_root())
            .arg("--identity-dir")
            .arg(&context.identity_dir)
            .args(["--expected-device", &candidate.device_node_id])
            .args(["--expected-service", &candidate.relay_service_node_id])
            .args(["--timeout", &remaining.to_string()]);
        let result = run_forwarding(&mut command, "approval")?;
        let approved = result
            .lines
            .iter()
            .rev()
            .find_map(|line| serde_json::from_str::<Value>(line).ok())
            .unwrap_or(Value::Null);
        let next = format!(
            "on '{}', restart its session if `tilo-identity pair` did not; then \
             `tilo-identity status` on either device shows the other online",
            candidate.label
        );
        emit(
            context,
            &json!({
                "approved_device": candidate.device_node_id,
                "label": candidate.label,
                "code": candidate.verification_code,
                "certificate": approved.get("certificate"),
                "next": next,
            }),
            &format!(
                "Approved '{}' ({}).\nnext: {next}",
                candidate.label, candidate.device_node_id
            ),
        );
        Ok(())
    }

    fn revoke(context: &Context, device: &str, reason: Reason, yes: bool) -> anyhow::Result<()> {
        refuse_root()?;
        let identity = active_identity(&context.identity_dir)?
            .ok_or_else(|| guided("this account has no identity", "see `tilo-identity status`"))?;
        let store = DeviceFleetStore::at(&context.identity_dir, context.paths.clone());
        let fleet = store.load(&identity.id)?;
        let matches: Vec<_> = fleet
            .certificates
            .iter()
            .filter(|cert| hex::encode(cert.device_public_key).starts_with(device))
            .collect();
        let cert = match matches.as_slice() {
            [cert] => *cert,
            [] => {
                return Err(guided(
                    format!("no device of this identity matches {device}"),
                    "list them with `tilo-identity devices`",
                ));
            }
            _ => {
                return Err(guided(
                    format!("{device} matches several devices"),
                    "give more of the device id",
                ));
            }
        };
        if device_cert(&context.paths, identity.public_key)
            .is_some_and(|local| local.device_public_key == cert.device_public_key)
        {
            return Err(guided(
                "that is this device",
                "revoke it from another of the person's devices",
            ));
        }
        if !yes {
            return Err(guided(
                format!("revoking '{}' cannot be undone", cert.device_label),
                "pass --yes to revoke it",
            ));
        }
        let entry = store
            .revoke(
                &identity.id,
                cert.device_public_key,
                reason.into(),
                now_ns(),
            )
            .map_err(|error| {
                anyhow!(error).context("revoke the device (signing needs this account's session)")
            })?;
        emit(
            context,
            &json!({
                "revoked_device": hex::encode(entry.revoked_device_public_key),
                "label": cert.device_label,
            }),
            &format!("Revoked '{}'.", cert.device_label),
        );
        Ok(())
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
