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
use crate::slots::{DeviceSession, Generation, Login, SlotRecord};

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
    /// The owner's greetd login on a laptop: layers merge system-wide and a
    /// restart restarts its desktop, never the login.
    pub login: Option<Login>,
}

impl Session {
    /// A login whose owner is logged out: nothing runs to restart.
    pub fn logged_out(&self) -> bool {
        self.login
            .as_ref()
            .is_some_and(|login| login.wrapper.is_none())
    }
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
                login: device.login.clone(),
            },
            _ => Session {
                slot: self.name.clone(),
                unit: session_unit(&self.name),
                user,
                units: Vec::new(),
                control_socket: control_socket(&self.name),
                stand_in: true,
                login: None,
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
    /// Whether this actual account/session is the claimed device owner in wheel.
    fn device_action_owner(&mut self, _session: &Session) -> bool {
        false
    }
    /// Enroll the current encrypted root for the existing owner's actual session.
    fn enroll_device_unlock(
        &mut self,
        _uid: u32,
        _request: &tilo_vm_protocol::DeviceUnlockEnrol,
    ) -> Result<tilo_vm_protocol::DeviceUnlockEnrolResult, DaemonError> {
        Err(DaemonError::Apply {
            step: "device unlock enrollment".into(),
            detail: "this backend does not serve device unlock enrollment".into(),
        })
    }
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
    /// Prepare one readonly combined filesystem view for a stand-in slot.
    fn session_layer_dropin(
        &mut self,
        slot: &str,
        generation: u64,
        layers: &[PathBuf],
    ) -> Result<String, DaemonError>;
    fn remove_session_view(&mut self, slot: &str) -> Result<(), DaemonError>;
    /// `systemctl daemon-reload`.
    fn reload(&mut self) -> Result<(), DaemonError>;
    fn start_unit(&mut self, unit: &str) -> Result<(), DaemonError>;
    fn stop_unit(&mut self, unit: &str) -> Result<(), DaemonError>;
    fn restart_unit(&mut self, unit: &str) -> Result<(), DaemonError>;
    /// Verify a declared component unit is installed and loadable before deferring activation.
    fn validate_unit(&mut self, unit: &str) -> Result<(), DaemonError>;
    /// Restart a declared component unit only if it is already running.
    fn try_restart_unit(&mut self, unit: &str) -> Result<(), DaemonError>;
    /// Restart a login's desktop in place: ask its `tilo-session` (SIGUSR1)
    /// and wait until the old compositor has exited. The login, its user
    /// manager and its services stay.
    fn restart_desktop(&mut self, session: &Session) -> Result<(), DaemonError>;
    /// Reload the running user manager and try-restart an existing component unit.
    fn restart_user_unit(&mut self, user: &str, unit: &str) -> Result<(), DaemonError>;
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
    fn install_app(
        &mut self,
        session: &Session,
        tapp: &std::path::Path,
    ) -> Result<String, DaemonError>;
    /// Select an authenticated development-cohort package, preserving holds.
    fn install_cohort_app(
        &mut self,
        session: &Session,
        tapp: &std::path::Path,
    ) -> Result<String, DaemonError>;
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
    /// Make sure [`crate::awake::AWAKE_UNIT`] runs, holding the machine
    /// awake until the deadline in the lease file `lease`.
    fn hold_awake(&mut self, lease: &std::path::Path) -> Result<(), DaemonError>;
    /// Stop holding the machine awake.
    fn release_awake(&mut self) -> Result<(), DaemonError>;
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
    if info.frame_cpu_trace
        && (component != "shell"
            || info.scope != Scope::Session
            || !matches!(record.kind, SlotKind::Headful | SlotKind::Headless))
    {
        return Err(DaemonError::InvalidParams(
            "phase CPU recording requires a stand-in slot's shell component".into(),
        ));
    }
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

/// The units a change restarts besides the session itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Restarts {
    /// System units of changed VM-wide components.
    pub units: Vec<String>,
    /// User units of changed VM or session components, which a login that keeps
    /// its user manager restarts one by one.
    pub user_units: Vec<String>,
}

impl Restarts {
    /// Every unit `generation`'s components name: what re-applying it whole
    /// restarts.
    pub fn of(generation: &Generation) -> Self {
        let mut restarts = Self::default();
        for entry in generation.components.values() {
            restarts.add(&entry.info);
        }
        restarts
    }

    fn add(&mut self, info: &ComponentInfo) {
        if info.scope == Scope::App {
            return;
        }
        if info.scope == Scope::Vm {
            for unit in &info.units {
                if !self.units.contains(unit) {
                    self.units.push(unit.clone());
                }
            }
        }
        for unit in &info.user_units {
            if !self.user_units.contains(unit) {
                self.user_units.push(unit.clone());
            }
        }
    }
}

