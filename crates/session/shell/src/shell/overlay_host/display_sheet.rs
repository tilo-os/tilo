//! The Super+P display sheet ([`tilo_sheets::displays`]) as the shell hosts
//! it: the shell keeps the displays, the change waiting for an answer and
//! the wireless receivers ([`crate::shell::display_layouts`]), tells the app
//! what to show, and carries out what the person chose through the shell's
//! own published display actions.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

use tilo_sheets::displays::{DisplaysEffect, DisplaysSeat, Layout};

use super::sheet_apps::{SheetHost, mount_sheet};
use crate::shell::display_layouts::DisplayRequest;

impl Default for SheetHost<DisplaysSeat> {
    fn default() -> Self {
        Self::new(
            LayerId::DisplayLayouts,
            LayerId::DisplayLayouts.surface_kind(),
            tilo_sheets::displays::definition,
        )
    }
}

impl ShellOverlayHost {
    /// Whether the person may change the shared display layout here.
    fn shared_display_editable(&self) -> bool {
        self.chrome.as_ref().is_some_and(|chrome| {
            crate::shell::search::providers::actions::current_shell_action_authorized(
                &chrome.borrow(),
                crate::shell::shared_display_actions::INCLUDE_ID,
            )
        })
    }

    /// Put the sheet up or take it down as it is asked for, and tell it
    /// what to show. An event that opened it (Super+P, its action) has it
    /// take what it shows now, before anything reads or draws it.
    pub(super) fn follow_display_sheet(&mut self) {
        let asked = self.overlay_flags().projection;
        self.display_sheet.follow_open(asked);
        let host = &self.layers.display_sheet;
        if asked && !host.requested() {
            let room = self.sheet_room();
            let apps = self.sheet_apps();
            self.layers
                .display_sheet
                .mount(&room, |definition, room, instance, seat| {
                    mount_sheet(apps.as_ref(), definition, room, instance, seat)
                });
            self.layers.display_sheet.open();
        } else if !asked && host.is_drawn() {
            self.layers.display_sheet.close_now();
        }
        if asked {
            let sheet = self.display_sheet.sheet(self.shared_display_editable());
            self.layers.display_sheet.seat().show(sheet);
        }
    }

    /// Carry out what the sheet asked, follow the receivers and the
    /// displays, close a sheet whose followed change settled, and keep what
    /// it shows current; whether anything changed on screen.
    pub(super) fn pump_display_sheet(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        let shown = self.layers.display_sheet.update(doc, path);
        let asked = self.layers.display_sheet.seat().take_asked();
        let acted = !asked.is_empty();
        for effect in asked {
            self.carry_out_display_choice(effect);
        }
        let open = self.overlay_flags().projection;
        let polled = self.display_sheet.poll(open) | self.close_settled_projection();
        self.follow_display_sheet();
        // What the shell told the app this frame is shown this frame.
        let told = (acted || polled) && self.layers.display_sheet.update(doc, path);
        shown | acted | polled | told
    }

    fn carry_out_display_choice(&mut self, effect: DisplaysEffect) {
        if let Some(layout) = Layout::of(&effect) {
            if let Some(request) = self.display_sheet.choose_layout(layout) {
                self.invoke_display_request(request);
            }
            return;
        }
        let pin = self.layers.display_sheet.seat().pin();
        let model = &mut self.display_sheet;
        match effect {
            DisplaysEffect::Keep | DisplaysEffect::Revert => {
                if model.may_answer() {
                    self.invoke_display_request(if effect == DisplaysEffect::Keep {
                        DisplayRequest::Keep
                    } else {
                        DisplayRequest::Revert
                    });
                }
            }
            DisplaysEffect::Include {
                device,
                output,
                revision,
                include,
            } => {
                let result = self
                    .chrome
                    .as_ref()
                    .ok_or_else(|| t("display-remote-unavailable"))
                    .and_then(|chrome| {
                        crate::shell::shared_display_actions::include(
                            &chrome.borrow(),
                            &device,
                            &output,
                            revision,
                            include,
                        )
                        .map_err(|error| error.to_string())
                    });
                self.display_sheet.included(result);
            }
            DisplaysEffect::Choose { receiver } => model.choose_receiver(&receiver),
            DisplaysEffect::Connect { receiver } => model.connect(&receiver, pin),
            DisplaysEffect::Stop => model.stop(),
            DisplaysEffect::Search => model.search(),
            DisplaysEffect::ShareAudio { on } => model.share_audio(on),
            DisplaysEffect::AdaptiveQuality { on } => model.adaptive_quality(on),
            DisplaysEffect::NextResolution => model.next_resolution(),
            DisplaysEffect::FrameRate { fps } => model.frame_rate(fps),
            DisplaysEffect::Close => {
                if let Some(chrome) = self.chrome.as_ref() {
                    let mut state = chrome.borrow_mut();
                    state.close_projection();
                    state.bump();
                }
            }
            DisplaysEffect::Extend
            | DisplaysEffect::Mirror
            | DisplaysEffect::BuiltinOnly
            | DisplaysEffect::ExternalOnly => {}
        }
    }

