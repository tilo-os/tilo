//! Turning a generation into running sessions.
//!
//! The decisions (what restarts, which drop-ins, in which order) live here
//! and are unit-tested against [`RecordingApply`]. The side effects sit
//! behind [`Apply`]; [`crate::systemd::SystemdApply`] performs them on a real
//! dev image.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tilo_vm_protocol::{
    Action, BootChange, BootConfirmResult, ComponentInfo, ControlResult, InputAction, OutputSpec,
    Policy, Scope, SlotKind,
};

use crate::error::DaemonError;
use crate::input::Geometry;
use crate::slots::{DeviceSession, Generation, SlotRecord};

pub const LAYERS_DROPIN: &str = "50-tilo-vm-layers.conf";
pub const SEAT_DROPIN: &str = "50-tilo-vm-seat.conf";
pub const GREETER_UNIT: &str = "greetd.service";

pub fn session_unit(slot: &str) -> String {
    format!("tilo-vm-session@{slot}.service")
}

/// Where a slot session's control socket is: the `RuntimeDirectory=` of
/// `tilo-vm-session@<slot>`.
pub fn control_socket(slot: &str) -> PathBuf {
    std::path::Path::new("/run/tilo-vm")
        .join(slot)
        .join("control.sock")
}

/// The session a slot's layers reach: its unit, user and control socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub slot: String,
    /// The system unit that runs it.
    pub unit: String,
    pub user: String,
    /// Units besides `unit` and the user manager that run the session's
    /// processes; they get the layers too.
    pub units: Vec<String>,
    pub control_socket: PathBuf,
    /// A slot's session stands in for a greetd login and is checked to be
    /// equivalent to one; the device's own session is the production one.
    pub stand_in: bool,
}

impl SlotRecord {
    /// The session this slot's layers reach; `None` for the `vm` slot.
    pub fn session(&self) -> Option<Session> {
        let user = self.user()?;
        Some(match &self.device {
            Some(device) if self.kind == SlotKind::Device => Session {
                slot: self.name.clone(),
                unit: device.unit.clone(),
                user,
                units: device.units.clone(),
                control_socket: device.control_socket.clone(),
                stand_in: false,
            },
            _ => Session {
                slot: self.name.clone(),
                unit: session_unit(&self.name),
                user,
                units: Vec::new(),
                control_socket: control_socket(&self.name),
                stand_in: true,
            },
        })
    }
}

pub fn user_unit(uid: u32) -> String {
    format!("user@{uid}.service")
}

