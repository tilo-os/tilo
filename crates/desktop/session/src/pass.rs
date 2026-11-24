//! The loop's pass: the turn it takes, when it may sleep, and what the login's first frame closes.
//! [`Session::run`] is the loop and [`Session::step`] one turn of it: wait for clients, devices and
//! the session's own due work, then carry out what the session asked and draw. What this file holds is
//! what a turn answers apart from drawing — the shortest wait its own clocks ask for, the session's
//! share of the frame's timings, the runs and services it fed, everything asked since the last look
//! ([`Session::settle`]), and the end of the startup measure.

use std::time::{Duration, Instant};

use crate::window_share::peer::PeerViews as _;
use crate::{Session, quick_settings};
use tilo_compositor::Compositor;

use crate::Error;

/// How long the session's own bookkeeping may wait for a turn that draws nothing: a session that keeps
/// drawing is read this often all the same.
const KEEP_INTERVAL: Duration = Duration::from_millis(100);

/// The content secret the members of the workspace `workspace` hold for the record of `app`, as
/// this device's engine keeps it: `None` until the keys reach it, when each part is sealed under a
/// fresh key instead.
fn secret_of(
    engine: &crate::engine::Engine,
    workspace: tilo_desktop_model::WorkspaceId,
    app: &str,
) -> Option<tilo_seal::SymmetricKey> {
    match engine.seal_keys(workspace) {
        Ok(crate::engine::SealKeys::Here(keys)) => keys.document_content_secret().ok(),
        Ok(crate::engine::SealKeys::Waiting | crate::engine::SealKeys::NoSecretStore) => None,
        Err(error) => {
            // A session that takes no part in the peer network keeps no channel of its
            // own: what it stores is sealed under a fresh key until it does.
            tilo_failure::ignored!(
                "a session with no channel service seals each part under a key of its own",
                tracing::debug!(%error, %app, "no content secret for a record the session keeps")
            );
            None
        }
    }
}

impl Session {
    /// How long this pass may sleep: no longer than `timeout` , and no longer than the next thing
    /// that is due on a clock of its own — a held direction repeating, or a profile the owner asked
    /// for ending.
    pub(super) fn wait(&self, timeout: Option<Duration>) -> Option<Duration> {
        let now = Instant::now();
        // A held direction repeats on its own clock, and a controller with more waiting than one
        // pass read is read again at once.
        let controller = self
            .controllers
            .as_ref()
            .and_then(|controllers| controllers.next_deadline(now))
            .map(|due| due.saturating_duration_since(now));
        // A package's trigger at the instant it names: the pass that runs it is due then,
        // whether or not a window of its app is open.
        let service = self.shared.services.borrow().wakes_in();
        let controller = match (controller, service) {
            (Some(controller), Some(service)) => Some(controller.min(service)),
            (controller, service) => controller.or(service),
        };
        let timeout = match (timeout, controller) {
            (Some(timeout), Some(due)) => Some(timeout.min(due)),
            (timeout, due) => timeout.or(due),
        };
        // A profile the owner asked for ends on time even while nothing else wakes the loop.
        match (timeout, tilo_frame_metrics::profile::deadline()) {
            (timeout, None) => timeout,
            (Some(timeout), Some(due)) => Some(timeout.min(due.saturating_duration_since(now))),
            (None, Some(due)) => Some(due.saturating_duration_since(now)),
        }
    }

    /// What this pass's services did: an agent's turns fed, the services polled, so the surfaces
    /// below show what they published.
    pub(super) fn serve_agent_runs_and_services(&self) {
        // What the session's own resources read, before the services poll the readings this pass
        // hands them.
        self.read_controls();
        self.shared.feed_agents();
        self.shared.follow_recipients();
        // What the apps' hosts said they play since the last pass, as the desktop's media
        // controls show it.
        self.shared.table.follow_media();
        // The apps' resources the services run over are the workspace's, once the session holds it.
        self.shared
            .keep_app_stores(self.automation_runtime.handle());
        let mut services = self.shared.services.borrow_mut();
        // What the records the services keep themselves reference: each record's content, kept in
        // the store of the workspace it stands in and offered on its route, so a device of the
        // workspace fetches a part from where the record is. An agent's change too large to record
        // whole is recorded rather than refused, and reads back here and on the person's devices.
        self.lend_record_content(&mut services);
        // Where the workspace's resources open on the web, as the session lends it to a tilolang
        // window's host: the session's own services mint their `share-link` under the same page.
        self.lend_link_home(&mut services);
        // What the packages the session runs itself (the shell) wrote into the records their parts
        // share, published with every app's before the API answers this pass.
        if let Err(error) = services.follow_shared() {
            tracing::error!(%error, "a package's records are not followed");
        }
        services.poll();
        if let Some(app_wire) = &self.app_wire {
            app_wire.link.serve(&mut services);
        }
        // A follow of a reading reads again once what the services publish moved.
        #[cfg(target_os = "linux")]
        self.shared
            .session_services
            .readings_moved(services.revision());
    }