/// What moving from `from` to `to` requires, and which units the change
/// restarts.
///
/// # Errors
/// A changed component whose scope does not fit the slot.
pub fn policy(
    record: &SlotRecord,
    from: &Generation,
    to: &Generation,
) -> Result<(Policy, Restarts), DaemonError> {
    let mut policy = Policy::NoRestart;
    let mut restarts = Restarts::default();
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
            Scope::Vm => Policy::VmRefresh,
        };
        if !entry.info.reboot {
            restarts.add(&entry.info);
        }
        policy = policy.max(needed);
    }
    Ok((policy, restarts))
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
    restarts: &Restarts,
    change: SessionChange,
) -> Result<Vec<Action>, DaemonError> {
    let mut actions = Vec::new();
    let Some(session) = record.session() else {
        apply.link_sysext(layers)?;
        apply.sysext_refresh()?;
        actions.push(Action::SysextRefresh);
        for unit in &restarts.units {
            // The greeter component is VM-wide, but its manager also owns
            // the active login. Re-query the host's verified session rather
            // than using a saved slot record or caller-supplied identity.
            if unit == GREETER_UNIT
                && apply.device_session().is_some_and(|device| {
                    device.login.is_some_and(|login| {
                        login.session.is_some_and(|id| !id.is_empty())
                            && login.wrapper.is_some_and(|pid| pid > 0)
                    })
                })
            {
                apply.validate_unit(unit)?;
                actions.push(Action::Deferred {
                    reason: format!(
                        "{unit}: active owner login preserved; updated greeter runs at the next greeter launch"
                    ),
                });
            } else {
                apply.try_restart_unit(unit)?;
                actions.push(Action::RestartUnit { unit: unit.clone() });
            }
        }
        return Ok(actions);
    };
    let change = match (change, policy) {
        (SessionChange::Keep, Policy::SessionRestart | Policy::VmRefresh | Policy::Reboot) => {
            SessionChange::Restart
        }
        (change, _) => change,
    };

    let uid = session_uid(apply, &session)?;
    if session.login.is_some() {
        return activate_login(apply, &session, uid, layers, restarts, change);
    }
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

    let dropin = apply.session_layer_dropin(&record.name, generation.number, layers)?;
    for unit in layered_units(&session, uid) {
        let text = if unit == session.unit
            && generation
                .components
                .get("shell")
                .is_some_and(|entry| entry.info.frame_cpu_trace)
        {
            format!("{dropin}\n[Service]\nEnvironment=TILO_VM_FRAME_CPU_TRACE=1\n")
        } else {
            dropin.clone()
        };
        apply.write_dropin(&unit, LAYERS_DROPIN, &text)?;
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

/// [`activate`] for the owner's greetd login on a laptop. greetd runs the
/// login in the host's mount namespace, so `layers` (this slot's and the
/// `vm` slot's: one system-wide set) merge with `systemd-sysext`, and a
/// restart restarts the changed user units and then the desktop in place;
/// the owner stays logged in.
fn activate_login(
    apply: &mut dyn Apply,
    session: &Session,
    uid: u32,
    layers: &[PathBuf],
    restarts: &Restarts,
    change: SessionChange,
) -> Result<Vec<Action>, DaemonError> {
    apply.link_sysext(layers)?;
    apply.sysext_refresh()?;
    let mut actions = vec![Action::SysextRefresh];
    match change {
        // A login is started by its owner at the greeter, never by tilo-vmd.
        SessionChange::Ensure => {}
        SessionChange::Keep => actions.push(Action::Deferred {
            reason:
                "app-only change: live at next app launch; other layers at the next desktop restart"
                    .into(),
        }),
        SessionChange::Restart if session.logged_out() => {}
        SessionChange::Restart => {
            for unit in &restarts.user_units {
                apply.restart_user_unit(&session.user, unit)?;
                actions.push(Action::RestartUserUnit {
                    user: session.user.clone(),
                    unit: unit.clone(),
                });
            }
        }
    }
    if change == SessionChange::Restart {
        actions.extend(restart_session(apply, session, uid)?);
    }
    Ok(actions)
}

/// A normal session restart: log out, then a fresh login. Each unit's mount
/// namespace freezes `/usr` when it starts, so both the session and the user
/// manager must restart to see a new layer set or a refreshed VM `/usr`; a
/// daemon-reload or reexec keeps the old view.
///
/// The owner's greetd login is never logged out: its desktop restarts in
/// place, and while the owner is logged out the next login takes the change.
///
/// # Errors
/// Any failing unit operation.
pub fn restart_session(
    apply: &mut dyn Apply,
    session: &Session,
    uid: u32,
) -> Result<Vec<Action>, DaemonError> {
    if session.logged_out() {
        return Ok(vec![Action::Deferred {
            reason: format!(
                "{} is logged out: the next login at the greeter runs these layers",
                session.user
            ),
        }]);
    }
    if session.login.is_some() {
        apply.restart_desktop(session)?;
        return Ok(vec![Action::RestartDesktop {
            user: session.user.clone(),
        }]);
    }
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
    apply.remove_session_view(&record.name)?;
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
    pub prepared_session_layers: Vec<(String, Vec<PathBuf>)>,
    /// The kernel layer the next boot runs; `None` for the image's.
    pub next_kernel: Option<PathBuf>,
    /// Answers for the next `control` calls, in order; an echo of the
    /// arguments once they run out.
    pub control_replies: std::collections::VecDeque<ControlResult>,
    /// Scripted readiness observations, consumed once per restarted session.
    pub readiness_replies: std::collections::VecDeque<Result<(), String>>,

    /// Exact normal installer output for production cohort selection tests.
    pub cohort_replies: std::collections::VecDeque<String>,
    /// The machine's own session, when it has one.
    pub device: Option<DeviceSession>,
    /// Every call is copied here too, for a test that hands this to a daemon.
    pub mirror: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
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
                login: None,
            }),
            ..Self::default()
        }
    }

    /// A laptop whose owner `account` is logged in through greetd, in
    /// logind session 3 with `tilo-session` as pid 4242.
    pub fn with_login(account: &str) -> Self {
        Self {
            device: Some(DeviceSession {
                unit: "session-3.scope".into(),
                account: account.to_owned(),
                units: Vec::new(),
                control_socket: PathBuf::from("/run/user/1000/tilo-session/test-control.sock"),
                login: Some(Login {
                    session: Some("3".into()),
                    wrapper: Some(4242),
                }),
            }),
            ..Self::default()
        }
    }

    fn call(&mut self, text: String) {
        if let Some(mirror) = &self.mirror
            && let Ok(mut calls) = mirror.lock()
        {
            calls.push(text.clone());
        }
        self.calls.push(text);
    }
}