/// Side effects on the guest. Each method is idempotent and reports a
/// failed side effect as [`DaemonError::Apply`] or [`DaemonError::Io`].
#[expect(
    clippy::missing_errors_doc,
    reason = "the trait documents its single error contract once"
)]
pub trait Apply {
    /// Create the slot's system user if missing; return its uid.
    fn ensure_user(&mut self, user: &str) -> Result<u32, DaemonError>;
    /// The uid of an existing account, which is left as it is.
    fn account_uid(&mut self, user: &str) -> Result<u32, DaemonError>;
    /// The session this machine runs for its account without a greeter,
    /// if it runs one (see [`crate::systemd`]).
    fn device_session(&mut self) -> Option<DeviceSession> {
        None
    }
    fn remove_user(&mut self, user: &str) -> Result<(), DaemonError>;
    fn write_dropin(&mut self, unit: &str, file: &str, contents: &str) -> Result<(), DaemonError>;
    fn remove_dropin(&mut self, unit: &str, file: &str) -> Result<(), DaemonError>;
    /// `systemctl daemon-reload`.
    fn reload(&mut self) -> Result<(), DaemonError>;
    fn start_unit(&mut self, unit: &str) -> Result<(), DaemonError>;
    fn stop_unit(&mut self, unit: &str) -> Result<(), DaemonError>;
    fn restart_unit(&mut self, unit: &str) -> Result<(), DaemonError>;
    /// `systemctl is-active` output, e.g. `active`, `inactive`, `failed`.
    fn unit_state(&mut self, unit: &str) -> String;
    /// Give a staged layer the `SELinux` labels its files get under `/`.
    fn label_layer(&mut self, layer: &std::path::Path) -> Result<(), DaemonError>;
    /// Point the VM-wide sysext set at exactly `layers`.
    fn link_sysext(&mut self, layers: &[PathBuf]) -> Result<(), DaemonError>;
    fn sysext_refresh(&mut self) -> Result<(), DaemonError>;
    /// Create a vkms card with `connectors` connected outputs and a logind
    /// seat for it; return the seat name.
    fn allocate_seat(&mut self, slot: &str, connectors: u32) -> Result<String, DaemonError>;
    /// Give the running session's outputs these modes and scales through
    /// the display settings, as `user` (`tilo-vmd display-apply`).
    fn apply_display(
        &mut self,
        slot: &str,
        user: &str,
        outputs: &[OutputSpec],
    ) -> Result<String, DaemonError>;
    fn release_seat(&mut self, slot: &str, seat: &str) -> Result<(), DaemonError>;
    /// Remove `user`'s desktop layout (`~/.config/tilo/state.json` and the
    /// document, meta and invocation records beside it). The session must
    /// be stopped, or it writes the layout back.
    fn reset_desktop_state(&mut self, user: &str) -> Result<(), DaemonError>;
    /// The last `lines` journal entries matching any of `matches`
    /// (journalctl `FIELD=value` terms, `OR`ed).
    fn journal(&mut self, matches: &[String], lines: u32) -> Result<String, DaemonError>;
    /// Run the in-VM `tilo-control` against this session.
    fn control(&mut self, session: &Session, args: &[String])
    -> Result<ControlResult, DaemonError>;
    /// PNG of the session's live scene.
    fn shot(&mut self, session: &Session) -> Result<Vec<u8>, DaemonError>;
    /// Capabilities and `SELinux` domain of the running session process,
    /// or `None` when it is not running.
    fn session_process(&mut self, session: &Session) -> Option<SessionProcess>;
    /// The main process of the running session unit, whose mount
    /// namespace has the slot's layers over `/usr`.
    fn session_pid(&mut self, _session: &Session) -> Option<u32> {
        None
    }
    /// The session variables ([`crate::exec::SESSION_ENV`]) of the
    /// running session, which a command run as the slot inherits.
    fn session_environment(&mut self, _session: &Session) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Wait until the session unit is up and stays up.
    ///
    /// # Errors
    /// The unit's state when it never settled (a start that fails and
    /// restarts looks `active` for a moment, so one sample is not enough).
    fn wait_session_ready(&mut self, _session: &Session) -> Result<(), String> {
        Ok(())
    }
    /// `(free, total)` bytes of the filesystem holding `path`, when known.
    fn disk_space(&mut self, path: &std::path::Path) -> Option<(u64, u64)>;
    /// Send input through `seat`'s uinput devices, creating them on first
    /// use; returns the number of kernel events written. `geometry` gives
    /// the coordinate spaces of a slot with several outputs (`None`: every
    /// device spans the seat's first output).
    fn input(
        &mut self,
        slot: &str,
        seat: &str,
        geometry: Option<Geometry>,
        actions: &[InputAction],
    ) -> Result<u64, DaemonError>;
    /// Remove the slot's uinput devices (they follow its seat).
    fn stop_input(&mut self, slot: &str) -> Result<(), DaemonError>;
    /// `tilo-install install --allow-self-signed --force <tapp>` as `user`;
    /// returns what it printed.
    fn install_app(&mut self, user: &str, tapp: &std::path::Path) -> Result<String, DaemonError>;
    /// Make the kernel in `layer` (already merged into `/usr`) the next boot,
    /// or with `None` put the image's own kernel back (see [`crate::boot`]).
    /// Returns what the next boot starts, and warnings.
    fn boot_kernel(
        &mut self,
        layer: Option<&std::path::Path>,
    ) -> Result<(BootChange, Vec<String>), DaemonError>;
    /// After the reboot: report the running kernel; make its entry the
    /// default when it is the expected one.
    fn confirm_boot(
        &mut self,
        expect_version: Option<&str>,
    ) -> Result<BootConfirmResult, DaemonError>;
}

/// What makes a session equivalent to a greetd one, as read from `/proc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionProcess {
    pub cap_amb: u64,
    pub cap_eff: u64,
    /// `/proc/<pid>/attr/current`; `None` without `SELinux`.
    pub context: Option<String>,
}

/// Why `process` differs from a greetd session, if it does. A greetd
/// session has no ambient or effective capabilities and runs in a user
/// domain (`expected_domain`, the type field of the `SELinux` context).
pub fn parity_problem(process: &SessionProcess, expected_domain: &str) -> Option<String> {
    let mut problems = Vec::new();
    if process.cap_amb != 0 {
        problems.push(format!("CapAmb is {:016x}, expected 0", process.cap_amb));
    }
    if process.cap_eff != 0 {
        problems.push(format!("CapEff is {:016x}, expected 0", process.cap_eff));
    }
    if let Some(context) = &process.context {
        let domain = context.split(':').nth(2).unwrap_or(context);
        if domain != expected_domain {
            problems.push(format!(
                "SELinux domain is {domain}, expected {expected_domain}"
            ));
        }
    }
    (!problems.is_empty()).then(|| problems.join("; "))
}

/// Refuse components whose scope does not fit the slot they are going into.
///
/// # Errors
/// [`DaemonError::ScopeMismatch`].
pub fn check_scope(
    record: &SlotRecord,
    component: &str,
    info: &ComponentInfo,
) -> Result<(), DaemonError> {
    let fits = match record.kind {
        SlotKind::Vm => info.scope == Scope::Vm,
        SlotKind::Headful | SlotKind::Headless | SlotKind::Device => info.scope != Scope::Vm,
    };
    if fits {
        Ok(())
    } else {
        Err(DaemonError::ScopeMismatch {
            component: component.to_owned(),
            slot: record.name.clone(),
            scope: info.scope.as_str().into(),
        })
    }
}