    /// Lend the session's own services the page the active workspace's resources open on the
    /// web, as the session lends it to a tilolang window's host: where a resource they publish
    /// is a browser page's, its `share-link` mints under it.
    fn lend_link_home(&self, services: &mut tilo_service::Services) {
        let workspace = self
            .shared
            .handle
            .read(|tree| tree.active_workspace().map(|space| space.id().to_string()));
        let home = workspace
            .as_deref()
            .and_then(|workspace| self.shared.link_home_json(workspace));
        services.lend_link_home(home.as_deref());
    }

    /// Keep each record the services keep themselves in the content store of the workspace it
    /// stands in: the session's own records' large parts are offered on their routes, where the
    /// workspace's devices fetch them, sealed under the secret its members hold
    /// ([`crate::content::ContentService::store`]).
    fn lend_record_content(&self, services: &mut tilo_service::Services) {
        let Some(workspace) = self
            .shared
            .handle
            .read(|tree| tree.active_workspace().map(|space| space.id().to_string()))
        else {
            return;
        };
        let front = tilo_desktop_model::WorkspaceId::parse(&workspace);
        let engine = self.shared.engine.as_deref();
        let content = &self.content;
        services.lend_content(&mut |app| {
            if let (Some(id), Some(engine)) = (front, engine) {
                content.seal_under(&workspace, app, secret_of(engine, id, app));
            }
            content.store(&workspace, app)
        });
    }

    /// The person's login is over once a frame reaches the display: the startup is measured and
    /// closed, and the heap it left is reported.
    pub(super) fn first_frame_presented(&mut self) {
        if self.first_frame || self.compositor.frames_presented() == 0 {
            return;
        }
        tilo_startup_metrics::mark("first-frame-presented");
        tilo_compositor::heap_trim::log_milestone("first-frame");
        tilo_startup_metrics::close_startup();
        self.first_frame = true;
    }

    /// The desktop is usable, and says so once: every enabled output has shown its first frame —
    /// the lock surface when this session starts locked, which is what that frame draws — and the
    /// windows this device left open are being asked for. What this publishes is what the update
    /// service waits for before it calls a restarted desktop back.
    pub(super) fn desktop_ready(&mut self) {
        let Some(ready) = &mut self.ready else {
            return;
        };
        if ready.published() {
            return;
        }
        let outputs = self.compositor.outputs();
        let mut enabled = outputs.iter().filter(|output| output.enabled).peekable();
        if enabled.peek().is_none()
            || !enabled.all(|output| self.compositor.output_has_presented(&output.name))
        {
            return;
        }
        if !self.shared.restore.borrow().started() {
            return;
        }
        if let Err(error) = ready.publish() {
            tracing::warn!(%error, "the desktop's ready record is not published");
        }
    }

    /// One turn of the loop: wait up to `timeout` for clients and devices,
    /// carry out what the desktop asked, and draw. What arrives on a socket,
    /// a controller or a surface ends the wait however long `timeout` is;
    /// `None` waits for one of them with no limit.
    ///
    /// A frame the displays owe is drawn at their next blank, and what
    /// arrives before it is carried out with it, so a turn can run up to one
    /// refresh past `timeout` while a frame is held for its blank.
    ///
    /// # Errors
    /// The compositor failed.
    pub fn step(&mut self, timeout: impl Into<Option<Duration>>) -> Result<(), Error> {
        // This thread draws the desktop for the length of the pass: a search that would wait for a
        // source here is caught where it is called in a development build, rather than freezing
        // the desktop as it did on 2026-11-13 (`tilo_desktop_search::the_loop`).
        tilo_desktop_search::the_loop(|| self.pass(timeout.into()))
    }

