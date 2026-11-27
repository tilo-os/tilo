//! Who starts a launch's process once the daemon decided it, and the record each launch is
//! admitted by.
//!
//! The daemon decides every launch: before any process exists it checks the app is installed,
//! the workspace is served here and the launch's kind may start, and records the launch under a
//! ticket ([`Tickets`]). Then the launch is executed. Where the daemon runs as the person's own
//! account (macOS, a developer's machine, a nested session) it starts the process itself. On Tilo
//! OS it runs as the device's own account, which cannot start a process as the person, reach
//! their display or see their processes: there a launch that shows a window is ordered of the
//! seat's shell ([`SeatOrders`]), which starts it as the person, held, and reports the process
//! back before letting it go on. A launch that shows nothing needs no display and still runs
//! here. Either way a connection is admitted from the daemon's record, by its ticket, never from
//! what the process or the shell says it is.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use runtime_collab::wire::shell::{
    HostEntry, HostKind, LaunchAsk, LaunchKind, LaunchOrder, OrderCommand,
};
use tilo_app_host::shell::AdmittedLaunch;
use tilo_lock::MutexRecover as _;

/// Who starts a decided package launch's process. A host app's is always started by the seat's
/// shell, which holds the person's display and keeps their windows across a restart; this daemon
/// resolves no desktop entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Executor {
    /// This daemon, which runs as the person's own account.
    Local,
    /// The seat's shell, for every launch that shows a window: this daemon runs as the device's
    /// own account.
    Seat,
}

impl Executor {
    /// The executor of the daemon serving the profile at `profile`: the seat's shell for the
    /// computer's own daemon on a computer whose daemon is the system's, this daemon otherwise.
    pub(crate) fn of_daemon(profile: &std::path::Path) -> Self {
        if tilo_machine::serves_the_computer(tilo_machine::system_daemon(), Some(profile)) {
            Self::Seat
        } else {
            Self::Local
        }
    }
}

/// The launches ordered of the seat's shell that it has not taken yet.
#[derive(Default)]
pub(crate) struct SeatOrders {
    waiting: Mutex<VecDeque<LaunchOrder>>,
    ordered: tokio::sync::Notify,
}

impl SeatOrders {
    /// Order `order` of the seat's shell.
    fn order(&self, order: LaunchOrder) {
        self.waiting.lock_recover().push_back(order);
        self.ordered.notify_one();
    }

    /// The orders waiting, once there is one.
    pub(crate) async fn take(self: Arc<Self>) -> Vec<LaunchOrder> {
        loop {
            let ordered = self.ordered.notified();
            let waiting: Vec<LaunchOrder> = self.waiting.lock_recover().drain(..).collect();
            if !waiting.is_empty() {
                return waiting;
            }
            ordered.await;
        }
    }
}

/// What a launch starts, as the daemon recorded it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Package,
    Host(HostKind),
}

/// Where a recorded launch's process stands.
enum Process {
    /// Decided, its process not started yet.
    Ordered,
    /// Started by this daemon.
    Here,
    /// Started by the seat's shell, pinned by the pidfd the shell handed over.
    Seat(
        #[expect(dead_code, reason = "held: it pins the process while the launch runs")]
        Arc<runtime_portal::descriptors::Owned>,
    ),
}

/// One launch the daemon decided.
struct Ticketed {
    app: String,
    workspace: String,
    window: Option<u64>,
    windowless: bool,
    member: Option<String>,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read by what a launch is asked about in tests")
    )]
    kind: Kind,
    process: Process,
}

/// The launches this daemon decided, by ticket.
#[derive(Default)]
pub(crate) struct Tickets {
    next: std::sync::atomic::AtomicU64,
    decided: Mutex<HashMap<u64, Ticketed>>,
}

