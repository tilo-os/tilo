//! The states a review capture stages: each scenario name `--shots` can ask
//! for, built from the room's sample demo so a screenshot of it is the
//! production view.

use ui::presence::PresenceCtx;
use ui::widget::FloatingSurfaceProvider;
use vello::kurbo::{Point, Rect};

use crate::call::CallState;
use crate::model;

use super::{ChatApp, Focus, Hit, MenuAction, MenuTarget};

impl ChatApp {
    /// Stage the view state of screenshot `scenario` over the record its
    /// actions wrote, once the frame is laid out. The demo scenarios read the
    /// design-crit conversation; the default is an ordinary reading frame, and
    /// collaboration states are explicit so a capture does not teach a
    /// floating call as Chat's resting state.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        if let Some(bar) = (self.bar.as_ref()).filter(|_| !matches!(scenario, "" | "channels")) {
            // A capture is of the conversation, on a window that shows one
            // column at a time too.
            bar.stage_pane(ui::widget::NavPane::Content);
        }
        if scenario != "starter" {
            self.active_channel = "c_crit".into();
            // The demo conversation is read by its own person: what they
            // wrote is the reader's.
            if !self.is_me(crate::model::ME) {
                self.former_ids.push(crate::model::ME.into());
            }
        }
        self.sharing_context = scenario == "live-context";
        self.stage_view(scenario);
        self.reselect();
        self.request_relayout();
        self.stage_floating_call(scenario);
    }

    /// The panels, menus and call state of `scenario`.
    #[allow(
        clippy::too_many_lines,
        reason = "the screenshot scenario dispatcher keeps each named visual contract explicit and adjacent"
    )]
    pub(crate) fn stage_view(&mut self, scenario: &str) {
        match scenario {
            "narrow" => {
                // Overlay-collapsed conversation: both drawers closed, no
                // thread, no floating call — the clean single-column reading
                // state.
                if let Some(bar) = &self.bar {
                    bar.stage_sidebar(false);
                }
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
            }
            "history" => {
                // A reader deliberately looking above the newest message. This
                // is the visual-review state for the local live-edge return
                // affordance; no synthetic message or read receipt is made.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.scroll = 220.0;
                self.timeline_at_end = false;
            }
            "compact" => {
                // Medium window: docked channel list, conversation reflowed,
                // members/thread summoned on demand (not forced open), no call.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
            }
            "people" => {
                self.thread_open = None;
                self.right_flap.snap(true);
                self.call = CallState::default();
            }
            "namesakes" => {
                // Two people in the room go by one name: everyone sees them
                // numbered alike, in the timeline and the people list.
                let mut presence = PresenceCtx::empty();
                presence.set_self_participant(
                    ui::permissions::UserId(self.me.id.clone()),
                    self.me.name.clone(),
                );
                for (id, name) in [
                    ("u_aria", "Aria Chen"),
                    ("u_sora", "aria chen"),
                    ("u_milo", "Milo Park"),
                    ("u_devon", "Devon Reyes"),
                    ("u_iris", "Iris Wolfe"),
                ] {
                    presence.set_display_name(ui::permissions::UserId(id.into()), name);
                }
                self.capture_agent_presence = Some(presence);
                self.thread_open = None;
                self.right_flap.snap(true);
                self.call = CallState::default();
            }
            "agent-streaming" | "agent-failed" | "agent-reasoning" | "agent-long-thread" => {
                if matches!(scenario, "agent-reasoning" | "agent-long-thread") {
                    self.expanded_reasoning.insert("shot_agent_stream".into());
                }
                self.thread_open =
                    (scenario == "agent-long-thread").then(|| "shot_agent_stream".into());
                self.right_flap.snap(scenario == "agent-long-thread");
                self.call = CallState::default();
            }
            "agent-work" => {
                self.thread_open = Some("m_agent_request".into());
                self.right_flap.snap(true);
                self.call = CallState::default();
            }
            "thread-narrow" => {
                self.thread_open = Some("m_concept".into());
                self.right_flap.snap(true);
                self.call = CallState::default();
            }
            "collaboration" => {
                self.thread_open = Some("m_concept".into());
                self.right_flap.open();
                self.call.join("c_huddle");
                self.stage_connected_call();
                // Two people in the room's call beside this one, so the
                // sidebar shows them under the voice channel.
                self.native_voice.snapshot.rosters = vec![ui::native_media::Roster {
                    channel_id: "c_huddle".into(),
                    people: vec!["u_aria".to_owned(), "u_milo".to_owned()],
                    ..Default::default()
                }];
                self.refresh_native_streams();
            }
            "lounge" | "lounge-call" | "lounge-video" | "lounge-share" | "lounge-bridged" => {
                // A shared room with people in its voice channels: this
                // person in the Design Huddle with their microphone on, three
                // others beside them and two in the standup, as the room's
                // call rosters report them before anyone opens a call.
                self.thread_open = None;
                self.call.join("c_huddle");
                self.call.muted = false;
                self.call.expanded = scenario != "lounge";
                self.right_flap.snap(false);
                self.stage_connected_call();
                let me = self.me.id.clone();
                let mut huddle = vec![
                    me,
                    "u_aria".to_owned(),
                    "u_milo".to_owned(),
                    "u_sora".to_owned(),
                ];
                if scenario != "lounge" {
                    huddle.push("u_devon".to_owned());
                }
                // Cameras on, and Sora sharing her screen, as the room says.
                let video = !matches!(scenario, "lounge" | "lounge-call" | "lounge-bridged");
                let cameras: Vec<String> = if video {
                    vec![
                        "u_aria".to_owned(),
                        "u_devon".to_owned(),
                        "u_milo".to_owned(),
                    ]
                } else {
                    Vec::new()
                };
                let screens: Vec<String> = if scenario == "lounge-share" {
                    vec!["u_sora".to_owned()]
                } else {
                    Vec::new()
                };
                self.native_voice.snapshot.rosters = vec![
                    ui::native_media::Roster {
                        channel_id: "c_huddle".into(),
                        people: huddle,
                        cameras: cameras.clone(),
                        screens: screens.clone(),
                        ..Default::default()
                    },
                    ui::native_media::Roster {
                        channel_id: "c_standup".into(),
                        people: if scenario == "lounge" {
                            vec!["u_devon".to_owned(), "u_iris".to_owned()]
                        } else {
                            Vec::new()
                        },
                        ..Default::default()
                    },
                ];
                // Aria is talking: her face rings wherever it shows.
                self.native_voice.snapshot.speaking = vec!["u_aria".to_owned()];
                if scenario == "lounge-bridged" {
                    self.stage_bridged_call();
                }
                self.refresh_native_streams();
                // Their pictures: a moving test picture each, coded and
                // decoded exactly as a call's pictures are, so the tiles show
                // real decoded frames.
                for (variant, person) in (0_u8..).zip(&cameras) {
                    self.call_pictures.feed_test_pattern(
                        person,
                        ui::native_media::PictureSource::Camera,
                        (640, 360),
                        variant,
                    );
                }
                for person in &screens {
                    self.call_pictures.feed_test_pattern(
                        person,
                        ui::native_media::PictureSource::Screen,
                        (1280, 720),
                        3,
                    );
                }
                self.refresh_native_streams();
            }
            "starter" => {
                // The real first-run room: `#chat` and `#voice`, no demo content.
                self.thread_open = None;
                self.call = CallState::default();
            }
            "context" => {
                self.menu = Some(self.context_menu(
                    MenuTarget::Channel("c_frontend".into()),
                    Point::new(240.0, 230.0),
                    MenuAction::for_channel().to_vec(),
                ));
            }
            "call" => {
                // The expanded full-window call overlay against the demo
                // huddle (host + three remotes), so the tile grid and the
                // wrapped controls bar can be captured headless at any width.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call.join("c_huddle");
                self.call.expanded = true;
                self.stage_connected_call();
            }
            "floating-call-attached"
            | "floating-call"
            | "floating-call-hover"
            | "floating-call-returning"
            | "floating-call-returned" => {
                // The after-layout shot hook stages the production ownership
                // edge once app-local coordinates are authoritative. All
                // three frames use the real live huddle renderer.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call.join("c_huddle");
                self.call.expanded = false;
                self.stage_connected_call();
            }
            "share" => {
                // The expanded call while screen-sharing: the featured screen
                // tile fills the stage above the participant filmstrip, and the
                // controls bar shows the active "Stop share". Local share is
                // staged directly (the same `call.sharing` flag the real toggle
                // sets on a portal grant) so the presentation layout renders
                // headless without a portal or capture device.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call.join("c_huddle");
                self.call.expanded = true;
                self.call.sharing = true;
                self.stage_connected_call();
            }
            "attach" => {
                // Show the workspace file picker with real, decodable
                // thumbnails (the demo scheme drives the same decode path as
                // a stored image). Ephemeral shot only.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.attach_entries = vec![
                    crate::picker::PickEntry {
                        path: crate::images::DEMO_CALL_GRID.into(),
                        name: "call-overlay-v3.png".into(),
                        size: 84_992,
                        is_video: false,
                    },
                    crate::picker::PickEntry {
                        path: crate::images::DEMO_SHARED_SHOT.into(),
                        name: "design-handoff.png".into(),
                        size: 132_480,
                        is_video: false,
                    },
                ];
                self.attach_open = true;
            }
            "attach-loading" => {
                self.stage_view("attach");
                // This mirrors the actual asynchronous picker state rather
                // than a hand-painted status string: no entries are usable
                // until the worker's generation-checked reply settles.
                self.attach_entries.clear();
                self.attachment_picker_pending = Some(0);
                self.attachment_status = Some("Loading attachments…".into());
            }
            "attach-upload" => {
                // A browser page, where the files to share are the person's
                // own: the chooser has answered and the upload is under way.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.external_services = false;
                self.attachment_status = Some("Uploading quarterly-plan.pdf…".into());
            }
            "attach-error" => {
                self.stage_view("attach");
                self.attachment_status =
                    Some("Not shared: the file is no longer there; choose it again".into());
            }
            "edit" => {
                // The inline message editor open on your own last message,
                // with an earlier message still carrying the static "(edited)"
                // marker — the whole edit story in one frame. Ephemeral shot
                // only: at seed time the messages aren't loaded yet, so the
                // working text is staged directly in the same shared input the
                // live Edit action uses.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                let text = "That's the one. Starting a huddle to walk through the grid — jump in. Moving it to 3pm so the west-coast folks can make it.";
                self.edit_input.set_text(text);
                self.editing = Some("m_me2".into());
                self.focus = Focus::Main;
                self.set_composer_focus();
            }
            "channels" => {
                // The sidebar at rest: unread channels, a mention waiting in
                // another one, and people talking in a voice channel.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                // Three people in the stand-up's call, as their presence
                // says.
                let mut presence = PresenceCtx::empty();
                let standup = Self::voice_presence_key("c_standup");
                for person in ["u_aria", "u_milo", "u_sora"] {
                    presence.set_caret(&standup, ui::permissions::UserId(person.into()), 0);
                }
                self.capture_agent_presence = Some(presence);
            }
            "mention" => {
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                self.focus = Focus::Main;
                self.set_composer_focus();
            }
            "new-channel" => {
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                let mut sheet = crate::channel_sheet::ChannelSheet::new_channel(
                    "cat_acme_main",
                    model::ChannelKind::Text,
                );
                sheet.fill("Release Notes", "Changelogs and launch dates");
                self.open_channel_sheet(sheet);
            }
            "composer" => {
                // The composer mid-compose: a multi-line draft with the caret,
                // so the shared TextInput's text + caret + focus ring can be
                // graded. Thread/call closed to keep the composer prominent.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                self.focus = Focus::Main;
                // The draft itself is the scenario's (`ChatShell::scenario`).
            }
            "emoji" => {
                // The reaction picker over your own last message.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.emoji_open = true;
                self.emoji_target = Some("m_me2".into());
                self.emoji_anchor = Rect::new(560.0, 300.0, 590.0, 330.0);
            }
            "edit-history" => {
                // Every version of an edited message.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.history_open = Some("m_sora".into());
                self.history_anchor = Point::new(424.0, 420.0);
            }
            "msgbar" | "msgbar-shift" | "msgmenu" => {
                // The pointer over one of your own messages: its row lifts
                // under its action bar; with Shift held the bar grows the
                // quick actions that go ahead at once. `msgmenu` then clicks
                // the bar's More, opening the message's whole menu under it
                // ([`crate::resource`]'s scenario input).
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                self.hovered_message = Some("m_me2".into());
                self.message_bar.settle_for_visual_review();
                ui::widget::note_shift(scenario == "msgbar-shift");
            }
            _ => {
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
            }
        }
    }

    /// Show the call as the voice service reports a joined call: connected,
    /// with nobody else's audio arriving yet.
    fn stage_connected_call(&mut self) {
        self.native_voice.channel = Some(self.call.channel.clone());
        self.native_voice.snapshot.state = ui::native_media::State::Live;
        self.native_voice.snapshot.issue = None;
        self.native_voice.snapshot.muted = self.call.muted;
    }

    #[doc(hidden)]
    pub(crate) fn stage_floating_call(&mut self, scenario: &str) {
        if !scenario.starts_with("floating-call") {
            return;
        }

        if scenario == "floating-call-attached" {
            self.call.placement.pin_handoff_for_visual_review(1_000);
            return;
        }

        // A live drag seeds `preferred_size` from the current call tile before
        // crossing the app edge. Derive this staged desktop rect the same way;
        // hard-coding a different size would manufacture a return-time resize
        // that the production pointer path never performs.
        let detached_size = self.call_video_rect().size();
        self.call.placement.set_preferred_size(detached_size);
        // Out over the desktop at the window's trailing side, whole at any
        // window size.
        let r = self.rect;
        let detached = Rect::from_origin_size(
            Point::new(
                (r.x0 + r.width() * 0.6).min(r.x1 - detached_size.width - self.chrome.gutter),
                r.y0 + r.height() * 0.375,
            ),
            detached_size,
        );
        self.call.placement.detach_to_desktop(detached);
        self.call.placement.set_desktop_transfer_covered(true);
        self.call.placement.confirm_desktop_adoption();

        if scenario == "floating-call-hover" {
            self.call_hovered = Some(Hit::CallCamera);
        }

        if let Some(permille) = scenario
            .strip_prefix("floating-call-collapse-")
            .and_then(|value| value.parse::<u16>().ok())
        {
            self.call.participants_expanded = false;
            self.call_chrome_motion
                .pin_for_visual_review(1.0 - f32::from(permille.min(1_000)) / 1_000.0);
        }

        if matches!(
            scenario,
            "floating-call-returning" | "floating-call-returned"
        ) {
            // Grabbed by its title bar and drawn back up and in, over the
            // conversation.
            let pointer = Point::new(
                detached.center().x,
                detached.y0 - self.chrome.call_title_h / 2.0,
            );
            self.call
                .placement
                .begin_drag(pointer, self.rect, self.desktop_floating_bounds());
            let _ = self.call.placement.update_drag(
                Point::new(r.x0 + r.width() * 0.5625, r.y0 + r.height() * 0.2625),
                self.rect,
                self.desktop_floating_bounds(),
            );
        }

        if scenario == "floating-call-returned" {
            let id = format!("chat-call:{}", self.call.channel);
            <Self as FloatingSurfaceProvider>::on_floating_surface_host_message(
                self,
                &ui::floating_surface::FloatingSurfaceHostMessage::Suspended { id },
            );
        }

        // Make the screenshots on either side of shell ownership directly
        // comparable: steady detached state is relaxed; both return frames
        // show the same restrained midpoint deformation.
        let handoff_phase = if matches!(
            scenario,
            "floating-call-returning" | "floating-call-returned"
        ) {
            500
        } else {
            1_000
        };
        self.call
            .placement
            .pin_handoff_for_visual_review(handoff_phase);
    }

    /// The Design Huddle bridged to a Discord voice channel: two people
    /// there in the call beside the room's, one of them talking.
    fn stage_bridged_call(&mut self) {
        let guest = |account: &str, name: &str, avatar: Option<&str>| ui::native_media::Guest {
            id: ui::native_media::Guest::id_of("discord", account),
            name: name.into(),
            avatar: avatar.map(str::to_owned),
            service: "discord".into(),
            linked: None,
            muted: false,
        };
        if let Some(huddle) = self
            .native_voice
            .snapshot
            .rosters
            .iter_mut()
            .find(|roster| roster.channel_id == "c_huddle")
        {
            huddle.guests = vec![
                guest("80351110224678912", "Bo", Some("tilo-demo://avatar-bo")),
                guest("90351110224678912", "Kai", None),
            ];
            huddle.bridges = vec!["discord".into()];
        }
        self.native_voice.snapshot.speaking = vec!["discord:80351110224678912".to_owned()];
    }
}