    /// One pass, with the loop's marker already set by [`Self::step`].
    fn pass(&mut self, timeout: Option<Duration>) -> Result<(), Error> {
        self.compositor.begin_tick();
        let timeout = self.wait(timeout);
        tilo_frame_metrics::profile::expire(Instant::now());
        self.compositor.dispatch(timeout)?;
        // A launch asked since the last pass starts before this one waits for its frame, and
        // again each time that wait is woken: the app's own start then runs beside the frame the
        // session draws rather than after it, and whoever is waiting on the launch hears back at
        // once.
        let launch = self.shared.launch.clone();
        if let Some(launch) = &launch {
            launch.follow_render_node(|| self.compositor.render_node(), self.first_frame);
            launch.start_asked_now();
            self.follow_windows_identities(launch);
        }
        // What a tool handed the session's input is taken where a device's own input reaches the session:
        // here, and each time the wait for the frame below is woken, so the frame carries it as it
        // carries a key the person pressed.
        let mut tool_input = serve_tool_input(&self.control, &self.shared, &mut self.compositor);
        // So is what the keyboard and pointer of another of the person's devices sent this one.
        serve_fabric_input(&self.shared, &mut self.compositor);
        // A frame that is owed is shown at the display's next blank: what arrives before that blank
        // joins the frame it is shown in, so the session below is laid out once per refresh however
        // fast events come.
        self.compositor.await_frame_arrivals(|compositor| {
            if let Some(launch) = &launch {
                launch.start_waiting();
            }
            tool_input = tool_input.saturating_add(serve_tool_input(
                &self.control,
                &self.shared,
                compositor,
            ));
            serve_fabric_input(&self.shared, compositor);
        })?;
        self.tool_input = tool_input;
        self.follow_logout_request();
        // What the session itself does before the frame: the session, the apps and the overlays this
        // pass lays out.
        let pass = Instant::now();
        // The person's language, before this pass resolves any of its words.
        self.follow_language();
        // The dates, clocks and weeks they write, as their region now says them.
        self.follow_formats();
        // What the person is in the middle of, as the session may not move under it (shell.md §1).
        let tilo_compositor::Underway { gesture, typing } = self.compositor.underway();
        let underway = tilo_session_core::layout::Busy { gesture, typing };
        self.shared.underway.set(underway);
        // What each window shows, as its app holds the record: the note this pass's session, layout
        // and layout are read from.
        self.read_window_notes();
        self.shared.follow_layout(underway);
        self.follow_controllers()?;
        self.follow_displays()?;
        self.follow_overlays();
        self.follow_setup();
        self.serve_agent_runs_and_services();
        // The staged system update, finished on its own clock: the desktop restarts onto it once
        // the person is away, which ends this process and comes back on the new interface.
        self.follow_update();
        // What the services published this pass is what every surface of the machine shows.
        self.shared.follow_status();
        // A due event the apps published is announced once, on the pass it arrived in.
        self.shared.activities.announce(&*self.shared.telling());
        self.follow_readout();
        self.shared.show_answered_readouts();
        // The surface apps are answered after the services took in what their devices did, so a
        // panel is sent the reading of this pass.
        self.surfaces.poll();
        self.surfaces.serve(&mut self.shared.published);
        self.surfaces.place(&self.compositor);
        // The windows shared with the workspaces: what the person asked,
        // what the live session brought, and the frames of the ones on screen.
        self.follow_window_shares();
        self.follow_text_inputs();
        self.follow_notifications();
        self.shared.open_notifications();
        // What Super alone asked for: the search of a tap, and the keys a hold shows or takes
        // away, before the pass that runs the chord ending them.
        self.shared.follow_super_tap(self.compositor.locked());
        self.settle()?;
        self.follow_switcher();
        self.follow_shell()?;
        self.follow_part_views()?;
        // The toolkit's clock, which a capture pins the frame's instant on.
        let now = ui::time::now();
        self.windows.step(&mut self.compositor, now);
        // The session this pass laid out is published before it is drawn: whoever reads the session's
        // state reads the session the frame about to be presented shows, not the one before it.
        self.control
            .publish(&session(&self.shared, &self.compositor));
        self.compositor.mark_pump(pass.elapsed());
        // The session's windows are drawn as far as they have come; an animated switch owns its frames
        // until they rest.
        self.compositor.set_switch_holding(self.windows.animating());
        // What the pass laid out went out before it is drawn: a client that answers it at once — a
        // window committing the frame it was placed in, an app acking the configure — is in this
        // frame, not in the next one.
        self.compositor.catch_up()?;
        let drawn = self.compositor.frames_presented();
        self.compositor.render()?;
        if self.compositor.frames_presented() == drawn {
            // The session stayed as it was: a compositor that has gone quiet after work gets freed
            // heap back once it stays quiet a moment.
            tilo_compositor::heap_trim::request_when_quiet();
        }
        // What arrived while the frame was drawn — the first pixels of a window that just opened —
        // is drawn in this turn too, so a first frame never waits a whole turn of the loop for it.
        // A turn answers at most a frame or two of arrivals and leaves the rest to the next.
        for _ in 0..2 {
            if !self.compositor.catch_up()? {
                break;
            }
            self.compositor.render()?;
        }
        self.first_frame_presented();
        self.desktop_ready();
        // What the session does once the frame is out, the control socket's answer aside (its own
        // `testctl_us`): the turn's `after_us`.
        let after = Instant::now();
        let answering = self.serve_drawn_frame();
        self.follow_shell_over_frame()?;
        // A notification the person handed to their agent, and a send taken since this pass's
        // settle — a tool's, answered over the frame just drawn, or the canvas's, asked as the
        // shell was read over it — go to Chat now: a message never waits for the next wake of the
        // loop, nor for the frame that wake draws.
        self.shared.deliver_hands();
        self.shared.deliver_sends();
        // What search and the session's steps read of what the session publishes is read once the
        // frame is out: a key's turn draws what it opens before anything is read, and the reading
        // never delays a frame. A session that keeps drawing is still read, every tenth of a second.
        if !self.compositor.owes_frame() || self.kept_at.elapsed() >= KEEP_INTERVAL {
            self.kept_at = Instant::now();
            self.shared.observe();
            self.follow_session_actions();
            self.follow_settings_steps();
        }
        self.compositor
            .mark_after(after.elapsed().saturating_sub(answering));
        self.compositor.end_tick();
        Ok(())
    }

