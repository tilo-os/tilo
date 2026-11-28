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
//! back before letting it go on. An app's held connection needs no display and outlives any
//! person's session: the device's app host starts it ([`runtime_portal::app_host`]), as an
//! unprivileged account of its own, never as the device's. A daemon of the person's own account
//! orders a window of the seat's shell too while one follows its orders: the shell holds the
//! display and what the person gave the app (the files they chose for it, the devices they
//! allowed it), so a window gets the same grants whoever asked for it. Either way a connection
//! is admitted from the daemon's record, by its ticket, never from what the process or the shell
//! says it is.
//!
//! The shell reports what became of each launch it started ([`LaunchEvent`]): the phases of its
//! start, the processes its windows came from, its memory tier and how it ended. The daemon keeps
//! them on the launch's record. A process the shell holds that no order of this link started (a
//! kept app it takes back after the desktop restarted in place, a window it draws itself) is
//! adopted: decided as any launch, and bound to its ticket.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use runtime_collab::wire::shell::{
    Adopt, HostEntry, HostKind, LaunchAsk, LaunchEnd, LaunchEvent, LaunchKind, LaunchOrder,
    MemoryTier, OrderCommand,
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
    /// Started by the seat's shell as the pid it named, pinned by the pidfd it handed over.
    Seat {
        pid: u32,
        #[expect(dead_code, reason = "held: it pins the process while the launch runs")]
        pinned: Arc<runtime_portal::descriptors::Owned>,
    },
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
    /// What the shell that started it said became of it.
    seen: Seen,
}

/// What the shell that started a launch reported of it ([`LaunchEvent`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Seen {
    /// Each phase of its start, with how long after it was asked for it was reached.
    pub(crate) phases: Vec<(String, u64)>,
    /// The processes its windows came from.
    pub(crate) windows: Vec<u32>,
    /// The memory tier it runs in.
    pub(crate) tier: Option<MemoryTier>,
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
                seen: Seen::default(),
            },
        );
        ticket
    }

    /// The launch the seat's shell started as `pid` for `app` in `workspace` and `window`, while
    /// it runs: what a process the shell adopts again keeps.
    fn seat_launch_of(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        window: Option<u64>,
    ) -> Option<u64> {
        self.decided
            .lock_recover()
            .iter()
            .find(|(_, launch)| {
                matches!(launch.process, Process::Seat { pid: at, .. } if at == pid)
                    && launch.app == app
                    && launch.workspace == workspace
                    && launch.window == window
            })
            .map(|(ticket, _)| *ticket)
    }

    /// Keep `event` on the launch `ticket` a shell started, and answer how it ended when it did.
    ///
    /// # Errors
    /// No launch with that ticket runs.
    fn seen(&self, ticket: u64, event: LaunchEvent) -> Result<Option<(String, LaunchEnd)>, String> {
        let mut decided = self.decided.lock_recover();
        let launch = decided
            .get_mut(&ticket)
            .ok_or_else(|| format!("no launch {ticket} runs"))?;
        // An order the shell never started ends all the same; nothing else is seen of it.
        if matches!(launch.process, Process::Ordered) && !matches!(event, LaunchEvent::Ended(_)) {
            return Err(format!("launch {ticket} has not started"));
        }
        match event {
            LaunchEvent::Phase { phase, after_ms } => launch.seen.phases.push((phase, after_ms)),
            LaunchEvent::Window { pid } => {
                if !launch.seen.windows.contains(&pid) {
                    launch.seen.windows.push(pid);
                }
            }
            LaunchEvent::Tier(tier) => launch.seen.tier = Some(tier),
            LaunchEvent::Ended(end) => {
                let app = launch.app.clone();
                decided.remove(&ticket);
                drop(decided);
                return Ok(Some((app, end)));
            }
        }
        drop(decided);
        Ok(None)
    }

    /// What the shell reported of the launch `ticket`.
    #[cfg(test)]
    pub(crate) fn seen_of(&self, ticket: u64) -> Option<Seen> {
        self.decided
            .lock_recover()
            .get(&ticket)
            .map(|launch| launch.seen.clone())
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
        pid: u32,
        pinned: Arc<runtime_portal::descriptors::Owned>,
    ) -> Result<(), String> {
        let mut decided = self.decided.lock_recover();
        let launch = decided
            .get_mut(&ticket)
            .ok_or_else(|| format!("no launch {ticket} was ordered of this shell"))?;
        if !matches!(launch.process, Process::Ordered) {
            return Err(format!("launch {ticket} has started already"));
        }
        launch.process = Process::Seat { pid, pinned };
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
        // A daemon of the device's own account starts no window: the seat's shell holds the
        // person's display and their grants.
        let ordered = matches!(ask.kind, LaunchKind::Host { .. })
            || (!ask.windowless && self.executor == Executor::Seat);
        let executed = if ordered {
            self.order(ticket, ask)
                .map(|order| self.orders.order(order))
        } else {
            let shows = if ask.windowless {
                super::Shows::Nothing
            } else {
                super::Shows::Window
            };
            self.start_ticketed(
                ticket,
                &ask.app,
                &ask.workspace,
                (shows, ask.open.as_deref(), ask.window),
            )
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
        self.run_with_signed_surface(&app);
        Ok(runtime_collab::wire::shell::PackageCommand {
            manifest: app.manifest_path,
            payload_root: app.payload_root,
            package_hash: original.package_hash,
            open: ask.open.clone(),
            visitor: ask.visitor,
        })
    }

    /// No seat orders a package here: only a Linux daemon runs as the device's own account.
    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        reason = "the same call keeps its receiver on every platform"
    )]
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
        self.tickets
            .seat_started(ticket, pid, Arc::clone(&pinned))?;
        tracing::info!(ticket, pid, "the seat's shell started a launch");
        // The launch is the daemon's on every endpoint it serves, not only by the ticket its
        // shell vouches it with: a connection the process makes to the device itself is
        // admitted as the app it runs, as one the daemon started here is.
        let started = self.tickets.admitted(ticket).and_then(|launch| {
            let shows = if launch.windowless {
                super::Shows::Nothing
            } else {
                super::Shows::Window
            };
            self.launched
                .record_showing(pid, &launch.app, &launch.workspace, shows, None, ticket)
                .then(|| self.launched.recorded_start(pid))
                .flatten()
        });
        if started.is_none() {
            tracing::warn!(
                ticket,
                pid,
                "a launch the seat started is not the app's on the device's own endpoint: its \
                 process start time cannot be read"
            );
        }
        forget_when_ended(
            Arc::clone(&self.tickets),
            Arc::clone(&self.launched),
            ticket,
            pid,
            started,
            pinned,
        );
        Ok(())
    }

    /// Keep `event`, what the seat's shell saw become of the launch `ticket` it started: an end
    /// is said as the end of a launch this daemon started is, and forgets the launch.
    ///
    /// # Errors
    /// No launch with that ticket runs.
    pub(crate) fn launch_event(&self, ticket: u64, event: LaunchEvent) -> Result<(), String> {
        if let LaunchEvent::Phase { phase, after_ms } = &event {
            tracing::debug!(ticket, phase, after_ms, "a launch's start went on");
        }
        if let Some((app, end)) = self.tickets.seen(ticket, event)? {
            said_ended(&app, ticket, &end);
        }
        Ok(())
    }

    /// Decide `adopt`, a launch whose process the seat's shell holds already (`pid`, pinned by
    /// `process`), and bind the process to it: its ticket. A process already bound to a launch of
    /// the same app, workspace and window keeps that launch.
    ///
    /// # Errors
    /// The launch is refused.
    pub(crate) fn adopt(
        &self,
        adopt: &Adopt,
        process: runtime_portal::descriptors::Owned,
    ) -> Result<u64, String> {
        if let Some(ticket) =
            self.tickets
                .seat_launch_of(adopt.pid, &adopt.app, &adopt.workspace, adopt.window)
        {
            return Ok(ticket);
        }
        let ask = LaunchAsk {
            app: adopt.app.clone(),
            workspace: adopt.workspace.clone(),
            kind: adopt.kind.clone(),
            open: None,
            visitor: false,
            window: adopt.window,
            windowless: adopt.windowless,
            member: None,
        };
        let ticket = self.decide(&ask)?;
        if let Err(error) = self.seat_started(ticket, adopt.pid, process) {
            self.tickets.forget(ticket);
            return Err(error);
        }
        Ok(ticket)
    }
}

