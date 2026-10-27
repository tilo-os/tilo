//! Control connections the agent door hands over for programs running in
//! apps (`tilo action` typed in a terminal): what each request may do in the
//! app's workspace scope ([`crate::compositor::workspace_door`]), and the
//! person's consent when it reaches further.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing compositor types"
)]
use super::*;

impl FrameBuilderControlState<'_> {
    /// What deciding a scoped invocation of `(provider, id)` needs: the room
    /// its resource belongs to and its contract, read through the same
    /// catalogue and gate an invocation is resolved through.
    fn row_facts(&self, provider: &str, id: &str) -> Result<RowFacts, test_control::ActionRefusal> {
        let (doc, root) = self.doc.ok_or_else(|| {
            test_control::ActionRefusal::unknown("control state has no live document")
        })?;
        let chrome_handle = self.fb.chrome_handle();
        let resource = ResourceRef::new(provider, id);
        let catalog = with_action_gate(&chrome_handle, |chrome, gate| {
            let first_use_gate = crate::compositor::admin_control::FirstUseVisibility {
                base: gate,
                first_use: self.fb.host().onboarding_active(),
            };
            self.fb.host().action_catalog_for_resource(
                &resource,
                chrome,
                doc,
                root,
                &first_use_gate,
            )
        })
        .map_err(test_control::ActionRefusal::unknown)?;
        let selected = catalog.select(&resource).ok_or_else(|| {
            test_control::ActionRefusal::unknown(format!(
                "no visible action addressed by ({provider}, {id}) in the current catalogue"
            ))
        })?;
        Ok(RowFacts {
            room: catalog
                .hierarchy_of(&resource)
                .map(|hierarchy| hierarchy.room.clone()),
            contract: selected.contract().cloned(),
            title: selected.title().to_owned(),
        })
    }
}

/// What deciding a scoped invocation needs to know about its action.
struct RowFacts {
    /// The room of the workspace its resource belongs to, if any.
    room: Option<String>,
    contract: Option<tilo_actions::ActionContract>,
    title: String,
}

/// What a scoped connection's next request gets.
pub(super) enum Gate {
    /// Unscoped, or permitted in its scope: served as usual.
    Serve,
    /// Waiting on the person's answer; nothing after it is read.
    Held,
    Refuse(Box<test_control::Response>),
}

/// Decide `line` for `conn`'s app scope, holding it for the person's answer
/// when it reaches beyond what they already allowed.
pub(super) fn gate(
    conn: &mut TestControlConn,
    line: &str,
    frame_builder: &FrameBuilder,
    ctx: &TestControlCtx,
    locked: bool,
) -> Gate {
    use crate::compositor::workspace_door::Decision;
    let confirmed = conn
        .confirmed
        .take()
        .is_some_and(|confirmed| *confirmed == *line);
    let Some(scope) = conn.scope.as_ref() else {
        return Gate::Serve;
    };
    match scoped_decision(scope, line, frame_builder, ctx, locked, confirmed) {
        Decision::Ask(ask) => {
            conn.held_for_consent = Some(hold_for_consent(frame_builder, line, ask));
            Gate::Held
        }
        Decision::Refuse(refusal) => Gate::Refuse(Box::new(refusal)),
        Decision::Serve => Gate::Serve,
    }
}

/// Narrow a listing answered on a scoped connection to what its app may do.
pub(super) fn filter_answer(
    conn: &TestControlConn,
    resp: &mut test_control::Response,
    frame_builder: &FrameBuilder,
) {
    if let Some(scope) = &conn.scope
        && let test_control::Response::Answer(answer) = resp
    {
        let allowed = desktop_allowed(frame_builder, scope);
        crate::compositor::workspace_door::filter_listing(answer, scope, allowed);
    }
}