    /// The windows shared with the workspaces: what the person asked, what the live session brought, and
    /// the frames of the ones on screen.
    fn follow_window_shares(&mut self) {
        // Pictures the decoder finished since the last pass reach their tabs first, so the shares
        // read this pass's picture.
        self.shared.handle.follow_peer_streams();
        let sync = self.shared.sync.clone();
        let displays = &self.displays_service;
        self.shared.window_shares.borrow_mut().step(
            &mut self.compositor,
            &self.shared.handle,
            displays,
            sync.as_deref(),
            self.shared.view.as_ref(),
            &self.shared.pages,
            Instant::now(),
        );
    }

    /// A Windows program's window names the class its saved tab reopens with: what makes the next
    /// start find the program again after a reboot.
    fn follow_windows_identities(&self, launch: &crate::apps::Apps) {
        if !launch.windows_wanted() {
            return;
        }
        let windows: Vec<(u32, String)> = self
            .compositor
            .windows()
            .into_iter()
            .filter_map(|(window, _)| Some((window.pid?, window.app_id?)))
            .collect();
        launch.note_windows_identities(&windows);
    }

    /// End a walk the session cannot keep: the session locked, or the walk has no window left to
    /// reach. The shell shows the walk it reads; a walk shorter than its reveal delay shows nothing.
    fn follow_switcher(&self) {
        let shared = &self.shared;
        let walk = &shared.overlays;
        if self.compositor.locked()
            || (walk.switcher_up() && crate::windows::switcher_walk_empty(&shared.handle, walk))
        {
            walk.forget_walk();
            walk.hide(tilo_session_core::desktop::overlays::Overlay::Switcher);
        }
    }

    /// What the frame just drawn asked of the session: the control socket's answer, what assistive
    /// technology reads of it, a live projection and a call it is shared to. Answers how long the
    /// control socket's answer took.
    fn serve_drawn_frame(&mut self) -> Duration {
        // A tile showing another device's window took its picture while the session built this frame,
        // after this turn's pass of the shares asked it: the shares ask their tiles again, so what
        // they say of a window is what the frame just drawn shows of it.
        self.shared
            .window_shares
            .borrow_mut()
            .follow_drawn(&self.shared.handle);
        let answering = Instant::now();
        // What `tilo-control` asks of the frame just drawn.
        let session = session(&self.shared, &self.compositor);
        self.control.serve(&mut self.compositor, &session);
        // A control the socket pressed in a window's bar or content is written to its app now:
        // the input the person makes after it is served into what the press opened, never ahead
        // of it.
        #[cfg(target_os = "linux")]
        if let Some(tabs) = &self.tabs {
            tabs.deliver_presses_now(&self.shared.desktop, &self.compositor);
        }
        let answered = answering.elapsed();
        self.compositor
            .mark_testctl(answered.saturating_add(self.tool_input));
        // What assistive technology reads of the frame just drawn, and what it asked of it.
        self.a11y.step(&self.shared.published);
        // A live projection is sent the desktop as the pass just drew it.
        let video = self.display_sheet.video_stream();
        self.projection_video
            .follow(&mut self.compositor, video.as_ref());
        // A call shares the same pass: it takes the frame when it wants one,
        // and a locked desktop hands it nothing.
        crate::calls::share_screen(&mut self.compositor);
        answered
    }