/// What moving from `from` to `to` requires, and which system units a
/// VM-wide change restarts.
///
/// # Errors
/// A changed component whose scope does not fit the slot.
pub fn policy(
    record: &SlotRecord,
    from: &Generation,
    to: &Generation,
) -> Result<(Policy, Vec<String>), DaemonError> {
    let mut policy = Policy::NoRestart;
    let mut units: Vec<String> = Vec::new();
    let names: std::collections::BTreeSet<&String> =
        from.components.keys().chain(to.components.keys()).collect();
    for name in names {
        let (before, after) = (from.components.get(name), to.components.get(name));
        if before == after {
            continue;
        }
        let Some(entry) = after.or(before) else {
            continue;
        };
        check_scope(record, name, &entry.info)?;
        let needed = match entry.info.scope {
            Scope::App => Policy::NoRestart,
            Scope::Session => Policy::SessionRestart,
            Scope::Vm if entry.info.reboot => Policy::Reboot,
            Scope::Vm => {
                for unit in &entry.info.units {
                    if !units.contains(unit) {
                        units.push(unit.clone());
                    }
                }
                Policy::VmRefresh
            }
        };
        policy = policy.max(needed);
    }
    Ok((policy, units))
}

pub fn layers_dropin(slot: &str, generation: u64, layers: &[PathBuf]) -> String {
    let dirs: Vec<String> = layers.iter().map(|p| p.display().to_string()).collect();
    let mut text = format!(
        "# Written by tilo-vmd: slot {slot}, generation {generation}. Do not edit.\n[Service]\nExtensionDirectories=\n"
    );
    if !dirs.is_empty() {
        text.push_str("ExtensionDirectories=");
        text.push_str(&dirs.join(" "));
        text.push('\n');
    }
    text
}

/// Seat selection for the session unit. `None` leaves the template's
/// headless default in place. `pam_systemd` takes the seat from `XDG_SEAT`;
/// seat0 additionally needs VT 1, which greetd gives up for the claim.
pub fn seat_dropin(record: &SlotRecord) -> Option<String> {
    let header = format!(
        "# Written by tilo-vmd: slot {}. Do not edit.\n",
        record.name
    );
    if record.seat0.is_some() {
        return Some(format!(
            "{header}[Unit]\nConflicts={GREETER_UNIT}\nAfter={GREETER_UNIT}\n\n[Service]\n\
             Environment=TILO_VM_BACKEND=drm XDG_SEAT=seat0 XDG_VTNR=1\n\
             TTYPath=/dev/tty1\nTTYReset=yes\nTTYVHangup=yes\nTTYVTDisallocate=yes\n\
             StandardInput=tty-fail\nStandardOutput=journal\nStandardError=journal\n"
        ));
    }
    match (&record.seat, record.kind) {
        (Some(seat), SlotKind::Headful) => Some(format!(
            "{header}[Service]\nEnvironment=TILO_VM_BACKEND=drm XDG_SEAT={seat}\n"
        )),
        _ => None,
    }
}

/// How the session reacts to an activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionChange {
    /// Start the session if it is not running (creation).
    Ensure,
    /// Log out and log in again.
    Restart,
    /// Leave the running session alone.
    Keep,
}

/// Bring a slot's system state in line with `generation`.
///
/// # Errors
/// Any failing side effect.
pub fn activate(
    apply: &mut dyn Apply,
    record: &mut SlotRecord,
    generation: &Generation,
    layers: &[PathBuf],
    policy: Policy,
    units: &[String],
    change: SessionChange,
) -> Result<Vec<Action>, DaemonError> {
    let mut actions = Vec::new();
    let Some(session) = record.session() else {
        apply.link_sysext(layers)?;
        apply.sysext_refresh()?;
        actions.push(Action::SysextRefresh);
        for unit in units {
            apply.restart_unit(unit)?;
            actions.push(Action::RestartUnit { unit: unit.clone() });
        }
        return Ok(actions);
    };

    let uid = session_uid(apply, &session)?;
    if session.stand_in {
        actions.push(Action::EnsureUser {
            user: session.user.clone(),
            uid,
        });
    }
    if record.kind == SlotKind::Headful {
        // Idempotent, and re-creates the card after a VM reboot emptied configfs.
        let seat = apply.allocate_seat(&record.name, record.connectors())?;
        if record.seat.as_ref() != Some(&seat) {
            actions.push(Action::AllocateSeat { seat: seat.clone() });
        }
        record.seat = Some(seat);
    }

    let dropin = layers_dropin(&record.name, generation.number, layers);
    for unit in layered_units(&session, uid) {
        apply.write_dropin(&unit, LAYERS_DROPIN, &dropin)?;
        actions.push(Action::WriteDropIn { unit });
    }
    // The device's own session picks its seat itself.
    if session.stand_in {
        match seat_dropin(record) {
            Some(text) => apply.write_dropin(&session.unit, SEAT_DROPIN, &text)?,
            None => apply.remove_dropin(&session.unit, SEAT_DROPIN)?,
        }
    }
    apply.reload()?;

    let change = match (change, policy) {
        (SessionChange::Keep, Policy::SessionRestart | Policy::VmRefresh | Policy::Reboot) => {
            SessionChange::Restart
        }
        (change, _) => change,
    };
    match change {
        SessionChange::Ensure => {
            apply.start_unit(&session.unit)?;
            actions.push(Action::StartUnit { unit: session.unit });
        }
        SessionChange::Restart => actions.extend(restart_session(apply, &session, uid)?),
        SessionChange::Keep => actions.push(Action::Deferred {
            reason:
                "app-only change: live at next app launch; other layers at next session restart"
                    .into(),
        }),
    }
    Ok(actions)
}