/// What becomes of a scoped connection's request `line`
/// ([`crate::compositor::workspace_door::Decision`]): refused while the session is
/// locked, refused unless it lists or invokes actions, and an invocation
/// served when it works in the app's own workspace, held for the person
/// when it reaches further. `confirmed` is a sensitive request the person
/// just allowed, once. A line that does not parse is answered as any other
/// connection's.
pub(super) fn scoped_decision(
    scope: &crate::compositor::workspace_door::AppScope,
    line: &str,
    frame_builder: &FrameBuilder,
    ctx: &TestControlCtx,
    locked: bool,
    confirmed: bool,
) -> crate::compositor::workspace_door::Decision {
    use crate::compositor::workspace_door::{self as door, Decision, Row};
    let Ok(request) = parse_request(line) else {
        return Decision::Serve;
    };
    if locked {
        return Decision::Refuse(test_control::Response::Err {
            code: "session_locked".into(),
            message: crate::shell::i18n::t("desktop-control-locked"),
        });
    }
    if !door::permitted(&request) {
        return Decision::Refuse(door::not_permitted());
    }
    let test_control::Command::ActionsInvoke { provider, id, .. } = &request.command else {
        return Decision::Serve;
    };
    if door::hidden(provider) {
        return Decision::Refuse(test_control::Response::Err {
            code: "unknown_action".into(),
            message: format!(
                "no visible action addressed by ({provider}, {id}) in the current catalogue"
            ),
        });
    }
    let facts = FrameBuilderControlState {
        fb: frame_builder,
        doc: Some((&ctx.doc, &ctx.root_obj)),
        shares: None,
        lock: None,
        seat: None,
        resolved: None,
    }
    .row_facts(provider, id);
    let facts = match facts {
        Ok(facts) => facts,
        Err(refusal) => {
            return Decision::Refuse(test_control::Response::Err {
                code: refusal.code.to_string(),
                message: refusal.message,
            });
        }
    };
    let active_room = frame_builder
        .chrome()
        .tree()
        .desktop
        .active_workspace()
        .map(|workspace| workspace.room().to_string());
    let row = Row {
        provider,
        room: facts.room.as_deref(),
        contract: facts.contract.as_ref(),
    };
    door::decide_invoke(
        row,
        &facts.title,
        scope,
        active_room.as_deref(),
        desktop_allowed(frame_builder, scope),
        confirmed,
    )
}

/// Whether the person let `scope`'s tab, or its app in its workspace,
/// control the desktop.
pub(super) fn desktop_allowed(
    frame_builder: &FrameBuilder,
    scope: &crate::compositor::workspace_door::AppScope,
) -> bool {
    let chrome = frame_builder.chrome();
    chrome.desktop_control.tab_allowed(scope.holder)
        || crate::shell::desktop_control::always_allowed(
            &chrome.tree(),
            &scope.room,
            &scope.package,
        )
}