    /// What the session reads of the frame just drawn: the shell and the parts laid out again, what
    /// their records say, and the picture of the workspace in front.
    fn follow_shell_over_frame(&mut self) -> Result<(), Error> {
        self.follow_shell()?;
        self.follow_part_views()?;
        self.settle()?;
        // The session this turn drew is the session kept: its records are written here, where a change
        // made by this turn's layout goes with it.
        self.shared.keep_layout();
        // The picture is the workspace in front's, which a change covering one workspace lands on.
        let active = self.shared.handle.read(|tree| {
            tree.active_workspace()
                .map(tilo_desktop_model::Workspace::id)
        });
        self.shared
            .wallpapers
            .set_active(active.map(|workspace| workspace.to_string()));
        // A command the person starts here joins the workspace in front: the file is written on
        // the switch, never read for on a clock of its own.
        self.published_workspace.follow(active);
        // A workspace in front that changed is a switch the person made; its frames are timed until
        // the session rests.
        if self
            .active_workspace
            .is_some_and(|before| Some(before) != active)
        {
            self.compositor.workspace_switched();
        }
        self.active_workspace = active;
        Ok(())
    }

    /// When something of the session's next needs a pass with no event in
    /// between: a frame a projection or a shared screen is due, a minute of
    /// the session's clock, a banner leaving, a presence beat, a listing asked
    /// again, a frame of a recording, a probe of a device being woken, a
    /// saved window giving up, a schedule's tick, a sample of the
    /// accelerometer, a shared window's next frame, a screen share's next
    /// frame, a readout no device answered yet, the warmth a schedule changes,
    /// the next look for an update to finish.
    /// `None` when nothing does, and the loop waits for an event alone.
    pub fn next_deadline(&self) -> Option<Instant> {
        let locked = self.compositor.locked();
        let stream = self.display_sheet.video_stream();
        let video = (!locked)
            .then(|| self.projection_video.next_deadline(stream.as_ref()))
            .flatten();
        let shared_screen = (!locked).then(crate::calls::next_screen_frame).flatten();
        // A night light that follows a schedule and a panel that follows the light in the room both
        // come and go on their own: the loop looks at the clock rather than waiting for someone to
        // touch something.
        let now = Instant::now();
        let night_light = self
            .night_light
            .wait()
            .and_then(|wait| now.checked_add(wait));
        let ambient = self
            .auto_brightness
            .wait()
            .and_then(|wait| now.checked_add(wait));
        let deadlines = vec![
            video,
            shared_screen,
            night_light,
            ambient,
            self.shared.notifications.borrow().next_deadline(),
            self.shared
                .sync
                .as_ref()
                .and_then(|sync| sync.next_deadline()),
            self.shared.activity.next_deadline(),
            self.surfaces.next_deadline(),
            // Alt+Tab's panel comes up while Alt stays held: the one instant it is due at, so
            // nothing draws before it.
            self.shared.overlays.deadline(now),
            self.shared.restore.borrow().next_deadline(),
            self.shared.waking.next_deadline(),
            self.capture_studio
                .as_ref()
                .and_then(crate::capture_studio::CaptureStudio::next_deadline),
            self.automation.next_deadline(),
            self.rotation.next_deadline(),
            self.shared.window_shares.borrow().next_deadline(),
            self.shared.handle.next_peer_wake(),
            self.portal
                .as_ref()
                .and_then(|portal| portal.next_deadline(locked)),
            crate::platform::keys::next_readout_deadline(&self.shared.waiting.borrow()),
            // A Super held alone turns into the keys at one instant, whatever else is due.
            self.shared.super_tap_deadline(),
            self.compositor.layer_settle_deadline(),
            Some(self.update_finish.next_deadline()),
        ];
        deadlines.into_iter().flatten().min()
    }