/// Say how the launch `ticket` of `app` ended, as the shell that started it saw it: an end the
/// person asked for or a clean exit is news to nobody; any other is what a person asking why the
/// app stopped reads first.
fn said_ended(app: &str, ticket: u64, end: &LaunchEnd) {
    let said = end.stderr.join(" | ");
    if end.asked_to_end || end.code == Some(0) {
        tracing::info!(app, ticket, ?end.code, said, "an app ended");
    } else if let Some(memory) = end.memory {
        tracing::warn!(app, ticket, ?memory, said, "an app was ended for memory");
    } else {
        tracing::error!(app, ticket, ?end.code, ?end.signal, said, "an app ended as it ran");
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

/// Forget the launch `ticket` once the process `pinned` pins ends, and the record `started`, the
/// start time the daemon recorded for it, makes of its pid.
#[cfg(target_os = "linux")]
fn forget_when_ended(
    tickets: Arc<Tickets>,
    launched: Arc<super::Launched>,
    ticket: u64,
    pid: u32,
    started: Option<u64>,
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
            if let Some(started) = started {
                launched.forget_at(pid, started);
            }
        });
    if let Err(error) = watched {
        tracing::error!(%error, ticket, "a launch's end cannot be watched");
    }
}

#[cfg(not(target_os = "linux"))]
fn forget_when_ended(
    _: Arc<Tickets>,
    _: Arc<super::Launched>,
    _: u64,
    _: u32,
    _: Option<u64>,
    _: Arc<runtime_portal::descriptors::Owned>,
) {
}

/// The device's launcher as a person's shell reaches it.
pub(crate) struct DeviceLauncher(pub(crate) Arc<super::Apps>);

impl tilo_app_host::shell::ShellLauncher for DeviceLauncher {
    fn launch(&self, ask: LaunchAsk) -> Result<LaunchOrder, String> {
        // A test holding the decisions holds this, as a daemon too busy to decide would.
        #[cfg(feature = "test-support")]
        drop(self.0.held_decisions.blocking_read());
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

    fn event(&self, ticket: u64, event: LaunchEvent) -> Result<(), String> {
        self.0.launch_event(ticket, event)
    }

    fn adopt(
        &self,
        adopt: Adopt,
        process: runtime_portal::descriptors::Owned,
    ) -> Result<u64, String> {
        self.0.adopt(&adopt, process)
    }
}