impl Tickets {
    /// Record a launch of `app` in `workspace`, decided now, and answer its ticket.
    pub(crate) fn decide(
        &self,
        app: &str,
        workspace: &str,
        (window, windowless, member): (Option<u64>, bool, Option<String>),
        kind: Kind,
    ) -> u64 {
        let ticket = self
            .next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1);
        self.decided.lock_recover().insert(
            ticket,
            Ticketed {
                app: app.to_owned(),
                workspace: workspace.to_owned(),
                window,
                windowless,
                member,
                kind,
                process: Process::Ordered,
            },
        );
        ticket
    }

    /// The launch `ticket` runs here, as the process [`super::Launched`] recorded for it.
    pub(crate) fn here(&self, ticket: u64) {
        if let Some(decided) = self.decided.lock_recover().get_mut(&ticket) {
            decided.process = Process::Here;
        }
    }

    /// The seat's shell started the ordered launch `ticket` as `pid`, pinned by `pinned`.
    ///
    /// # Errors
    /// No ordered launch with that ticket waits for its process.
    fn seat_started(
        &self,
        ticket: u64,
        pinned: Arc<runtime_portal::descriptors::Owned>,
    ) -> Result<(), String> {
        let mut decided = self.decided.lock_recover();
        let launch = decided
            .get_mut(&ticket)
            .ok_or_else(|| format!("no launch {ticket} was ordered of this shell"))?;
        if !matches!(launch.process, Process::Ordered) {
            return Err(format!("launch {ticket} has started already"));
        }
        launch.process = Process::Seat(pinned);
        drop(decided);
        Ok(())
    }

    /// Forget the launch `ticket`: its process ended, or never started.
    pub(crate) fn forget(&self, ticket: u64) {
        self.decided.lock_recover().remove(&ticket);
    }

    /// What the launch `ticket` was recorded as, once its process runs.
    pub(crate) fn admitted(&self, ticket: u64) -> Option<AdmittedLaunch> {
        let decided = self.decided.lock_recover();
        let launch = decided.get(&ticket)?;
        if matches!(launch.process, Process::Ordered) {
            return None;
        }
        let admitted = AdmittedLaunch {
            app: launch.app.clone(),
            workspace: launch.workspace.clone(),
            window: launch.window,
            windowless: launch.windowless,
            member: launch.member.clone(),
        };
        drop(decided);
        Some(admitted)
    }

    /// What the launch `ticket` starts.
    #[cfg(test)]
    pub(crate) fn kind(&self, ticket: u64) -> Option<Kind> {
        self.decided
            .lock_recover()
            .get(&ticket)
            .map(|launch| launch.kind)
    }
}

impl super::Apps {
    /// Decide `ask`, the person's launch: refused before any process exists unless the app is
    /// installed, the workspace is served here and the launch's kind may start; recorded
    /// otherwise, and its ticket answered.
    ///
    /// # Errors
    /// Why the launch is refused.
    pub(crate) fn decide(&self, ask: &LaunchAsk) -> Result<u64, String> {
        let kind = match &ask.kind {
            LaunchKind::Package => {
                if !self.launches_into(&ask.workspace) {
                    return Err(format!(
                        "workspace {} is not served by this runtime",
                        ask.workspace
                    ));
                }
                if ask.windowless {
                    self.installed_app(&ask.app)?;
                } else {
                    self.launchable(&ask.app)?;
                }
                Kind::Package
            }
            LaunchKind::Host { kind, entry } => {
                host_may_launch(self, ask, entry)?;
                Kind::Host(*kind)
            }
        };
        Ok(self.tickets.decide(
            &ask.app,
            &ask.workspace,
            (ask.window, ask.windowless, ask.member.clone()),
            kind,
        ))
    }

    /// Decide `ask`, the person's own launch, and answer the order their shell starts it by: the
    /// shell runs as the person and holds their display, so it starts what it asks for itself in
    /// either shape of daemon, and only what the daemon decided.
    ///
    /// # Errors
    /// Why the launch is refused.
    pub(crate) fn order_for_shell(&self, ask: &LaunchAsk) -> Result<LaunchOrder, String> {
        let ticket = self.decide(ask)?;
        self.order(ticket, ask)
            .inspect_err(|_| self.tickets.forget(ticket))
    }

    /// Decide `ask`, a launch the daemon makes for another caller (an app's, an agent's), and
    /// have its process started: here, or by the seat's shell for a host app, and for a launch
    /// that shows a window on a daemon that runs as the device's own account. The launch's
    /// ticket.
    ///
    /// # Errors
    /// The launch is refused, or its process could not be started here.
    pub(crate) fn launch_asked(&self, ask: &LaunchAsk) -> Result<u64, String> {
        let ticket = self.decide(ask)?;
        let ordered = matches!(ask.kind, LaunchKind::Host { .. })
            || (self.executor == Executor::Seat && !ask.windowless);
        let executed = if ordered {
            self.order(ticket, ask)
                .map(|order| self.orders.order(order))
        } else {
            let shows = if ask.windowless {
                super::Shows::Nothing
            } else {
                super::Shows::Window
            };
            self.start_ticketed(ticket, &ask.app, &ask.workspace, shows, ask.open.as_deref())
        };
        if let Err(error) = executed {
            self.tickets.forget(ticket);
            return Err(error);
        }
        Ok(ticket)
    }

    /// The order the seat's shell starts the decided launch `ticket` by: a package's command
    /// built here from its signed manifest, a host app's entry for the shell to resolve.
    fn order(&self, ticket: u64, ask: &LaunchAsk) -> Result<LaunchOrder, String> {
        let (command, title) = match &ask.kind {
            LaunchKind::Package => (
                OrderCommand::Package(self.package_command(ask)?),
                self.display_name(&ask.app),
            ),
            LaunchKind::Host { kind, entry } => (
                OrderCommand::Host {
                    kind: *kind,
                    entry: entry.clone(),
                },
                ask.app.clone(),
            ),
        };
        Ok(LaunchOrder {
            ticket,
            app: ask.app.clone(),
            title,
            workspace: ask.workspace.clone(),
            window: ask.window,
            windowless: ask.windowless,
            command,
        })
    }