    /// Run until `stop` holds or the backend is done.
    ///
    /// A run asked to last a measured interval (`TILO_RUN_SECONDS`, as the frame instrumentation
    /// documents it) ends by itself, cleanly, once it is over: the trace and the summary of the
    /// run are written on the way out.
    ///
    /// # Errors
    /// The compositor failed.
    pub fn run(&mut self, mut stop: impl FnMut(&Compositor) -> bool) -> Result<(), Error> {
        // Everything from here to the first frame is what the person waits for after the session is
        // built.
        tilo_startup_metrics::mark("render-worker-start");
        let measured = tilo_frame_metrics::run_seconds_from_env()
            .and_then(|seconds| std::time::Instant::now().checked_add(seconds));
        let measured_over = || measured.is_some_and(|over| std::time::Instant::now() >= over);
        while !self.ended
            && !self.compositor.closed()
            && !measured_over()
            && !stop(&self.compositor)
        {
            // Whatever comes first: a client, a device, a due frame of the compositor's own, or an
            // instant something of the session's needs a pass for. With nothing due, the loop waits
            // for the person — or for the end of a measured run.
            let timeout = match (self.compositor.next_wait(), self.next_deadline()) {
                (Some(frame), Some(due)) => {
                    Some(frame.min(due.saturating_duration_since(std::time::Instant::now())))
                }
                (frame, due) => frame.or_else(|| {
                    due.map(|due| due.saturating_duration_since(std::time::Instant::now()))
                }),
            };
            let timeout = match (timeout, measured) {
                (Some(due), Some(over)) => {
                    Some(due.min(over.saturating_duration_since(std::time::Instant::now())))
                }
                (due, over) => due.or_else(|| {
                    over.map(|over| over.saturating_duration_since(std::time::Instant::now()))
                }),
            };
            self.step(timeout)?;
        }
        self.shared.keep_layout_now();
        // Nothing of this session outlives it: an ordered end publishes the lock state the next one
        // recovers, and one that ends any other way keeps the armed record, which recovers locked.
        if let Some(recovery) = self.recovery.take()
            && let Err(error) = recovery.finish(self.compositor.session_locked())
        {
            tracing::error!(
                %error,
                "the login's lock record was not written: the next start recovers locked"
            );
        }
        Ok(())
    }

    /// What the person is writing with besides the keyboard: dictation and the emoji picker, both
    /// through the field they are typing in.
    fn follow_text_inputs(&mut self) {
        // Dictation writes into the field the person is typing in, so it runs before the frame
        // rather than with the sheets.
        let locked = self.compositor.locked();
        self.shared.dictation.step(
            &mut self.compositor,
            &self.shared.handle,
            &self.shared.clipboard,
            locked,
            Instant::now(),
        );
        quick_settings::follow_dictation(&self.shared);
        // The picker's picks go into the field the person was typing in,
        // through the same text-input API dictation writes through.
        let origin = self
            .shared
            .desktop
            .with_widgets(|host| host.placement().origin);
        self.shared
            .emoji
            .step(&mut self.compositor, origin, Instant::now());
    }

    /// A logout asked through the login wrapper's channel, by the power menu or by a service of
    /// this session, ends it in order: the wrapper tears the login down once this process is gone.
    /// The request file already says the logout was asked for, so none is written again.
    fn follow_logout_request(&mut self) {
        if os_actions::take_logout_request() && !self.ended {
            tracing::info!("the logout asked for through the login wrapper ends this session");
            if let Some(desktop) = &self.desktop {
                desktop.end();
            }
            self.ended = true;
        }
    }

    /// The notifications that arrived this pass, and the flash a person who asked for visual alerts
    /// sees for them.
    fn follow_notifications(&mut self) {
        let compositor = &mut self.compositor;
        let appeared = self
            .shared
            .notifications
            .borrow_mut()
            .step(&mut || Some(compositor.activation_token()));
        // What the person was just told flashes the screen when they asked for a visual alert: a
        // person who cannot hear the alert sound is told of it the same way.
        if appeared && compositor.a11y().visual_alerts {
            compositor.flash();
        }
    }