    /// While the sheet is up every event is its own. Once the person
    /// presses a key or a button in it, it is theirs: it no longer closes
    /// when the change it followed settles.
    pub(super) fn route_display_sheet(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.overlay_flags().projection {
            return false;
        }
        self.follow_display_sheet();
        if is_press(event)
            && let Some(chrome) = self.chrome.as_ref()
        {
            chrome.borrow_mut().projection_follow =
                crate::shell::chrome_state::ProjectionFollow::Person;
        }
        self.layers.display_sheet.route(event, ctx);
        *ctx.handled = true;
        true
    }

    /// Make a display choice from the sheet through its published action,
    /// resolved and invoked exactly as the control socket does, so the
    /// person at the sheet and an agent pass one authority check and stage
    /// through one door. The sheet follows the job to its receipt.
    fn invoke_display_request(&mut self, request: DisplayRequest) {
        use crate::shell::search::action_api::{ActionError, AllVisible, ResourceRef};
        // The display actions stage in settings, never in a document.
        let mut doc = automerge::AutoCommit::new();
        let outcome = self
            .chrome
            .clone()
            .ok_or_else(|| ActionError::Refused("this shell has no desktop to act on".into()))
            .and_then(|chrome| {
                let resource = ResourceRef::new(
                    crate::shell::search::providers::actions::PROVIDER_ID,
                    request.action_id(),
                );
                // Visibility is not authority: the action itself checks the
                // workspace's grant when it is invoked, as for every caller.
                let catalog = self.action_catalog_for_resource(
                    &resource,
                    &chrome.borrow(),
                    &doc,
                    &automerge::ROOT,
                    &AllVisible,
                );
                let selected = catalog.select(&resource).ok_or_else(|| {
                    ActionError::Refused(format!("{} is not published", request.action_id()))
                })?;
                selected.invoke_nullary(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                    &chrome,
                )
            });
        self.display_sheet.follow_request(request, outcome);
    }

    /// Close the display sheet a `display.layout.*` action opened once the
    /// change it shows is kept, reverted, refused or replaced, so an agent
    /// or script that answered it leaves no modal sheet holding the
    /// keyboard. A sheet the person opened or used is left alone. Returns
    /// whether the sheet closed.
    fn close_settled_projection(&mut self) -> bool {
        use crate::shell::chrome_state::ProjectionFollow;
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let (open, follow) = {
            let state = chrome.borrow();
            (state.overlays.projection, state.projection_follow)
        };
        match follow {
            ProjectionFollow::Transaction(id) if open => {
                if !self
                    .display_sheet
                    .followed_change_settled(id, Instant::now())
                {
                    return false;
                }
                let mut state = chrome.borrow_mut();
                state.close_projection();
                state.bump();
                true
            }
            ProjectionFollow::Person => false,
            // Closed some other way: nothing is followed any more.
            _ if !open => {
                chrome.borrow_mut().projection_follow = ProjectionFollow::Person;
                false
            }
            _ => false,
        }
    }

    /// Whether the sheet needs another frame: discovery or a handshake in
    /// flight, the Keep/Revert countdown, or a followed change that read as
    /// settled, which one more host update closes.
    pub(super) fn display_sheet_needs_frame(&self, now: Instant) -> bool {
        let open = self.overlay_flags().projection;
        let settled = self.chrome.as_ref().is_some_and(|chrome| {
            chrome.try_borrow().is_ok_and(|state| {
                matches!(
                    state.projection_follow,
                    crate::shell::chrome_state::ProjectionFollow::Transaction(id)
                        if state.overlays.projection
                            && self.display_sheet.followed_change_read_as_settled(id)
                )
            })
        });
        self.display_sheet.needs_frame(open)
            || settled
            || self.layers.display_sheet.is_animating(now)
    }

    /// The sheet as assistive technology reads it, once its app drew it:
    /// the dialog, with what a reader meets in it after it, in order.
    pub fn display_sheet_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        if !self.layers.display_sheet.is_drawn() {
            return None;
        }
        let mut order = ui::flatten_reading_order(self.layers.display_sheet.widget()?).into_iter();
        let mut dialog = order
            .by_ref()
            .find(|node| node.role == ui::a11y::Role::Dialog)?;
        dialog.children = order.filter(|node| !node.state.hidden).collect();
        Some(dialog)
    }

    /// Where the sheet stands while it asks for an `AirPlay` PIN or
    /// password: a capture draws it as the placeholder.
    pub(super) fn display_sheet_secret_rect(&self) -> Option<Rect> {
        let host = &self.layers.display_sheet;
        if !host.is_drawn() || !host.seat().sheet().asks_pin {
            return None;
        }
        let dialog = ui::flatten_reading_order(host.widget()?)
            .into_iter()
            .find(|node| node.role == ui::a11y::Role::Dialog)
            .and_then(|node| node.bounds);
        Some(dialog.map_or_else(|| host.area(), |bounds| bounds.to_rect().inflate(2.0, 2.0)))
    }

    pub fn projection(&self) -> &crate::shell::projection::ProjectionController {
        self.display_sheet.controller()
    }

    /// Whether a receiver session is being established or is live. The
    /// compositor uses this to turn the frame encoder on only while needed.
    pub fn projection_capture_requested(&self) -> bool {
        self.display_sheet.capture_requested()
    }
}