    /// The launcher's command for the package `ask` names, from the package this device installed
    /// and its signed manifest, with the grants the person gave it in the workspace.
    #[cfg(target_os = "linux")]
    fn package_command(
        &self,
        ask: &LaunchAsk,
    ) -> Result<runtime_collab::wire::shell::PackageCommand, String> {
        let app = self.launchable(&ask.app)?;
        let original = self.original_launch(&app)?;
        let grants = serde_json::to_string(&self.app_grants(&app, &ask.workspace))
            .map_err(|error| format!("refusing to launch {}: its grants: {error}", ask.app))?;
        self.run_with_signed_surface(&app);
        Ok(runtime_collab::wire::shell::PackageCommand {
            manifest: app.manifest_path,
            payload_root: app.payload_root,
            package_hash: original.package_hash,
            grants,
            open: ask.open.clone(),
            visitor: ask.visitor,
        })
    }

    /// No seat orders a package here: only a Linux daemon runs as the device's own account.
    #[cfg(not(target_os = "linux"))]
    fn package_command(
        &self,
        ask: &LaunchAsk,
    ) -> Result<runtime_collab::wire::shell::PackageCommand, String> {
        Err(format!(
            "refusing to launch {}: no seat starts packages on this platform",
            ask.app
        ))
    }

    /// The seat's shell started the ordered launch `ticket` as `pid`, pinned by `process`: bind
    /// it to the launch, and forget the launch once the process ends.
    ///
    /// # Errors
    /// No ordered launch with that ticket waits for its process.
    pub(crate) fn seat_started(
        &self,
        ticket: u64,
        pid: u32,
        process: runtime_portal::descriptors::Owned,
    ) -> Result<(), String> {
        let pinned = Arc::new(process);
        self.tickets.seat_started(ticket, Arc::clone(&pinned))?;
        tracing::info!(ticket, pid, "the seat's shell started a launch");
        forget_when_ended(Arc::clone(&self.tickets), ticket, pinned);
        Ok(())
    }
}

/// Whether the host app `entry` may start as `ask` asks: only for the person themselves (an
/// agent starts no unconfined program), never for a Tilo package (which starts only in its
/// sandbox, from its signed manifest), and only into no workspace or one served here.
fn host_may_launch(apps: &super::Apps, ask: &LaunchAsk, entry: &HostEntry) -> Result<(), String> {
    if ask.member.is_some() {
        return Err(format!(
            "{} runs unconfined as the person, so only they start it",
            ask.app
        ));
    }
    if ask.app != entry.id() {
        return Err(format!(
            "a launch of {} names the host entry {}",
            ask.app,
            entry.id()
        ));
    }
    if apps.installed_app(&ask.app).is_ok() {
        return Err(format!(
            "{} is a Tilo package, which starts only in its sandbox",
            ask.app
        ));
    }
    if let HostEntry::Program { path } = entry
        && !path.is_absolute()
    {
        return Err(format!("{} is no program's path", path.display()));
    }
    if !ask.workspace.is_empty() && !apps.launches_into(&ask.workspace) {
        return Err(format!(
            "workspace {} is not served by this runtime",
            ask.workspace
        ));
    }
    Ok(())
}

/// Forget the launch `ticket` once the process `pinned` pins ends.
#[cfg(target_os = "linux")]
fn forget_when_ended(
    tickets: Arc<Tickets>,
    ticket: u64,
    pinned: Arc<runtime_portal::descriptors::Owned>,
) {
    let watched = std::thread::Builder::new()
        .name("tilo-seat-launch".into())
        .spawn(move || {
            use std::os::fd::AsFd as _;
            let fd = pinned.as_fd();
            let mut ended = [rustix::event::PollFd::new(
                &fd,
                rustix::event::PollFlags::IN,
            )];
            // A pidfd reads once its process ended; an interrupted wait looks again.
            while !matches!(rustix::event::poll(&mut ended, None), Ok(1..)) {}
            tickets.forget(ticket);
        });
    if let Err(error) = watched {
        tracing::error!(%error, ticket, "a launch's end cannot be watched");
    }
}

#[cfg(not(target_os = "linux"))]
fn forget_when_ended(_: Arc<Tickets>, _: u64, _: Arc<runtime_portal::descriptors::Owned>) {}

/// The device's launcher as a person's shell reaches it.
pub(crate) struct DeviceLauncher(pub(crate) Arc<super::Apps>);

impl tilo_app_host::shell::ShellLauncher for DeviceLauncher {
    fn launch(&self, ask: LaunchAsk) -> Result<LaunchOrder, String> {
        self.0.order_for_shell(&ask)
    }

    fn orders(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<LaunchOrder>> + Send>> {
        Box::pin(Arc::clone(&self.0.orders).take())
    }

    fn started(
        &self,
        ticket: u64,
        pid: u32,
        process: runtime_portal::descriptors::Owned,
    ) -> Result<(), String> {
        self.0.seat_started(ticket, pid, process)
    }

    fn admitted(&self, ticket: u64) -> Option<AdmittedLaunch> {
        self.0.tickets.admitted(ticket)
    }
}