    /// Carry out everything asked since the last look: the desktop's asks,
    /// the gestures it took, the actions keys asked for, and what those
    /// asked in turn.
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered pass: what was asked, in the order the session, its panels and its hosted surfaces must see it"
    )]
    pub(super) fn settle(&mut self) -> Result<(), Error> {
        // What the history wrote back reaches the session before anything reads it, and what the
        // person opened from it starts from here.
        self.shared.serve_activity();
        self.shared.serve_mentions();
        self.follow_collaboration();
        // A workspace's strips and windows are the workspace's: what its members put there —
        // before this device joined, and since — is this device too, and what this device
        // arranges there is theirs to see. A session with no one else on it writes its workspaces'
        // layouts through the same records.
        self.shared.follow_layouts();
        self.follow_automation();
        // The person's list takes in the workspaces of theirs the device's daemon holds on hosts.
        self.shared.follow_records();
        // The person's settings follow them between their devices in the workspace whose only
        // member is them: what they set here is written there first, and what they set there
        // reaches the document this session reads.
        if let Some(view) = &self.shared.view {
            self.shared.settings.sync(
                &self.shared.daemon,
                &view.person(),
                &self.shared.wake_handle,
            );
        }
        // What was handed to an agent, and what was sent since the last look, by a box, an agent
        // or a script.
        self.shared.deliver_hands();
        self.shared.deliver_sends();
        self.follow_power()?;
        if let Some(comforts) = &mut self.comforts {
            comforts.observe();
        }
        self.follow_owner_session()?;
        self.follow_lock()?;
        self.follow_keyboard();
        // The engine of the active source, after the on-screen keyboard gave up the seat: one input
        // method owns it.
        self.follow_input_method();
        if let Some(captures) = &mut self.captures {
            captures.serve(
                &mut self.compositor,
                self.capture_studio.as_mut(),
                &self.capture_io,
            );
        }
        if let Some(studio) = &mut self.capture_studio {
            studio.serve(&mut self.compositor);
        }
        if let Some(portal) = &mut self.portal {
            let documents_served = self
                .desktop
                .as_ref()
                .is_some_and(|desktop| desktop.documents().is_some());
            portal.serve(
                &mut self.compositor,
                &self.shared.handle,
                &self.launches,
                documents_served,
            );
        }
        self.feed_portal();
        self.feed_capabilities();
        self.feed_window_artwork();
        // What Files copied goes on the desktop's clipboard, as a file manager's copy does: the
        // person pastes it into any app.
        if let Some(app_wire) = &self.app_wire {
            for files in app_wire.file_clipboard.take_published() {
                crate::clipboard::offer_files(&mut self.compositor, &files);
            }
        }
        // What the apps copied since the last pass joins the history; a copy chosen in it is pasted
        // once the panel let the keyboard go.
        self.shared.clipboard.follow(&mut self.compositor);
        // What the tilolang apps copied goes on the clipboard as the person's own copy, and what
        // they paste is read off the selection: their processes hold no display to do it on.
        if let Some(clipboard) = self.shared.table.clipboard() {
            clipboard.follow(&self.shared.clipboard, &mut self.compositor);
        }
        // The question about a display change, before the sheets are placed: one that just opened
        // stands on this pass's displays.
        if let Some(confirm) = self.shared.display_confirm.as_ref() {
            let notices = self.shared.telling();
            self.confirmation
                .follow(&mut self.compositor, confirm, &self.displays, &*notices);
        }
        self.display_sheet.follow(
            &self.compositor.outputs(),
            self.displays.awaiting_confirmation().is_some(),
            &self.shared.invocations,
        );
        self.place_sheets();
        // What the async work this session started answered since the last pass.
        self.shared.landings.land();
        self.settle_asks()?;
        // Who each workspace on the shell is whose, once this pass has opened the ones it was asked
        // for: the person's own are theirs, and one joined keeps its host's owner. A workspace the
        // session shows is claimed in the pass that shows it, never a pass later — a person's action in
        // it is decided against that ownership.
        self.shared.own_workspaces();
        // The broker decides a person's own action against the copy of the session taken here, which
        // follows every change this pass made, so a workspace the person just came into is theirs
        // there too when their first action in it arrives.
        self.publish_layout();
        // What the bar and the session's roles asked of the chrome is carried out: the session
        // this pass lays out and publishes names it, not the one after.
        self.shared.follow_intents();
        // Which apps stopped answering reaches the session, and the person's Wait or Force quit
        // reaches the compositor.
        self.follow_app_liveness();
        // The apps the person can see keep the foreground tier; the ones they cannot give memory
        // back first when the machine runs low.
        self.follow_app_tiers();
        // The menu the reading says is up has its view over the shell, and the view goes when the
        // menu does.
        self.follow_window_menu();
        // Each open window takes the temperature of the workspace it is in, as this pass left it.
        self.follow_window_warmth();
        self.serve_clipboard_asks();
        // A paste of files runs on its own thread; the loop only asks whether it is over and tells
        // the person how it went.
        for notice in self.shared.clipboard.step_files() {
            self.shared.notifications.borrow().tell(&notice);
        }
        // What was copied here reaches the person's other devices, and what they copied there joins
        // the clipboard here.
        if let Some(fabric) = &self.shared.fabric {
            let media_enabled = fabric.require_media().is_ok();
            self.shared.clipboard.sync_fabric(fabric.as_ref());
            // What the person's devices sent this one, before this pass's pumps: a route they asked
            // for starts on the same pass it was asked for on.
            if let Some(sync) = &self.shared.sync {
                for envelope in sync.fabric_actions() {
                    self.route_fabric_action(fabric, &envelope);
                }
            }
            // The person's speaker groups, remote audio and the view Settings asks of them move on
            // the same fabric, on the same pass: a device arriving, a group being created or a
            // calibration progressing is what the next snapshot shows.
            tilo_speaker_service::remote_audio::pump(fabric.as_ref(), media_enabled, None);
            tilo_speaker_service::groups::pump(fabric.as_ref(), media_enabled);
            tilo_speaker_service::settings::observe(fabric.as_ref());
            // A Bluetooth accessory the person moved to another of their devices, and what came of
            // the moves under way.
            self.shared.bluetooth.pump(&self.shared, fabric);
            // The person's sleeping devices, and the probes that follow them.
            self.shared.waking.follow(&self.shared);
            self.shared.follow_device_vpn(fabric);
            // What this pass offered the clipboard, and what the pumps just changed, is this
            // device's next word to its other devices.
            if let Some(sync) = &self.shared.sync {
                sync.announce_fabric();
            }
        }
        // The keyboard and pointer of the person's other devices that drive this one, on the pass
        // their actions were routed in.
        if let Some(shared_input) = &self.shared.keyboard_and_pointer {
            // A drag carried between the person's devices keeps its content in the workspace's,
            // sealed under the secret its members hold, as a record the session keeps itself does.
            let engine = self.shared.engine.as_deref();
            let content = &self.content;
            let store = |workspace: &str| {
                let id = tilo_desktop_model::WorkspaceId::parse(workspace)?;
                let app = tilo_desktop_model::ownership::DESKTOP_APP;
                if let Some(engine) = engine {
                    content.seal_under(workspace, app, secret_of(engine, id, app));
                }
                Some(content.store(workspace, app))
            };
            let session = crate::shared_input::Shell {
                handle: &self.shared.handle,
                content: &store,
            };
            let handover = shared_input.follow(&mut self.compositor, &session);
            // The clipboard follows them: what was copied last is what pastes where they are.
            self.shared
                .clipboard
                .follow_control(handover.arrived, &handover.partners);
        }
        Ok(())
    }
}