/// A normal session restart: log out, then a fresh login. Each unit's mount
/// namespace freezes `/usr` when it starts, so both the session and the user
/// manager must restart to see a new layer set or a refreshed VM `/usr`; a
/// daemon-reload or reexec keeps the old view.
///
/// # Errors
/// Any failing unit operation.
pub fn restart_session(
    apply: &mut dyn Apply,
    session: &Session,
    uid: u32,
) -> Result<Vec<Action>, DaemonError> {
    let user_manager = user_unit(uid);
    apply.stop_unit(&session.unit)?;
    apply.restart_unit(&user_manager)?;
    apply.start_unit(&session.unit)?;
    Ok(vec![
        Action::StopUnit {
            unit: session.unit.clone(),
        },
        Action::RestartUnit { unit: user_manager },
        Action::StartUnit {
            unit: session.unit.clone(),
        },
    ])
}

/// The uid the session runs as: a slot's own user, created when missing,
/// or the existing account of the device's session, left untouched.
///
/// # Errors
/// A user that cannot be created, or an account that does not exist.
pub fn session_uid(apply: &mut dyn Apply, session: &Session) -> Result<u32, DaemonError> {
    if session.stand_in {
        apply.ensure_user(&session.user)
    } else {
        apply.account_uid(&session.user)
    }
}

/// Every unit whose mount namespace must show the slot's layers: the user
/// manager (it starts the session's user services), the session unit and
/// the units that unit starts for the session.
pub fn layered_units(session: &Session, uid: u32) -> Vec<String> {
    let mut units = vec![user_unit(uid), session.unit.clone()];
    units.extend(session.units.iter().cloned());
    units
}

/// Undo everything [`activate`] did for a user slot. The session stops
/// before its seat is released: a vkms card removed under a live session
/// leaves the unit running with a dead device and `Restart=` never fires.
///
/// # Errors
/// Any failing side effect.
pub fn deactivate(apply: &mut dyn Apply, record: &SlotRecord) -> Result<Vec<Action>, DaemonError> {
    let mut actions = Vec::new();
    let Some(Session {
        user,
        unit: session,
        stand_in: true,
        ..
    }) = record.session()
    else {
        return Ok(actions);
    };
    apply.stop_unit(&session)?;
    actions.push(Action::StopUnit {
        unit: session.clone(),
    });
    if record.kind == SlotKind::Headful {
        apply.stop_input(&record.name)?;
    }
    let uid = apply.ensure_user(&user)?;
    let user_manager = user_unit(uid);
    apply.stop_unit(&user_manager)?;
    actions.push(Action::StopUnit {
        unit: user_manager.clone(),
    });
    for (unit, file) in [
        (&session, LAYERS_DROPIN),
        (&session, SEAT_DROPIN),
        (&user_manager, LAYERS_DROPIN),
    ] {
        apply.remove_dropin(unit, file)?;
    }
    actions.push(Action::RemoveDropIn { unit: session });
    actions.push(Action::RemoveDropIn { unit: user_manager });
    apply.reload()?;
    if let Some(seat) = &record.seat {
        apply.release_seat(&record.name, seat)?;
        actions.push(Action::ReleaseSeat { seat: seat.clone() });
    }
    apply.remove_user(&user)?;
    actions.push(Action::RemoveUser { user });
    Ok(actions)
}

/// An [`Apply`] that performs nothing and records every call. Used by unit
/// tests and by `tilo-vmd rpc --apply record` for protocol tests.
#[derive(Debug, Default)]
pub struct RecordingApply {
    pub calls: Vec<String>,
    pub dropins: BTreeMap<(String, String), String>,
    users: BTreeMap<String, u32>,
    units: BTreeMap<String, bool>,
    pub sysext: Vec<PathBuf>,
    /// The kernel layer the next boot runs; `None` for the image's.
    pub next_kernel: Option<PathBuf>,
    /// Answers for the next `control` calls, in order; an echo of the
    /// arguments once they run out.
    pub control_replies: std::collections::VecDeque<ControlResult>,
    /// The machine's own session, when it has one.
    pub device: Option<DeviceSession>,
}

