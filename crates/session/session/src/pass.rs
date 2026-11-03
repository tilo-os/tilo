//! The loop's pass: the turn it takes, when it may sleep, and what the login's
//! first frame closes.
//!
//! [`Session::run`] is the loop and [`Session::step`] one turn of it: wait for
//! clients, devices and the session's own due work, then carry out what the
//! desk asked and draw. What this file holds is what a turn answers apart from
//! drawing — the shortest wait its own clocks ask for, the session's share of
//! the frame's timings, the runs and services it fed, and the end of the
//! startup measure.

use std::time::{Duration, Instant};

use crate::{Session, import_readiness, quick_settings};
use tilo_compositor::{Compositor, Error};

impl Session {
    /// How long this pass may sleep: no longer than `timeout`, and no longer
    /// than the next thing that is due on a clock of its own — a held
    /// direction repeating, or a profile the owner asked for ending.
    pub(super) fn wait(&self, timeout: Option<Duration>) -> Option<Duration> {
        let now = Instant::now();
        // A held direction repeats on its own clock, and a controller with
        // more waiting than one pass read is read again at once.
        let controller = self
            .controllers
            .as_ref()
            .and_then(|controllers| controllers.next_deadline(now))
            .map(|due| due.saturating_duration_since(now));
        let timeout = match (timeout, controller) {
            (Some(timeout), Some(due)) => Some(timeout.min(due)),
            (timeout, due) => timeout.or(due),
        };
        // A profile the owner asked for ends on time even while nothing else
        // wakes the loop.
        match (timeout, tilo_frame_metrics::profile::deadline()) {
            (timeout, None) => timeout,
            (Some(timeout), Some(due)) => Some(timeout.min(due.saturating_duration_since(now))),
            (None, Some(due)) => Some(due.saturating_duration_since(now)),
        }
    }

    /// What this pass's runs and services did: an agent's run fed, the door
    /// that reads its conversation lent, and the services polled, so the
    /// surfaces below show what they published.
    pub(super) fn serve_runs_and_services(&self) {
        // What the session's own resources read, before the services poll
        // the readings this pass hands them.
        self.read_controls();
        self.shared.feed_agent_runs();
        // The door that decides what a person's line in a run's conversation
        // reads and changes, once this pass's runs know where they are
        // (shell.md §8.1).
        let front = self.shared.handle.read(|tree| {
            tree.active_workspace()
                .map(|held| held.id().to_string())
                .unwrap_or_default()
        });
        self.shared
            .lend_conversations(self.action_plane.as_ref(), &front);
        let mut services = self.shared.services.borrow_mut();
        // What the packages the session runs itself (the shell) wrote into the records their
        // parts share, published with every app's before the door answers this pass.
        if let Err(error) = services.follow_shared() {
            tracing::error!(%error, "a package's records are not followed");
        }
        services.poll();
        if let Some(plane) = &self.action_plane {
            plane.link.serve(&mut services);
        }
    }