/// The session's input a tool handed the loop since it last looked, served where a device's input is:
/// before the pass lays its frame out, so the frame carries it. How long answering it took.
fn serve_tool_input(
    control: &crate::test_control::Control,
    shared: &crate::Shared,
    compositor: &mut Compositor,
) -> std::time::Duration {
    let answering = Instant::now();
    let session = session(shared, compositor);
    control.serve_input(compositor, &session);
    answering.elapsed()
}

/// What the keyboard and pointer of the person's other devices sent this one, handed to the session the
/// moment it arrived rather than on the pass that routes the rest of what their devices sent.
fn serve_fabric_input(shared: &crate::Shared, compositor: &mut Compositor) {
    let (Some(sync), Some(shared_input)) = (&shared.sync, &shared.keyboard_and_pointer) else {
        return;
    };
    sync.fabric_actions_now(|envelope| shared_input.route(envelope));
    shared_input.inject(compositor, None);
}

/// The session as the control socket's answers read it now.
fn session<'a>(
    shared: &'a crate::Shared,
    compositor: &Compositor,
) -> crate::test_control::Session<'a> {
    crate::test_control::Session {
        published: &shared.published,
        handle: &shared.handle,
        overlays: &shared.overlays,
        overview: &shared.overview,
        view: shared.view.as_deref(),
        agent_sessions: &shared.agent_sessions,
        sync: shared.sync.as_deref(),
        shares: &shared.window_shares,
        locked: compositor.locked(),
        lock_screen_drawn: compositor.lock_drawn(),
        lock: &shared.lock,
        pointer: compositor.pointer_location(),
        pointer_shown: compositor.pointer_shown(),
        clipboard: &shared.clipboard,
        shared_input: shared.keyboard_and_pointer.as_deref(),
        layout: &shared.layout,
        notifications: shared.notifications.borrow().desktop(),
    }
}