impl RecordingApply {
    /// A machine whose own session runs as `account`, as a board image's does.
    pub fn with_device(account: &str) -> Self {
        Self {
            device: Some(DeviceSession {
                unit: format!("tilo-board-headless-session@{account}.service"),
                account: account.to_owned(),
                units: vec!["tilo-board-drm-session.service".into()],
                control_socket: PathBuf::from("/run/user/1000/tilo-session/test-control.sock"),
            }),
            ..Self::default()
        }
    }

    fn call(&mut self, text: String) {
        self.calls.push(text);
    }
}

impl Apply for RecordingApply {
    fn ensure_user(&mut self, user: &str) -> Result<u32, DaemonError> {
        self.call(format!("ensure_user {user}"));
        let next = u32::try_from(self.users.len())
            .unwrap_or(0)
            .saturating_add(60_001);
        Ok(*self.users.entry(user.to_owned()).or_insert(next))
    }
    fn account_uid(&mut self, user: &str) -> Result<u32, DaemonError> {
        self.call(format!("account_uid {user}"));
        Ok(1000)
    }
    fn device_session(&mut self) -> Option<DeviceSession> {
        self.device.clone()
    }
    fn remove_user(&mut self, user: &str) -> Result<(), DaemonError> {
        self.call(format!("remove_user {user}"));
        self.users.remove(user);
        Ok(())
    }
    fn write_dropin(&mut self, unit: &str, file: &str, contents: &str) -> Result<(), DaemonError> {
        self.call(format!("write_dropin {unit} {file}"));
        self.dropins
            .insert((unit.to_owned(), file.to_owned()), contents.to_owned());
        Ok(())
    }
    fn remove_dropin(&mut self, unit: &str, file: &str) -> Result<(), DaemonError> {
        if self
            .dropins
            .remove(&(unit.to_owned(), file.to_owned()))
            .is_some()
        {
            self.call(format!("remove_dropin {unit} {file}"));
        }
        Ok(())
    }
    fn reload(&mut self) -> Result<(), DaemonError> {
        self.call("reload".into());
        Ok(())
    }
    fn start_unit(&mut self, unit: &str) -> Result<(), DaemonError> {
        self.call(format!("start {unit}"));
        self.units.insert(unit.to_owned(), true);
        Ok(())
    }
    fn stop_unit(&mut self, unit: &str) -> Result<(), DaemonError> {
        self.call(format!("stop {unit}"));
        self.units.insert(unit.to_owned(), false);
        Ok(())
    }
    fn restart_unit(&mut self, unit: &str) -> Result<(), DaemonError> {
        self.call(format!("restart {unit}"));
        self.units.insert(unit.to_owned(), true);
        Ok(())
    }
    fn unit_state(&mut self, unit: &str) -> String {
        if self.units.get(unit).copied().unwrap_or(false) {
            "active"
        } else {
            "inactive"
        }
        .into()
    }
    fn label_layer(&mut self, _layer: &std::path::Path) -> Result<(), DaemonError> {
        self.call("label_layer".into());
        Ok(())
    }
    fn link_sysext(&mut self, layers: &[PathBuf]) -> Result<(), DaemonError> {
        self.call(format!("link_sysext {}", layers.len()));
        self.sysext = layers.to_vec();
        Ok(())
    }
    fn sysext_refresh(&mut self) -> Result<(), DaemonError> {
        self.call("sysext_refresh".into());
        Ok(())
    }
    fn allocate_seat(&mut self, slot: &str, connectors: u32) -> Result<String, DaemonError> {
        if connectors == 1 {
            self.call(format!("allocate_seat {slot}"));
        } else {
            self.call(format!("allocate_seat {slot} {connectors}"));
        }
        Ok(format!("seat-{slot}"))
    }
    fn apply_display(
        &mut self,
        slot: &str,
        _user: &str,
        outputs: &[OutputSpec],
    ) -> Result<String, DaemonError> {
        let list = tilo_vm_protocol::display::format_outputs(outputs);
        self.call(format!("apply_display {slot} {list}"));
        Ok(format!("outputs set to {list}"))
    }
    fn release_seat(&mut self, slot: &str, seat: &str) -> Result<(), DaemonError> {
        self.call(format!("release_seat {slot} {seat}"));
        Ok(())
    }
    fn reset_desktop_state(&mut self, user: &str) -> Result<(), DaemonError> {
        self.call(format!("reset_desktop_state {user}"));
        Ok(())
    }
    fn journal(&mut self, matches: &[String], lines: u32) -> Result<String, DaemonError> {
        self.call(format!("journal {lines}"));
        Ok(format!("(recorded) journal of {}\n", matches.join(" + ")))
    }
    fn control(
        &mut self,
        session: &Session,
        args: &[String],
    ) -> Result<ControlResult, DaemonError> {
        self.call(format!("control {} {}", session.slot, args.join(" ")));
        if let Some(reply) = self.control_replies.pop_front() {
            return Ok(reply);
        }
        Ok(ControlResult {
            exit_code: 0,
            stdout: format!("{}\n", args.join(" ")),
            stderr: String::new(),
        })
    }
    fn shot(&mut self, session: &Session) -> Result<Vec<u8>, DaemonError> {
        self.call(format!("shot {}", session.slot));
        Ok(b"\x89PNG\r\n\x1a\n".to_vec())
    }
    fn disk_space(&mut self, _path: &std::path::Path) -> Option<(u64, u64)> {
        None
    }
    fn input(
        &mut self,
        slot: &str,
        seat: &str,
        geometry: Option<Geometry>,
        actions: &[InputAction],
    ) -> Result<u64, DaemonError> {
        match geometry {
            Some(Geometry {
                desktop: (w, h), ..
            }) => self.call(format!("input {slot} {seat} {w}x{h} {}", actions.len())),
            None => self.call(format!("input {slot} {seat} {}", actions.len())),
        }
        Ok(u64::try_from(actions.len()).unwrap_or(0).saturating_mul(2))
    }
    fn stop_input(&mut self, slot: &str) -> Result<(), DaemonError> {
        self.call(format!("stop_input {slot}"));
        Ok(())
    }
    fn install_app(&mut self, user: &str, tapp: &std::path::Path) -> Result<String, DaemonError> {
        self.call(format!("install_app {user}"));
        let name = tapp
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(format!("installed: so.tilo.{name} 0.1.0\n"))
    }
    fn boot_kernel(
        &mut self,
        layer: Option<&std::path::Path>,
    ) -> Result<(BootChange, Vec<String>), DaemonError> {
        self.call(format!(
            "boot_kernel {}",
            layer.map_or_else(|| "image".into(), |l| l.display().to_string())
        ));
        self.next_kernel = layer.map(std::path::Path::to_path_buf);
        let entry = layer
            .and_then(std::path::Path::file_name)
            .map(|h| format!("tilo-vm-recorded-{}", h.to_string_lossy()));
        Ok((
            BootChange {
                kver: layer.map(|_| "0.0.0+dev".into()),
                entry,
                fallback: Some("image".into()),
                loader: "recorded".into(),
                initramfs: None,
                initramfs_seconds: 0.0,
            },
            Vec::new(),
        ))
    }
    fn confirm_boot(
        &mut self,
        _expect_version: Option<&str>,
    ) -> Result<BootConfirmResult, DaemonError> {
        self.call("confirm_boot".into());
        Ok(BootConfirmResult {
            running: "0.0.0+dev".into(),
            version: "#1".into(),
            expected: self.next_kernel.as_ref().map(|_| "0.0.0+dev".into()),
            confirmed: true,
            default_entry: None,
        })
    }
    fn session_process(&mut self, session: &Session) -> Option<SessionProcess> {
        self.units
            .get(&session.unit)
            .copied()
            .unwrap_or(false)
            .then(|| SessionProcess {
                cap_amb: 0,
                cap_eff: 0,
                context: Some("unconfined_u:unconfined_r:unconfined_t:s0-s0:c0.c1023".into()),
            })
    }
}