impl Apply for RecordingApply {
    fn wait_session_ready(&mut self, session: &Session) -> Result<(), String> {
        self.call(format!("wait_session_ready {}", session.slot));
        self.readiness_replies.pop_front().unwrap_or(Ok(()))
    }

    fn session_layer_dropin(
        &mut self,
        slot: &str,
        generation: u64,
        layers: &[PathBuf],
    ) -> Result<String, DaemonError> {
        self.prepared_session_layers
            .push((slot.to_owned(), layers.to_vec()));
        Ok(layers_dropin(slot, generation, layers))
    }
    fn remove_session_view(&mut self, _slot: &str) -> Result<(), DaemonError> {
        Ok(())
    }
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
    fn validate_unit(&mut self, unit: &str) -> Result<(), DaemonError> {
        self.call(format!("validate-unit {unit}"));
        Ok(())
    }
    fn try_restart_unit(&mut self, unit: &str) -> Result<(), DaemonError> {
        self.call(format!("try-restart {unit}"));
        // An inactive or failed service remains stopped. The actual manager
        // validates installed units; this recorder has no installed-unit DB.
        Ok(())
    }
    fn restart_desktop(&mut self, session: &Session) -> Result<(), DaemonError> {
        self.call(format!("restart_desktop {}", session.user));
        Ok(())
    }
    fn restart_user_unit(&mut self, user: &str, unit: &str) -> Result<(), DaemonError> {
        self.call(format!("restart_user_unit {user} {unit}"));
        Ok(())
    }
    fn unit_state(&mut self, unit: &str) -> String {
        // A login's scope runs as long as the login does.
        if unit.starts_with("session-") || unit == GREETER_UNIT {
            return "active".into();
        }
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
    fn install_app(
        &mut self,
        session: &Session,
        tapp: &std::path::Path,
    ) -> Result<String, DaemonError> {
        self.call(format!("install_app {}", session.user));
        let name = tapp
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(format!("installed: so.tilo.{name} 0.1.0\n"))
    }
    fn install_cohort_app(
        &mut self,
        session: &Session,
        tapp: &std::path::Path,
    ) -> Result<String, DaemonError> {
        self.call(format!(
            "install_cohort_app {} {}",
            session.user,
            tapp.display()
        ));
        self.cohort_replies
            .pop_front()
            .ok_or_else(|| DaemonError::Apply {
                step: "select cohort app".into(),
                detail: "no recorded installer receipt".into(),
            })
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
    fn hold_awake(&mut self, _lease: &std::path::Path) -> Result<(), DaemonError> {
        self.units.insert(crate::awake::AWAKE_UNIT.to_owned(), true);
        Ok(())
    }
    fn release_awake(&mut self) -> Result<(), DaemonError> {
        self.units
            .insert(crate::awake::AWAKE_UNIT.to_owned(), false);
        Ok(())
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
    fn frame_cpu_trace_is_generation_scoped_and_stand_in_only() {
        let mut apply = RecordingApply::default();
        let mut slot = record("cpu", SlotKind::Headless);
        let plain = generation(1, &[("shell", "h1", Scope::Session, &[])]);
        let mut traced = plain.clone();
        traced.number = 2;
        traced
            .components
            .get_mut("shell")
            .unwrap()
            .info
            .frame_cpu_trace = true;
        let info = &traced.components["shell"].info;
        check_scope(&slot, "shell", info).unwrap();
        for kind in [SlotKind::Vm, SlotKind::Device] {
            assert!(check_scope(&record("other", kind), "shell", info).is_err());
        }
        assert!(check_scope(&slot, "music", info).is_err());
        let layers = [PathBuf::from("/var/lib/tilo-vm/layers/h1")];
        let key = (session_unit("cpu"), LAYERS_DROPIN.into());
        for (g, enabled) in [(&traced, true), (&plain, false), (&traced, true)] {
            activate(
                &mut apply,
                &mut slot,
                g,
                &layers,
                Policy::SessionRestart,
                &Restarts::default(),
                SessionChange::Keep,
            )
            .unwrap();
            assert_eq!(
                apply.dropins[&key].contains("Environment=TILO_VM_FRAME_CPU_TRACE=1"),
                enabled
            );
            assert!(
                !apply.dropins[&(user_unit(60_001), LAYERS_DROPIN.into())]
                    .contains("TILO_VM_FRAME_CPU_TRACE")
            );
        }
    }

    #[test]
    fn component_refresh_does_not_start_inactive_services() {
        let mut apply = RecordingApply::default();
        apply.units.insert("fs.service".into(), true);
        apply.units.insert("tilo-fs-replica.service".into(), false);
        let mut vm = record("vm", SlotKind::Vm);
        activate(
            &mut apply,
            &mut vm,
            &Generation::default(),
            &[PathBuf::from("/layers/fs")],
            Policy::VmRefresh,
            &Restarts {
                units: vec!["fs.service".into(), "tilo-fs-replica.service".into()],
                ..Restarts::default()
            },
            SessionChange::Keep,
        )
        .unwrap();
        assert_eq!(apply.units.get("fs.service"), Some(&true));
        assert_eq!(
            apply.units.get("tilo-fs-replica.service"),
            Some(&false),
            "updating an executable must not enable an optional replica"
        );
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
            assert_eq!(units.units, ["fs.service", "greetd.service"]);

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
            assert!(units.units.is_empty(), "{units:?}");
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
                &Restarts::default(),
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
                &Restarts {
                    units: vec!["fs.service".into()],
                    ..Restarts::default()
                },
                SessionChange::Keep,
            )
            .unwrap();
            assert_eq!(
                apply.calls,
                ["link_sysext 1", "sysext_refresh", "try-restart fs.service"]
            );
        }
    }

    #[test]
    fn vm_greeter_update_preserves_only_a_verified_active_owner_login() {
        for (name, mut apply, defer) in [
            ("owner", RecordingApply::with_login("quickuser"), true),
            ("ordinary-vm", RecordingApply::default(), false),
            (
                "board-session",
                RecordingApply::with_device("quickuser"),
                false,
            ),
            (
                "logged-out",
                {
                    let mut apply = RecordingApply::with_login("quickuser");
                    apply.device = apply.device.take().map(DeviceSession::logged_out);
                    apply
                },
                false,
            ),
            (
                "incomplete-login",
                {
                    let mut apply = RecordingApply::with_login("quickuser");
                    apply
                        .device
                        .as_mut()
                        .unwrap()
                        .login
                        .as_mut()
                        .unwrap()
                        .session = None;
                    apply
                },
                false,
            ),
        ] {
            let mut slot = record("vm", SlotKind::Vm);
            let next = generation(
                1,
                &[("greeter", "g", Scope::Vm, &[GREETER_UNIT, "fs.service"])],
            );
            let (policy, restarts) = policy(&slot, &Generation::default(), &next).unwrap();
            let layers = vec![PathBuf::from("/var/lib/tilo-vm/layers/greeter")];
            let actions = activate(
                &mut apply,
                &mut slot,
                &next,
                &layers,
                policy,
                &restarts,
                SessionChange::Keep,
            )
            .unwrap();
            assert_eq!(apply.sysext, layers, "{name}: verified layers still merge");
            assert!(actions.contains(&Action::SysextRefresh));
            assert!(
                apply.calls.contains(&"try-restart fs.service".to_owned()),
                "{name}: other required services still activate"
            );
            assert_eq!(
                apply.calls.contains(&format!("try-restart {GREETER_UNIT}")),
                !defer,
                "{name}: an active owner's greetd manager must stay running"
            );
            assert_eq!(
                apply
                    .calls
                    .contains(&format!("validate-unit {GREETER_UNIT}")),
                defer,
                "{name}: a deferred unit still passes installed-unit validation"
            );
            assert_eq!(
                actions.iter().any(|action| matches!(action,
                Action::Deferred { reason } if reason.contains(GREETER_UNIT))),
                defer,
                "{name}: report the actual deferred manager activation"
            );
            assert!(
                !apply
                    .calls
                    .iter()
                    .any(|call| call.starts_with("restart_desktop")
                        || call.starts_with("stop ")
                        || call.starts_with("start ")),
                "{name}: this update cannot authenticate, log out or restart the owner"
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
                &Restarts::default(),
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
                &Restarts::default(),
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
        // Phase `the_owners_login_merges_system_wide_and_restarts_its_desktop_in_place`.
        {
            let mut apply = RecordingApply::with_login("quickuser");
            let mut device = record("device", SlotKind::Device);
            device.device = apply.device.clone();
            let g = generation(1, &[("shell", "h1", Scope::Session, &[])]);
            let layers = [
                PathBuf::from("/var/lib/tilo-vm/layers/v1"),
                PathBuf::from("/var/lib/tilo-vm/layers/h1"),
            ];
            let restarts = Restarts {
                user_units: vec!["tilo-ush.service".into()],
                ..Restarts::default()
            };
            let actions = activate(
                &mut apply,
                &mut device,
                &g,
                &layers,
                Policy::SessionRestart,
                &restarts,
                SessionChange::Keep,
            )
            .unwrap();
            assert_eq!(
                apply.calls,
                [
                    "account_uid quickuser",
                    "link_sysext 2",
                    "sysext_refresh",
                    "restart_user_unit quickuser tilo-ush.service",
                    "restart_desktop quickuser",
                ],
                "no drop-ins, no unit stopped: the login stays"
            );
            assert_eq!(apply.sysext, layers);
            assert_eq!(
                actions.last(),
                Some(&Action::RestartDesktop {
                    user: "quickuser".into()
                })
            );
            // Logged out: the layers still merge and wait for the next login.
            let mut logged_out = device.clone();
            logged_out.device = logged_out.device.map(DeviceSession::logged_out);
            let session = logged_out.session().unwrap();
            assert!(session.logged_out());
            assert_eq!(session.unit, GREETER_UNIT);
            let mut apply = RecordingApply::default();
            let actions = activate(
                &mut apply,
                &mut logged_out,
                &g,
                &layers,
                Policy::SessionRestart,
                &restarts,
                SessionChange::Keep,
            )
            .unwrap();
            assert_eq!(
                apply.calls,
                ["account_uid quickuser", "link_sysext 2", "sysext_refresh"]
            );
            assert!(
                matches!(actions.last(), Some(Action::Deferred { reason }) if reason.contains("next login")),
                "{actions:?}"
            );
            // Which user units a swap restarts: the changed session components'.
            let ush = |layer: &str| {
                let mut g = generation(1, &[("ush", layer, Scope::Session, &[])]);
                g.components.get_mut("ush").unwrap().info.user_units =
                    vec!["tilo-ush.service".into()];
                g
            };
            let (p, restarts) = policy(&device, &ush("u1"), &ush("u2")).unwrap();
            assert_eq!(p, Policy::SessionRestart);
            assert_eq!(restarts.user_units, ["tilo-ush.service"]);
            assert!(
                policy(&device, &ush("u1"), &ush("u1"))
                    .unwrap()
                    .1
                    .user_units
                    .is_empty()
            );
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
                &Restarts::default(),
                SessionChange::Ensure,
            )
            .unwrap();
            activate(
                &mut apply,
                &mut slot,
                &g,
                &[],
                Policy::SessionRestart,
                &Restarts::default(),
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
                &Restarts::default(),
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