/// Whether the client on `stream` closed its end: a peek that would read
/// nothing more, ever.
fn peer_hung_up(stream: &UnixStream) -> bool {
    let mut probe = 0_u8;
    // SAFETY: a one-byte peek into a live local through a descriptor the
    // stream owns; nothing is consumed.
    let read = unsafe {
        libc::recv(
            std::os::fd::AsRawFd::as_raw_fd(stream),
            (&raw mut probe).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    read == 0
}

/// A scoped request held until the person answers whether its app may
/// reach beyond its workspace.
pub(super) struct HeldForConsent {
    pub(super) line: zeroize::Zeroizing<String>,
    pub(super) ask: crate::shell::desktop_control::DesktopControlAsk,
    pub(super) answer: tokio::sync::oneshot::Receiver<crate::portal::PromptOutcome>,
}

/// Ask the person `ask` in trusted chrome, holding `line` until they answer.
pub(super) fn hold_for_consent(
    frame_builder: &FrameBuilder,
    line: &str,
    ask: crate::shell::desktop_control::DesktopControlAsk,
) -> HeldForConsent {
    let (reply, answer) = tokio::sync::oneshot::channel();
    let chrome_handle = frame_builder.chrome_handle();
    let mut chrome = chrome_handle.borrow_mut();
    chrome
        .pending_grants
        .push_back(crate::shell::chrome_state::PendingPrompt::desktop_control(
            ask.clone(),
            reply,
        ));
    chrome.bump();
    HeldForConsent {
        line: zeroize::Zeroizing::new(line.to_owned()),
        ask,
        answer,
    }
}

/// Apply the person's answer to a held request: record what they allowed
/// and give back the request to run, or the refusal to answer it with.
fn settle_consent(
    frame_builder: &FrameBuilder,
    held: HeldForConsent,
    outcome: crate::portal::PromptOutcome,
) -> Result<(zeroize::Zeroizing<String>, bool), Box<test_control::Response>> {
    use crate::portal::PromptOutcome;
    use crate::shell::desktop_control as grants;
    if let Some(refusal) = crate::compositor::workspace_door::refusal_for(outcome) {
        return Err(Box::new(refusal));
    }
    let ask = &held.ask;
    match outcome {
        PromptOutcome::AllowSession => {
            let label = grants::tab_label(&frame_builder.chrome().tree(), &ask.tabs)
                .unwrap_or_else(|| ask.app_label.clone());
            frame_builder
                .chrome_handle()
                .borrow_mut()
                .desktop_control
                .allow_tab(grants::TabGrant {
                    holder: ask.holder,
                    package: ask.package.clone(),
                    room: ask.room.clone(),
                    tabs: ask.tabs.clone(),
                    label,
                });
        }
        PromptOutcome::AllowAlways => {
            let tree = frame_builder.chrome().tree_handle();
            grants::allow_always(&mut tree.write(), &ask.room, &ask.package);
        }
        PromptOutcome::AllowOnce | PromptOutcome::Deny => {}
    }
    frame_builder.chrome_handle().borrow_mut().bump();
    // A sensitive action runs once on "Allow"; nothing more is granted.
    Ok((held.line, ask.sensitive))
}

thread_local! {
    /// Set when a watched tab's process ended, until the loop notices.
    static TAB_ENDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The processes of the live "this tab" grants, watched so a grant ends the
/// moment its tab closes and the bars stop saying so.
#[derive(Default)]
pub(super) struct TabGrantWatch {
    watched: HashMap<crate::shell::desktop_control::Holder, calloop::RegistrationToken>,
    generation: Option<u64>,
    /// The last stop request from Settings seen, by its time.
    stop_seen: Option<u64>,
}

impl TabGrantWatch {
    /// Forget the grants of tabs that closed, and watch each new grant's
    /// tab. Cheap when nothing changed: no process is read.
    pub(super) fn sync(
        &mut self,
        frame_builder: &FrameBuilder,
        handle: &calloop::LoopHandle<'static, State>,
    ) {
        let chrome_handle = frame_builder.chrome_handle();
        if TAB_ENDED.with(|ended| ended.replace(false)) {
            let mut chrome = chrome_handle.borrow_mut();
            if chrome.desktop_control.forget_closed() {
                chrome.bump();
            }
        }
        let chrome = chrome_handle.borrow();
        let generation = chrome.desktop_control.generation();
        if self.generation == Some(generation) {
            return;
        }
        self.generation = Some(generation);
        publish(&chrome);
        let live: HashSet<_> = chrome
            .desktop_control
            .tabs()
            .map(|grant| grant.holder)
            .collect();
        self.watched.retain(|holder, token| {
            let keep = live.contains(holder);
            if !keep {
                handle.remove(*token);
            }
            keep
        });
        for holder in live {
            if let std::collections::hash_map::Entry::Vacant(slot) = self.watched.entry(holder)
                && let Some(token) = watch_exit(holder, handle)
            {
                slot.insert(token);
            }
        }
    }
}

impl TabGrantWatch {
    /// Settings asked to stop a "this tab" grant: stop it, once.
    pub(super) fn stop_requested(&mut self, frame_builder: &FrameBuilder) {
        use settings_store::desktop_control::StopRequest;
        let Some(request) = StopRequest::load() else {
            return;
        };
        if self.stop_seen == Some(request.requested_unix_ms) {
            return;
        }
        self.stop_seen = Some(request.requested_unix_ms);
        if !request.is_fresh(tilo_clock::unix_now_ms()) {
            return;
        }
        let chrome_handle = frame_builder.chrome_handle();
        let mut chrome = chrome_handle.borrow_mut();
        let holder = chrome
            .desktop_control
            .tabs()
            .map(|grant| grant.holder)
            .find(|holder| grant_id(*holder) == request.grant);
        if let Some(holder) = holder
            && chrome.desktop_control.revoke_tab(holder)
        {
            chrome.bump();
        }
    }
}

/// How Settings names a "this tab" grant: its tab's process.
fn grant_id(holder: crate::shell::desktop_control::Holder) -> String {
    format!("{}:{}", holder.pid, holder.start)
}

/// Publish the live "this tab" grants for Settings to list.
fn publish(chrome: &crate::shell::chrome_state::ChromeState) {
    use settings_store::desktop_control::{DesktopControlSnapshot, TabGrantEntry};
    let Ok((publisher_pid, publisher_start_ticks)) = settings_store::hardware::publisher_identity()
    else {
        return;
    };
    let tree = chrome.tree();
    let tabs = chrome
        .desktop_control
        .tabs()
        .map(|grant| TabGrantEntry {
            id: grant_id(grant.holder),
            app: grant.package.clone(),
            app_label: crate::shell::launcher::display_name_for_app_id_without_waiting(
                &grant.package,
            )
            .unwrap_or_else(|| grant.package.clone()),
            tab: grant.label.clone(),
            workspace: tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room().to_string() == grant.room)
                .map(|workspace| workspace.name.clone())
                .unwrap_or_default(),
        })
        .collect();
    let snapshot = DesktopControlSnapshot {
        publisher_pid,
        publisher_start_ticks,
        tabs,
    };
    if let Err(error) = snapshot.save() {
        tracing::debug!(%error, "desktop control: could not publish the live grants");
    }
}

/// Wake the loop once `holder` ends, through a pidfd.
fn watch_exit(
    holder: crate::shell::desktop_control::Holder,
    handle: &calloop::LoopHandle<'static, State>,
) -> Option<calloop::RegistrationToken> {
    let pid = libc::pid_t::try_from(holder.pid).ok()?;
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor
    // or -1; nothing is borrowed.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    let raw = std::os::fd::RawFd::try_from(raw)
        .ok()
        .filter(|fd| *fd >= 0)?;
    // SAFETY: `raw` was just returned by pidfd_open and is owned by nothing
    // else.
    let pidfd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    // The pid may have been reused before the descriptor was opened: then
    // the tab has already closed.
    if !holder.is_live() {
        TAB_ENDED.with(|ended| ended.set(true));
        return None;
    }
    handle
        .insert_source(
            Generic::new(pidfd, Interest::READ, Mode::Level),
            |_, _, _| {
                TAB_ENDED.with(|ended| ended.set(true));
                Ok(PostAction::Remove)
            },
        )
        .ok()
}

/// Where a connection's request held for the person stands.
pub(super) enum Held {
    /// None is held, or the one held may run now.
    Nothing,
    Waiting,
    /// The person refused it: answer with this.
    Refused(Box<test_control::Response>),
}

/// Settle `conn`'s request held for the person, once they answered: record
/// what they allowed and queue the request to run. A client gone before
/// the person answered closes the connection, withdrawing the prompt with
/// it (dropping the hold closes the reply).
pub(super) fn settle_held(conn: &mut TestControlConn, frame_builder: &FrameBuilder) -> Held {
    use tokio::sync::oneshot::error::TryRecvError;
    let Some(held) = conn.held_for_consent.as_mut() else {
        return Held::Nothing;
    };
    let outcome = match held.answer.try_recv() {
        Err(TryRecvError::Empty) if peer_hung_up(conn.reader.get_ref()) => {
            conn.held_for_consent = None;
            conn.closed = true;
            return Held::Waiting;
        }
        Err(TryRecvError::Empty) => return Held::Waiting,
        Ok(outcome) => outcome,
        // A prompt withdrawn unanswered is a refusal.
        Err(TryRecvError::Closed) => crate::portal::PromptOutcome::Deny,
    };
    let Some(held) = conn.held_for_consent.take() else {
        return Held::Nothing;
    };
    match settle_consent(frame_builder, held, outcome) {
        Ok((line, sensitive)) => {
            if sensitive {
                conn.confirmed = Some(line.clone());
            }
            conn.replay = Some(line);
            Held::Nothing
        }
        Err(refusal) => Held::Refused(refusal),
    }
}

impl TestControlConn {
    /// A connection the agent door handed over for a program running in an
    /// app ([`crate::compositor::workspace_door`]), with the request it
    /// opened with and the scope it is served with.
    pub(super) fn scoped(
        stream: UnixStream,
        first_line: String,
        scope: crate::compositor::workspace_door::AppScope,
    ) -> Option<Self> {
        let mut conn = Self::new(stream, false)?;
        conn.scope = Some(scope);
        conn.replay = Some(zeroize::Zeroizing::new(first_line));
        Some(conn)
    }
}

/// Take on the connections the agent door handed over, up to
/// [`MAX_SCOPED_CONTROL_CONNS`] at once; more are dropped as they arrive.
/// Each is served with its app's scope once the tabs it asked from are
/// found; a Terminal tab no longer shown is told it is served nothing.
pub(super) fn accept(
    handed_over: &std::sync::mpsc::Receiver<crate::compositor::agent_door::ControlHandoff>,
    conns: &mut Vec<TestControlConn>,
    handle: &calloop::LoopHandle<'static, State>,
    frame_builder: &FrameBuilder,
    state: &State,
) {
    while let Ok(handoff) = handed_over.try_recv() {
        let scoped = conns.iter().filter(|conn| conn.scope.is_some()).count();
        if scoped >= MAX_SCOPED_CONTROL_CONNS {
            tracing::info!("agent door: too many programs in apps connected at once");
            continue;
        }
        let Some(scope) = resolve(handoff.attribution, frame_builder, state) else {
            crate::compositor::agent_door::refuse_control(handoff.stream);
            continue;
        };
        if let Some(mut conn) = TestControlConn::scoped(handoff.stream, handoff.first_line, scope) {
            conn.watch(handle);
            conns.push(conn);
        }
    }
}

/// Find the tabs `attribution` asked from: the Terminal tab this desktop
/// shows whose Terminal reported the session, or the windows of the
/// launch, by the launch custody of their Wayland clients.
pub(super) fn resolve(
    attribution: crate::compositor::workspace_door::Attribution,
    frame_builder: &FrameBuilder,
    state: &State,
) -> Option<crate::compositor::workspace_door::AppScope> {
    let chrome = frame_builder.chrome();
    let windows = chrome
        .hosted_inputs
        .in_process()
        .map(|apps| apps.borrow().hosted_windows())
        .unwrap_or_default();
    let tree = chrome.tree();
    let workspace_tabs = || {
        tree.desktop.workspaces.iter().flat_map(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .map(move |tab| (workspace, tab))
        })
    };
    attribution.resolve(
        |session| {
            windows
                .iter()
                .find(|(_, window)| window.session().as_deref() == Some(session))
                .map(|(tab, _)| *tab)
        },
        |custody| {
            workspace_tabs()
                .filter_map(|(_, tab)| match tab.tile {
                    crate::shell::state::TileContent::Foreign { surface_id, .. }
                        if state.surface_launch_custody(surface_id) == Some(custody) =>
                    {
                        Some(tab.id)
                    }
                    _ => None,
                })
                .collect()
        },
        |wanted| {
            workspace_tabs()
                .find(|(_, tab)| tab.id == wanted)
                .map(|(workspace, _)| workspace.room().to_string())
        },
    )
}