#[cfg(test)]
mod tests {
    use tilo_vm_protocol::Owner;

    use super::*;
    use crate::slots::Entry;

    fn record(name: &str, kind: SlotKind) -> SlotRecord {
        SlotRecord {
            name: name.into(),
            kind,
            owner: Owner::default(),
            created_at: String::new(),
            current: 0,
            seat: None,
            seat0: None,
            outputs: Vec::new(),
            device: None,
        }
    }

    fn generation(number: u64, components: &[(&str, &str, Scope, &[&str])]) -> Generation {
        let mut g = Generation {
            number,
            ..Generation::default()
        };
        for (name, layer, scope, units) in components {
            g.layers.push((*layer).into());
            g.components.insert(
                (*name).into(),
                Entry {
                    layer: (*layer).into(),
                    info: ComponentInfo::new(
                        *scope,
                        units.iter().map(|u| (*u).to_string()).collect(),
                    ),
                },
            );
        }
        g
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn apply_scope_decides_swap_refresh_and_session_policy() {
        // Phase `swap_policy_follows_scope`.
        {
            let user = record("a", SlotKind::Headless);
            let base = generation(
                1,
                &[
                    ("shell", "h1", Scope::Session, &[]),
                    ("apps+runtime", "a1", Scope::App, &[]),
                ],
            );
            let shell = generation(
                2,
                &[
                    ("shell", "h2", Scope::Session, &[]),
                    ("apps+runtime", "a1", Scope::App, &[]),
                ],
            );
            let apps = generation(
                3,
                &[
                    ("shell", "h1", Scope::Session, &[]),
                    ("apps+runtime", "a2", Scope::App, &[]),
                ],
            );
            assert_eq!(
                policy(&user, &base, &shell).unwrap().0,
                Policy::SessionRestart
            );
            assert_eq!(policy(&user, &base, &apps).unwrap().0, Policy::NoRestart);
            assert_eq!(policy(&user, &base, &base).unwrap().0, Policy::NoRestart);
            // Removing a session component (rollback to base) restarts too.
            assert_eq!(
                policy(&user, &shell, &Generation::default()).unwrap().0,
                Policy::SessionRestart
            );

            let vm = record("vm", SlotKind::Vm);
            let fs1 = generation(1, &[("fs", "f1", Scope::Vm, &["fs.service"])]);
            let fs2 = generation(
                2,
                &[
                    ("fs", "f2", Scope::Vm, &["fs.service"]),
                    ("greeter", "g", Scope::Vm, &["greetd.service", "fs.service"]),
                ],
            );
            let (p, units) = policy(&vm, &fs1, &fs2).unwrap();
            assert_eq!(p, Policy::VmRefresh);
            assert_eq!(units, ["fs.service", "greetd.service"]);

            // A kernel outranks a refresh, and restarts no unit of its own.
            let mut kernel = fs2.clone();
            kernel.number = 3;
            kernel.layers.push("k1".into());
            kernel.components.insert(
                "kernel".into(),
                Entry {
                    layer: "k1".into(),
                    info: ComponentInfo {
                        reboot: true,
                        ..ComponentInfo::new(Scope::Vm, vec![])
                    },
                },
            );
            let (p, units) = policy(&vm, &fs2, &kernel).unwrap();
            assert_eq!(p, Policy::Reboot);
            assert!(units.is_empty(), "{units:?}");
            // Rolling the kernel back is a reboot too.
            assert_eq!(policy(&vm, &kernel, &fs2).unwrap().0, Policy::Reboot);
            assert_eq!(policy(&vm, &kernel, &kernel).unwrap().0, Policy::NoRestart);
        }
        // Phase `scope_misuse_is_refused`.
        {
            let user = record("a", SlotKind::Headful);
            let vm = record("vm", SlotKind::Vm);
            let fs = ComponentInfo::new(Scope::Vm, vec![]);
            let shell = ComponentInfo::new(Scope::Session, vec![]);
            assert!(matches!(
                check_scope(&user, "fs", &fs),
                Err(DaemonError::ScopeMismatch { .. })
            ));
            assert!(matches!(
                check_scope(&vm, "shell", &shell),
                Err(DaemonError::ScopeMismatch { .. })
            ));
            assert!(check_scope(&user, "shell", &shell).is_ok());
            assert!(check_scope(&vm, "fs", &fs).is_ok());
        }
        // Phase `app_only_change_keeps_session`.
        {
            let mut apply = RecordingApply::default();
            let mut slot = record("a", SlotKind::Headless);
            let actions = activate(
                &mut apply,
                &mut slot,
                &Generation::default(),
                &[],
                Policy::NoRestart,
                &[],
                SessionChange::Keep,
            )
            .unwrap();
            assert!(
                !apply
                    .calls
                    .iter()
                    .any(|c| c.starts_with("stop") || c.starts_with("start"))
            );
            assert!(matches!(actions.last(), Some(Action::Deferred { .. })));
        }
        // Phase `vm_slot_refreshes_sysext_and_restarts_units`.
        {
            let mut apply = RecordingApply::default();
            let mut vm = record("vm", SlotKind::Vm);
            let layers = [PathBuf::from("/l/f2")];
            activate(
                &mut apply,
                &mut vm,
                &Generation::default(),
                &layers,
                Policy::VmRefresh,
                &["fs.service".into()],
                SessionChange::Keep,
            )
            .unwrap();
            assert_eq!(
                apply.calls,
                ["link_sysext 1", "sysext_refresh", "restart fs.service"]
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn session_lifecycle_matches_greetd_and_cleans_up() {
        // Phase `parity_matches_a_greetd_session`.
        {
            let greetd = SessionProcess {
                cap_amb: 0,
                cap_eff: 0,
                context: Some("unconfined_u:unconfined_r:unconfined_t:s0-s0:c0.c1023".into()),
            };
            assert_eq!(parity_problem(&greetd, "unconfined_t"), None);
            assert_eq!(
                parity_problem(
                    &SessionProcess {
                        context: None,
                        ..greetd
                    },
                    "unconfined_t"
                ),
                None
            );
            let unit = SessionProcess {
                cap_amb: 0x8_0000_0000,
                cap_eff: 0,
                context: Some("system_u:system_r:unconfined_service_t:s0".into()),
            };
            let problem = parity_problem(&unit, "unconfined_t").unwrap();
            assert!(problem.contains("CapAmb is 0000000800000000"), "{problem}");
            assert!(
                problem.contains("domain is unconfined_service_t"),
                "{problem}"
            );
        }
        // Phase `session_restart_restarts_user_manager_between_logout_and_login`.
        {
            let mut apply = RecordingApply::default();
            let mut slot = record("a", SlotKind::Headless);
            let g = generation(1, &[("shell", "h1", Scope::Session, &[])]);
            let layers = [PathBuf::from("/var/lib/tilo-vm/layers/h1")];
            let actions = activate(
                &mut apply,
                &mut slot,
                &g,
                &layers,
                Policy::SessionRestart,
                &[],
                SessionChange::Keep,
            )
            .unwrap();
            assert_eq!(
                apply.calls,
                [
                    "ensure_user tds-a",
                    "write_dropin user@60001.service 50-tilo-vm-layers.conf",
                    "write_dropin tilo-vm-session@a.service 50-tilo-vm-layers.conf",
                    "reload",
                    "stop tilo-vm-session@a.service",
                    "restart user@60001.service",
                    "start tilo-vm-session@a.service",
                ]
            );
            assert!(actions.contains(&Action::StartUnit {
                unit: "tilo-vm-session@a.service".into()
            }));
            let dropin =
                &apply.dropins[&("user@60001.service".to_string(), LAYERS_DROPIN.to_string())];
            assert!(dropin.contains(
                "ExtensionDirectories=\nExtensionDirectories=/var/lib/tilo-vm/layers/h1\n"
            ));
        }
        // Phase `the_device_session_restarts_through_its_own_unit_as_its_account`.
        {
            let mut apply = RecordingApply::with_device("quickuser");
            let mut device = record("device", SlotKind::Device);
            device.device = apply.device.clone();
            let g = generation(1, &[("shell", "h1", Scope::Session, &[])]);
            let layers = [PathBuf::from("/var/lib/tilo-vm/layers/h1")];
            let actions = activate(
                &mut apply,
                &mut device,
                &g,
                &layers,
                Policy::SessionRestart,
                &[],
                SessionChange::Keep,
            )
            .unwrap();
            let unit = "tilo-board-headless-session@quickuser.service";
            assert_eq!(
                apply.calls,
                [
                    "account_uid quickuser",
                    "write_dropin user@1000.service 50-tilo-vm-layers.conf",
                    &format!("write_dropin {unit} 50-tilo-vm-layers.conf"),
                    "write_dropin tilo-board-drm-session.service 50-tilo-vm-layers.conf",
                    "reload",
                    &format!("stop {unit}"),
                    "restart user@1000.service",
                    &format!("start {unit}"),
                ]
            );
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, Action::EnsureUser { .. })),
                "the account is not a slot user: {actions:?}"
            );
            let session = device.session().unwrap();
            assert!(!session.stand_in);
            assert_eq!(session.user, "quickuser");
            assert_eq!(
                session.control_socket,
                PathBuf::from("/run/user/1000/tilo-session/test-control.sock")
            );
            // Without the session found, the slot has none.
            device.device = None;
            assert_eq!(device.session(), None);
        }
        // Phase `headful_ensures_its_seat_on_every_activation`.
        {
            let mut apply = RecordingApply::default();
            let mut slot = record("b", SlotKind::Headful);
            let g = Generation::default();
            activate(
                &mut apply,
                &mut slot,
                &g,
                &[],
                Policy::NoRestart,
                &[],
                SessionChange::Ensure,
            )
            .unwrap();
            activate(
                &mut apply,
                &mut slot,
                &g,
                &[],
                Policy::SessionRestart,
                &[],
                SessionChange::Keep,
            )
            .unwrap();
            assert_eq!(
                apply
                    .calls
                    .iter()
                    .filter(|c| c.starts_with("allocate_seat"))
                    .count(),
                2
            );
            let seat = &apply.dropins[&(
                "tilo-vm-session@b.service".to_string(),
                SEAT_DROPIN.to_string(),
            )];
            assert!(seat.contains("TILO_VM_BACKEND=drm XDG_SEAT=seat-b"));
        }
        // Phase `deactivate_cleans_up`.
        {
            let mut apply = RecordingApply::default();
            let mut slot = record("b", SlotKind::Headful);
            activate(
                &mut apply,
                &mut slot,
                &Generation::default(),
                &[],
                Policy::NoRestart,
                &[],
                SessionChange::Ensure,
            )
            .unwrap();
            deactivate(&mut apply, &slot).unwrap();
            assert!(apply.dropins.is_empty());
            let stop = apply
                .calls
                .iter()
                .position(|c| c == "stop tilo-vm-session@b.service")
                .unwrap();
            let release = apply
                .calls
                .iter()
                .position(|c| c == "release_seat b seat-b")
                .unwrap();
            assert!(
                stop < release,
                "the session must stop before its card goes away"
            );
            assert_eq!(
                apply.calls.last().map(String::as_str),
                Some("remove_user tds-b")
            );
        }
    }
}
