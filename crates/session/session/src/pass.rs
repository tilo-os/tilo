//! The loop's pass: the turn it takes, when it may sleep, and what the login's first frame closes.
//! [`Session::run`] is the loop and [`Session::step`] one turn of it: wait for clients, devices and
//! the session's own due work, then carry out what the desk asked and draw. What this file holds is
//! what a turn answers apart from drawing — the shortest wait its own clocks ask for, the session's
//! share of the frame's timings, the runs and services it fed, everything asked since the last look
//! ([`Session::settle`]), and the end of the startup measure.

use std::time::{Duration, Instant};

use crate::{Session, import_readiness, quick_settings};
use tilo_compositor::Compositor;

use crate::Error;

/// How long the desk's own bookkeeping may wait for a turn that draws nothing: a desk that keeps
/// drawing is read this often all the same.
const KEEP_INTERVAL: Duration = Duration::from_millis(100);

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
        let mut services = self.shared.services.borrow_mut();
        // What the packages the session runs itself (the shell) wrote into the records their parts
        // share, published with every app's before the door answers this pass.
        if let Err(error) = services.follow_shared() {
            tracing::error!(%error, "a package's records are not followed");
        }
        services.poll();
        if let Some(plane) = &self.action_plane {
            plane.link.serve(&mut services);
        }
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
        self.compositor.begin_tick();
        let timeout = self.wait(timeout.into());
        tilo_frame_metrics::profile::expire(Instant::now());
        self.compositor.dispatch(timeout)?;
        // A launch asked since the last pass starts before this one waits for its frame, and
        // again each time that wait is woken: the app's own start then runs beside the frame the
        // desk draws rather than after it, and whoever is waiting on the launch hears back at
        // once.
        let launch = self.shared.launch.clone();
        if let Some(launch) = &launch {
            launch.follow_render_node(|| self.compositor.render_node());
            launch.start_asked_now();
        }
        // What search reads of the desk is read in turns that have nothing to draw. What arrived
        // above is drawn first: a window committing the frame it was configured for is answered by
        // this turn's frame rather than by a turn the index is read in first. A desk that keeps
        // drawing is still read, every tenth of a second.
        if !self.compositor.owes_frame() || self.kept_at.elapsed() >= KEEP_INTERVAL {
            self.kept_at = Instant::now();
            self.shared.observe();
        }
        // What a tool handed the desk's input since the last pass is taken here, where a device's
        // own input reaches the desk: the frame below carries it, as it carries a key the person
        // pressed.
        self.serve_tool_input();
        // A frame that is owed is shown at the display's next blank: what arrives before that blank
        // joins the frame it is shown in, so the desk below is laid out once per refresh however
        // fast events come.
        self.compositor.await_frame_arrivals(|| {
            if let Some(launch) = &launch {
                launch.start_waiting();
            }
        })?;
        self.follow_logout_request();
        // What the session itself does before the frame: the desk, the apps and the overlays this
        // pass lays out.
        let pass = Instant::now();
        // The person's language, before this pass resolves any of its words.
        self.follow_language();
        // Add folders: answered whether the identity ceremony is over, and the review the person
        // left opens once it is.
        self.follow_folder_import();
        // What the person is in the middle of, as the desk may not move under it (shell.md §1).
        let underway = crate::arrangement::busy(self.compositor.underway());
        self.shared.underway.set(underway);
        // What each window shows, as its app holds the record: the note this pass's desk, layout
        // and arrangement are read from.
        self.read_window_notes();
        self.follow_session_actions();
        self.shared.follow_arrangement(underway);
        self.follow_controllers()?;
        self.follow_displays()?;
        self.follow_fullscreen();
        self.follow_overlays();
        self.follow_setup();
        self.serve_agent_runs_and_services();
        // The staged system update, finished on its own clock: the desktop restarts onto it once
        // the person is away, which ends this process and comes back on the new interface.
        self.follow_update();
        // What the services published this pass is what every surface of the machine shows.
        self.shared.follow_status();
        self.follow_readout();
        self.shared.show_answered_readouts();
        // The surface apps are answered after the services took in what their devices did, so a
        // panel is sent the reading of this pass.
        self.surfaces.poll();
        self.surfaces.serve(&mut self.shared.doors);
        // The windows shared with the workspaces: what the person asked,
        // what the plane brought, and the frames of the ones on screen.
        self.follow_window_shares();
        self.follow_text_inputs();
        self.follow_notifications();
        self.shared.open_notifications();
        self.settle()?;
        self.follow_bars();
        self.follow_switcher();
        self.follow_shell()?;
        self.follow_slot_views()?;
        // The toolkit's clock, which a capture pins the frame's instant on.
        let now = ui::time::now();
        self.windows.step(&mut self.compositor, now);
        // The overview's windows fly between the desk and the overview: its panel names them, the
        // desk says where each stands, and the compositor draws their own pixels.
        self.shared
            .overview
            .step(&self.shared.handle, &self.shared.desktop, now);
        // The desk this pass laid out is published before it is drawn: whoever reads the session's
        // state reads the desk the frame about to be presented shows, not the one before it.
        crate::test_control::publish(
            &self.control,
            crate::test_control::Desk {
                doors: &self.shared.doors,
                handle: &self.shared.handle,
                overlays: &self.shared.overlays,
                overview: self.shared.overview.open_cell(),
                engine: self.shared.engine.as_deref(),
                seats: &self.shared.seats,
                locked: self.compositor.locked(),
                lock_screen_drawn: self.compositor.lock_drawn(),
                lock: &self.shared.lock,
            },
        );
        self.compositor.mark_pump(pass.elapsed());
        // The desk's windows are drawn as far as they have come; an animated switch owns its frames
        // until they rest.
        self.compositor.set_switch_holding(self.windows.animating());
        // What the pass laid out went out before it is drawn: a client that answers it at once — a
        // window committing the frame it was placed in, an app acking the configure — is in this
        // frame, not in the next one.
        self.compositor.catch_up()?;
        let drawn = self.compositor.frames_presented();
        self.compositor.render()?;
        if self.compositor.frames_presented() == drawn {
            // The desk stayed as it was: a compositor that has gone quiet after work gets freed
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
        self.serve_drawn_frame();
        self.follow_desk_over_frame()?;
        self.compositor.end_tick();
        Ok(())
    }

    /// The windows shared with the workspaces: what the person asked, what the plane brought, and
    /// the frames of the ones on screen.
    fn follow_window_shares(&mut self) {
        let sync = self.shared.sync.clone();
        let displays = &self.displays_service;
        self.shared.window_shares.borrow_mut().step(
            &mut self.compositor,
            &self.shared.handle,
            displays,
            sync.as_deref(),
            &self.shared.pages,
            Instant::now(),
        );
    }

    /// What the desk's walk reached this pass, handed to the panel that shows it: whether the
    /// session is locked, and where each of the desk's windows stands.
    fn follow_switcher(&self) {
        // Each window's size in logical pixels, as the desk lays it out: the shape a card of the
        // panel takes, so the window's picture fills it.
        let sizes = self
            .compositor
            .windows()
            .into_iter()
            .filter_map(|(window, (_, _, width, height))| {
                Some((
                    window.id,
                    (u32::try_from(width).ok()?, u32::try_from(height).ok()?),
                ))
            })
            .filter(|(_, (width, height))| *width > 0 && *height > 0)
            .collect();
        let shared = &self.shared;
        if crate::switcher::show(
            &shared.handle,
            &shared.desk_overlays,
            &shared.switcher,
            self.compositor.locked(),
            &sizes,
        ) {
            // The panel stands over the desk's windows: the chrome lays itself out again for what
            // it now shows, as it does for any surface of the desk's going up or down.
            crate::platform::surfaces::Desk::relayout(&shared.desktop);
        }
    }

    /// The desk's input a tool handed the loop since the last pass, served where a device's input
    /// is: before the pass lays its frame out, so the frame carries it.
    fn serve_tool_input(&mut self) {
        let answering = Instant::now();
        let (locked, lock_screen_drawn) = (self.compositor.locked(), self.compositor.lock_drawn());
        crate::test_control::serve_input(
            &self.control,
            &mut self.compositor,
            crate::test_control::Desk {
                doors: &self.shared.doors,
                handle: &self.shared.handle,
                overlays: &self.shared.overlays,
                overview: self.shared.overview.open_cell(),
                engine: self.shared.engine.as_deref(),
                seats: &self.shared.seats,
                locked,
                lock_screen_drawn,
                lock: &self.shared.lock,
            },
        );
        self.tool_input = answering.elapsed();
    }

    /// What the frame just drawn asked of the session: the control socket's answer, what assistive
    /// technology reads of it, a live projection and a call it is shared to.
    fn serve_drawn_frame(&mut self) {
        let answering = Instant::now();
        // What `tilo-control` asks of the frame just drawn.
        let locked = self.compositor.locked();
        let lock_screen_drawn = self.compositor.lock_drawn();
        crate::test_control::serve(
            &self.control,
            &mut self.compositor,
            crate::test_control::Desk {
                doors: &self.shared.doors,
                handle: &self.shared.handle,
                overlays: &self.shared.overlays,
                overview: self.shared.overview.open_cell(),
                engine: self.shared.engine.as_deref(),
                seats: &self.shared.seats,
                locked,
                lock_screen_drawn,
                lock: &self.shared.lock,
            },
        );
        self.compositor
            .mark_testctl(answering.elapsed().saturating_add(self.tool_input));
        // What assistive technology reads of the frame just drawn, and what it asked of it.
        self.a11y.step(&self.shared.doors);
        // A live projection is sent the desktop as the pass just drew it.
        let video = self.display_sheet.video_stream();
        self.projection_video
            .follow(&mut self.compositor, video.as_ref());
        // A call shares the same pass: it takes the frame when it wants one,
        // and a locked desktop hands it nothing.
        crate::calls::share_screen(&mut self.compositor);
    }

    /// What the desk reads of the frame just drawn: the shell and the slots laid out again, what
    /// their records say, and the picture of the workspace in front.
    fn follow_desk_over_frame(&mut self) -> Result<(), Error> {
        self.follow_shell()?;
        self.follow_slot_views()?;
        self.settle()?;
        // The desk this turn drew is the desk kept: its records are written here, where a change
        // made by this turn's layout goes with it.
        self.shared.keep_desk();
        // The picture is the workspace in front's, which a change covering one workspace lands on.
        let active = self
            .shared
            .handle
            .read(|tree| tree.active_workspace().map(|workspace| workspace.slot.0));
        self.shared
            .wallpapers
            .set_active(active.map(|workspace| workspace.to_string()));
        // A workspace in front that changed is a switch the person made; its frames are timed until
        // the desk rests.
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
    /// the desk's clock, a banner leaving, a presence beat, a listing asked
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
            self.shared.desk_overlays.deadline(now),
            self.shared.restore.borrow().next_deadline(),
            // A branch being copied off the loop is looked at again soon, so it goes on the desk
            // once it is there.
            self.shared.branches.next_deadline(now),
            self.shared.waking.next_deadline(),
            // Quick settings is this session's own layer: its app drew the
            // panel once and the pose the compositor grows it by is set here,
            // so the pass that moves it is this loop's to schedule.
            self.quick_settings_panel.next_pass(now),
            self.capture_studio
                .as_ref()
                .and_then(crate::capture_studio::CaptureStudio::next_deadline),
            self.automation.next_deadline(),
            self.rotation.next_deadline(),
            self.shared.window_shares.borrow().next_deadline(),
            self.portal
                .as_ref()
                .and_then(|portal| portal.next_deadline(locked)),
            crate::platform::keys::next_readout_deadline(&self.shared.waiting.borrow()),
            Some(self.update_finish.next_deadline()),
        ];
        deadlines.into_iter().flatten().min()
    }

    /// Run until `stop` holds or the backend is done.
    ///
    /// # Errors
    /// The compositor failed.
    pub fn run(&mut self, mut stop: impl FnMut(&Compositor) -> bool) -> Result<(), Error> {
        // Everything from here to the first frame is what the person waits for after the desk is
        // built.
        tilo_startup_metrics::mark("render-worker-start");
        while !self.ended && !self.compositor.closed() && !stop(&self.compositor) {
            // Whatever comes first: a client, a device, a due frame of the compositor's own, or an
            // instant something of the session's needs a pass for. With nothing due, the loop waits
            // for the person.
            let timeout = match (self.compositor.next_wait(), self.next_deadline()) {
                (Some(frame), Some(due)) => {
                    Some(frame.min(due.saturating_duration_since(std::time::Instant::now())))
                }
                (frame, due) => frame.or_else(|| {
                    due.map(|due| due.saturating_duration_since(std::time::Instant::now()))
                }),
            };
            self.step(timeout)?;
        }
        self.shared.keep_desk_now();
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

    /// Answer the installed folder importer this pass, and open the review it saved once the
    /// person's first use is over: the Add folders click they left for after setup rather than one
    /// they must remember.
    fn follow_folder_import(&self) {
        let first_use_over = !self.setup_up();
        let opens_now = self
            .shared
            .folder_import
            .borrow_mut()
            .as_mut()
            .is_some_and(|import| import.follow(first_use_over));
        if !opens_now {
            return;
        }
        let Some(apps) = &self.shared.launch else {
            return;
        };
        let launch = tilo_app_launch::Launch::Host {
            desktop_id: import_readiness::IMPORT_DESKTOP_ID.to_owned(),
            open: None,
        };
        if let Err(error) = apps.open(&launch) {
            tracing::warn!(
                %error,
                "the saved folder import did not open; Add folders opens it"
            );
        }
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
        // through the same text-input door dictation writes through.
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
    pub(super) fn settle(&mut self) -> Result<(), Error> {
        // What the history wrote back reaches the desk before anything reads it, and what the
        // person opened from it starts from here.
        self.shared.serve_activity();
        self.shared.serve_mentions();
        self.follow_collaboration();
        self.follow_automation();
        // The person's list is kept on the hosts of their workspaces through the sessions the
        // broker reaches them on.
        let hosted = self
            .action_plane
            .as_ref()
            .and_then(crate::plane::Plane::action_broker)
            .and_then(|broker| broker.hosted_workspaces());
        self.shared.follow_records(hosted.as_deref());
        // What was sent since the last look, by a box, an agent or a script.
        self.shared.deliver_sends();
        // The community this computer is configured for is joined on its own,
        // once, as the session starts.
        self.shared.follow_community();
        // Who each workspace on the desk is whose, after the workspaces this pass brought: the
        // person's own are theirs, and one joined keeps its host's owner.
        self.shared.own_workspaces();
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
        // Quick settings runs while the stack has its slot up, where the stack placed it.
        self.follow_quick_settings();
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
                .is_some_and(|desktop| desktop.documents_served());
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
        if let Some(plane) = &self.action_plane {
            for files in plane.file_clipboard.take_published() {
                crate::clipboard::offer_files(&mut self.compositor, &files);
            }
        }
        // What the apps copied since the last pass joins the history; a copy chosen in it is pasted
        // once the panel let the keyboard go.
        self.shared.clipboard.follow(&mut self.compositor);
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
        self.settle_asks()?;
        // Which apps stopped answering reaches the desk, and the person's Wait or Force quit
        // reaches the compositor.
        self.follow_app_liveness();
        // The apps the person can see keep the foreground tier; the ones they cannot give memory
        // back first when the machine runs low.
        self.follow_app_tiers();
        // The menu the reading says is up has its view over the desk, and the view goes when the
        // menu does.
        self.follow_window_menu();
        // Each open window takes the temperature of the workspace it is in, as this pass left it.
        self.follow_window_warmth();
        self.shared.clipboard.serve_asks(&mut self.compositor);
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
            // A device another of the person's devices found asks here to be them: its answer
            // leaves, and a new ask is put to the person from their own top bar, where they decide
            // it.
            crate::certification::pump(fabric.as_ref());
            self.shared.follow_device_asks();
            // A device that joined a person's identity holds no root of their own, so nothing here
            // can reissue its certificate. Once the person's devices are reachable, this asks them
            // for the renewal the certificate is due.
            if let Some(renewal) = &self.shared.fleet_renewal {
                renewal.follow();
            }
            // The person's settings follow them between their devices on the same fabric: what they
            // set here is offered to it, and what they set there reaches the document this session
            // reads.
            self.shared.settings.sync(fabric.as_ref());
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
        Ok(())
    }
}