    /// The person's login is over once a frame reaches the display: the
    /// startup is measured and closed, and the heap it left is reported.
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
    /// # Errors
    /// The compositor failed.
    pub fn step(&mut self, timeout: impl Into<Option<Duration>>) -> Result<(), Error> {
        self.compositor.begin_tick();
        let timeout = self.wait(timeout.into());
        tilo_frame_metrics::profile::expire(Instant::now());
        self.compositor.dispatch(timeout)?;
        self.follow_logout_request();
        // What the session itself does before the frame: the desk, the apps
        // and the overlays this pass lays out.
        let pass = Instant::now();
        // Add folders: answered whether the identity ceremony is over, and the
        // review the person left opens once it is.
        self.follow_folder_import();
        // What the person is in the middle of, as the desk may not move
        // under it (shell.md §1).
        let underway = self.compositor.underway();
        self.shared.underway.set(underway);
        self.shared.follow_arrangement(underway);
        self.follow_controllers()?;
        self.follow_displays()?;
        self.follow_fullscreen();
        self.follow_overlays();
        self.follow_setup();
        self.serve_runs_and_services();
        // What the services published this pass is what every surface of the
        // machine shows.
        self.shared.follow_status();
        self.follow_readout();
        self.shared.show_answered_readouts();
        // The surface apps are answered after the services took in what
        // their devices did, so a panel is sent the reading of this pass.
        self.surfaces.poll();
        self.surfaces.serve(&mut self.shared.doors);
        // The windows shared with the workspaces: what the person asked,
        // what the plane brought, and the frames of the ones on screen.
        {
            let sync = self.shared.sync.clone();
            let displays = &self.displays_service;
            self.shared.window_shares.borrow_mut().step(
                &mut self.compositor,
                &self.shared.handle,
                displays,
                sync.as_deref(),
                std::time::Instant::now(),
            );
        }
        self.follow_text_inputs();
        self.follow_notifications();
        self.shared.open_notifications();
        self.settle()?;
        self.follow_bars();
        self.follow_shell()?;
        self.follow_slot_views()?;
        // The toolkit's clock, which a capture pins the frame's instant on.
        let now = ui::time::now();
        self.windows.step(&mut self.compositor, now);
        // The overview's windows fly between the desk and the overview: its
        // panel names them, the desk says where each stands, and the
        // compositor draws their own pixels.
        self.shared.overview.step(&self.shared, now);
        self.compositor.mark_pump(pass.elapsed());
        // The desk's windows are drawn as far as they have come; an animated
        // switch owns its frames until they rest.
        self.compositor.set_switch_holding(self.windows.animating());
        let drawn = self.compositor.frames_presented();
        self.compositor.render()?;
        if self.compositor.frames_presented() == drawn {
            // The desk stayed as it was: a compositor that has gone quiet
            // after work gets freed heap back once it stays quiet a moment.
            tilo_compositor::heap_trim::request_when_quiet();
        }
        self.first_frame_presented();
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
            },
        );
        self.compositor.mark_testctl(answering.elapsed());
        // What assistive technology reads of the frame just drawn, and what
        // it asked of it.
        self.a11y.step(&self.shared.doors);
        // A live projection is sent the desktop as the pass just drew it.
        let video = self.display_sheet.video_stream();
        self.projection_video
            .follow(&mut self.compositor, video.as_ref());
        // A call shares the same pass: it takes the frame when it wants one,
        // and a locked desktop hands it nothing.
        crate::calls::share_screen(&mut self.compositor);
        self.follow_shell()?;
        self.follow_slot_views()?;
        self.settle()?;
        self.shared.observe();
        self.shared.keep_desk();
        self.shared.keep_arrangement();
        // The picture is the workspace in front's, which a change covering one workspace lands on.
        let active = self
            .shared
            .handle
            .read(|tree| tree.active_workspace().map(|workspace| workspace.slot.0));
        self.shared
            .wallpapers
            .set_active(active.map(|workspace| workspace.to_string()));
        // A workspace in front that changed is a switch the person made; its
        // frames are timed until the desk rests.
        if self
            .active_workspace
            .is_some_and(|before| Some(before) != active)
        {
            self.compositor.workspace_switched();
        }
        self.active_workspace = active;
        self.compositor.end_tick();
        Ok(())
    }

    /// When something of the session's next needs a pass with no event in
    /// between: a frame a projection or a shared screen is due, a minute of
    /// the desk's clock, a banner leaving, a presence beat, a listing asked
    /// again, a frame of a recording, a probe of a device being woken, a
    /// saved window giving up, a schedule's tick, a sample of the
    /// accelerometer, a shared window's next frame, a screen share's next
    /// frame, a readout no device answered yet, the warmth a schedule changes.
    /// `None` when nothing does, and the loop waits for an event alone.
    pub fn next_deadline(&self) -> Option<Instant> {
        let locked = self.compositor.locked();
        let stream = self.display_sheet.video_stream();
        let video = (!locked)
            .then(|| self.projection_video.next_deadline(stream.as_ref()))
            .flatten();
        let shared_screen = (!locked).then(crate::calls::next_screen_frame).flatten();
        // A night light that follows a schedule and a panel that follows the
        // light in the room both come and go on their own: the loop looks at
        // the clock rather than waiting for someone to touch something.
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
            self.shared.restore.borrow().next_deadline(),
            self.shared.waking.next_deadline(),
            self.capture_studio
                .as_ref()
                .and_then(crate::capture_studio::CaptureStudio::next_deadline),
            self.automation.next_deadline(),
            self.rotation.next_deadline(),
            self.shared.window_shares.borrow().next_deadline(),
            self.portal
                .as_ref()
                .and_then(|portal| portal.next_deadline(locked)),
            crate::keys::next_readout_deadline(&self.shared.waiting.borrow()),
        ];
        deadlines.into_iter().flatten().min()
    }

    /// Run until `stop` holds or the backend is done.
    ///
    /// # Errors
    /// The compositor failed.
    pub fn run(&mut self, mut stop: impl FnMut(&Compositor) -> bool) -> Result<(), Error> {
        // Everything from here to the first frame is what the person waits
        // for after the desk is built.
        tilo_startup_metrics::mark("render-worker-start");
        while !self.ended && !self.compositor.closed() && !stop(&self.compositor) {
            // Whatever comes first: a client, a device, a due frame of the
            // compositor's own, or an instant something of the session's needs
            // a pass for. With nothing due, the loop waits for the person.
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
        // Nothing of this session outlives it: an ordered end publishes the
        // lock state the next one recovers, and one that ends any other way
        // keeps the armed record, which recovers locked.
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

    /// Answer the installed folder importer this pass, and open the review it
    /// saved once the person's first use is over: the Add folders click they
    /// left for after setup rather than one they must remember.
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

    /// What the person is writing with besides the keyboard: dictation and
    /// the emoji picker, both through the field they are typing in.
    fn follow_text_inputs(&mut self) {
        // Dictation writes into the field the person is typing in, so it runs
        // before the frame rather than with the sheets.
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

    /// A logout asked through the login wrapper's channel, by the power menu
    /// or by a service of this session, ends it in order: the wrapper tears
    /// the login down once this process is gone. The request file already says
    /// the logout was asked for, so none is written again.
    fn follow_logout_request(&mut self) {
        if os_actions::take_logout_request() && !self.ended {
            tracing::info!("the logout asked for through the login wrapper ends this session");
            if let Some(desktop) = &self.desktop {
                desktop.end();
            }
            self.ended = true;
        }
    }

    /// The notifications that arrived this pass, and the flash a person who
    /// asked for visual alerts sees for them.
    fn follow_notifications(&mut self) {
        let compositor = &mut self.compositor;
        let appeared = self
            .shared
            .notifications
            .borrow_mut()
            .step(&mut || Some(compositor.activation_token()));
        // What the person was just told flashes the screen when they asked
        // for a visual alert: a person who cannot hear the alert sound is
        // told of it the same way.
        if appeared && compositor.a11y().visual_alerts {
            compositor.flash();
        }
    }
}
