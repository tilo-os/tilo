//! Notifications pane — the editor for the **shared notification policy**.
//!
//! This panel edits [`crate::prefs::NotificationPrefs`], the durable
//! notification-delivery section of the shared `settings.json`
//! ([`crate::region::RegionSettings::store_path`]). The compositor's
//! notification layer reads that section when it decides whether to raise a
//! banner, reveal its contents, sound it, or mirror it to the lock screen — so
//! the toggles here are the single source of truth the rest of the system
//! follows, exactly like Appearance owns the theme.
//!
//! Controls (each bound to a CRDT key; `sync` folds the change into the loaded
//! prefs and persists the shared store):
//! - **Do Not Disturb** — master mute for banners and sounds.
//! - **Show banners** — pop-up banners for incoming notifications.
//! - **Show previews** — reveal the message body (off shows only the app name).
//! - **Play sounds** — an audible cue when a notification arrives.
//! - **Show on lock screen** — mirror notifications onto the lock screen.

use std::collections::HashMap;

use automerge::{AutoCommit, ChangeHash, ObjId, ReadDoc, Value, transaction::Transactable};
use qos_actions::EncodedAction;
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::actions::{SETTINGS_ACTION_TYPE, SettingsAction, set_bool_action};
use crate::prefs::{NotificationPrefs, SystemPrefs};
use crate::widgets::{El, caption, card, rows, setting_row_described, status_pill};

pub const DND_KEY: &str = "notifications.do_not_disturb";
pub const BANNERS_KEY: &str = "notifications.show_banners";
pub const PREVIEWS_KEY: &str = "notifications.show_previews";
pub const SOUNDS_KEY: &str = "notifications.play_sounds";
pub const LOCK_SCREEN_KEY: &str = "notifications.show_on_lock_screen";

pub struct NotificationsPane {
    prefs: NotificationPrefs,
    needs_seed: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
    /// Framework-owned action dispatch: brackets the undo boundary (bubbled up by
    /// `SettingsApp` via [`Widget::take_committed_action`], so the host records
    /// exactly one boundary per event), encodes and dispatches each settings
    /// action, and remembers the last one so a test can prove an on-screen toggle
    /// builds the same [`EncodedAction`] the palette does.
    actions: ui::ActionHost,
}

impl Default for NotificationsPane {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationsPane {
    pub fn new() -> Self {
        Self::with_prefs(SystemPrefs::load().notifications)
    }

    /// Construct from explicit preferences — used by tests and the
    /// deterministic `--shots` render.
    pub fn with_prefs(prefs: NotificationPrefs) -> Self {
        Self {
            prefs,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
            actions: ui::ActionHost::new(),
        }
    }

    /// The most recently dispatched settings action's wire form (test hook).
    pub fn last_dispatched(&self) -> Option<&EncodedAction> {
        self.actions.last_dispatched()
    }

    /// The scratch (edit-intent) key a toggle binds to for a given canonical
    /// setting key. The widget writes the scratch; `sync` reconciles it into the
    /// canonical value through an action, and mirrors the canonical value back
    /// into the scratch when it changes underneath us (undo / remote / palette).
    fn scratch_key(canonical: &str) -> String {
        format!("{canonical}.edit")
    }

    /// Dispatch one [`SettingsAction`] — the single CRDT-mutation path shared
    /// with the Ctrl+K palette. The framework [`ActionHost`](ui::ActionHost)
    /// brackets the undo boundary and applies the action through the qOS executor
    /// (a pinned actor + committed change, so it replays byte-identically on
    /// peers).
    ///
    /// This is *only* the CRDT write. Carrying the preference to the compositor's
    /// notification layer (the shared `settings.json`, see [`Self::persist`]) is a
    /// separate side effect driven by the resulting document state — never part
    /// of an action.
    fn perform_action(&mut self, state: &mut AutoCommit, action: SettingsAction) {
        self.actions.dispatch(state, SETTINGS_ACTION_TYPE, &action);
    }

    /// Reconcile one boolean setting between its widget scratch key and its
    /// canonical (action-owned) key. Returns `(new_value, changed)`.
    ///
    /// * canonical changed under us (undo / remote / palette) → adopt it and
    ///   mirror it into the scratch so the toggle reflects it;
    /// * else the user flipped the toggle (scratch ≠ canonical) → dispatch the
    ///   action that writes the canonical value.
    fn reconcile_bool(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        canonical: &str,
        field: bool,
    ) -> (bool, bool) {
        let scratch = Self::scratch_key(canonical);
        let cv = Self::read_bool(state, root, canonical).unwrap_or(field);
        let sv = Self::read_bool(state, root, &scratch).unwrap_or(cv);
        if cv != field {
            let _ = state.put(root, scratch.as_str(), automerge::ScalarValue::Boolean(cv));
            (cv, true)
        } else if sv != cv {
            self.perform_action(state, set_bool_action(canonical, sv));
            (sv, true)
        } else {
            (field, false)
        }
    }

    pub fn prefs(&self) -> &NotificationPrefs {
        &self.prefs
    }

    /// A one-word delivery status, so the effect of the toggles reads at a
    /// glance without re-parsing every row.
    fn status(&self) -> (&'static str, vello::peniko::Color) {
        let t = Theme::dark();
        if self.prefs.do_not_disturb {
            ("Silenced", t.text_dim)
        } else if !self.prefs.show_banners {
            ("Banners off", t.tokens.colors.semantic.warning)
        } else {
            ("Active", t.status_success)
        }
    }

    fn toggle(id: &str, canonical: &str) -> El {
        // The toggle writes the scratch (edit-intent) key; `sync` turns that
        // edit into an action on the canonical key. The palette dispatches the
        // same action on the same canonical key — one source of truth.
        Box::new(Toggle::new(id, &Self::scratch_key(canonical)).with_id(id))
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let (status_label, status_tint) = self.status();

        // A label-left / status-pill-right row, so the combined effect of the
        // toggles is legible at the top of the pane.
        let status_row: El = Box::new(
            ui::widget::Row::new()
                .push(Box::new(ui::widget::Text::new("Delivery").with_size(13.0)))
                .push(Box::new(ui::widget::Spacer::new()))
                .push(status_pill(status_label, status_tint)),
        );

        // Master Do Not Disturb, with the visible delivery-status pill.
        let dnd_card = card(
            "Do Not Disturb",
            rows(vec![
                status_row,
                setting_row_described(
                    "Do Not Disturb",
                    "Silence all banners and sounds until you turn it off",
                    Self::toggle("notifications.dnd", DND_KEY),
                ),
            ]),
        );

        let banners_card = card(
            "Banners",
            rows(vec![
                setting_row_described(
                    "Show banners",
                    "Pop up a banner when a notification arrives",
                    Self::toggle("notifications.banners", BANNERS_KEY),
                ),
                setting_row_described(
                    "Show message previews",
                    "Reveal the message body in the banner, not just the app",
                    Self::toggle("notifications.previews", PREVIEWS_KEY),
                ),
            ]),
        );

        let delivery_card = card(
            "Sound & lock screen",
            rows(vec![
                setting_row_described(
                    "Play sounds",
                    "An audible cue when a notification arrives",
                    Self::toggle("notifications.sounds", SOUNDS_KEY),
                ),
                setting_row_described(
                    "Show on lock screen",
                    "Mirror notifications while the session is locked",
                    Self::toggle("notifications.lock_screen", LOCK_SCREEN_KEY),
                ),
            ]),
        );

        let explainer = card(
            "How this applies",
            rows(vec![caption(
                "These choices are read by the compositor's notification layer, \
                 so every qOS app follows the same delivery policy. Do Not \
                 Disturb overrides the individual banner and sound toggles.",
            )]),
        );

        crate::widgets::cards(vec![dnd_card, banners_card, delivery_card, explainer])
    }

    /// Persist the current prefs into the shared store, preserving other
    /// sections. Reads fresh so a change here never clobbers another pane's.
    fn persist(&self) {
        let mut p = SystemPrefs::load();
        p.notifications = self.prefs;
        if let Err(e) = p.save() {
            warn!(target: "qos::settings", op = "notifications_save", error = %e, "failed to persist notification prefs");
        }
    }

    fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
        match state.get(root, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                automerge::ScalarValue::Boolean(b) => Some(*b),
                _ => None,
            },
            _ => None,
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(NotificationsPane::new())
}

impl Widget for NotificationsPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            // Seed both the canonical (action-owned) key and the scratch
            // (widget-bound) key, so the toggle shows the current value and the
            // reconciliation starts settled.
            let seed = |state: &mut AutoCommit, canonical: &str, v: bool| {
                let _ = state.put(root, canonical, automerge::ScalarValue::Boolean(v));
                let _ = state.put(
                    root,
                    Self::scratch_key(canonical).as_str(),
                    automerge::ScalarValue::Boolean(v),
                );
            };
            seed(state, DND_KEY, self.prefs.do_not_disturb);
            seed(state, BANNERS_KEY, self.prefs.show_banners);
            seed(state, PREVIEWS_KEY, self.prefs.show_previews);
            seed(state, SOUNDS_KEY, self.prefs.play_sounds);
            seed(state, LOCK_SCREEN_KEY, self.prefs.show_on_lock_screen);
            self.needs_seed = false;
        }

        // Reconcile every toggle's scratch → canonical through an action (a user
        // flip) or mirror the canonical → scratch (an undo / remote / palette
        // change). Each binding is (canonical key, current field value).
        let mut changed = false;
        let (v, c) = self.reconcile_bool(state, root, DND_KEY, self.prefs.do_not_disturb);
        self.prefs.do_not_disturb = v;
        changed |= c;
        let (v, c) = self.reconcile_bool(state, root, BANNERS_KEY, self.prefs.show_banners);
        self.prefs.show_banners = v;
        changed |= c;
        let (v, c) = self.reconcile_bool(state, root, PREVIEWS_KEY, self.prefs.show_previews);
        self.prefs.show_previews = v;
        changed |= c;
        let (v, c) = self.reconcile_bool(state, root, SOUNDS_KEY, self.prefs.play_sounds);
        self.prefs.play_sounds = v;
        changed |= c;
        let (v, c) =
            self.reconcile_bool(state, root, LOCK_SCREEN_KEY, self.prefs.show_on_lock_screen);
        self.prefs.show_on_lock_screen = v;
        changed |= c;

        if changed {
            self.persist();
        }

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "NotificationsPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Hand the host the heads captured before an on-screen toggle dispatched its
    /// settings action, so it records one undo boundary for it (bubbled up to the
    /// root by `SettingsApp`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_writes_current_prefs_into_crdt() {
        let mut pane = NotificationsPane::with_prefs(NotificationPrefs {
            do_not_disturb: true,
            show_banners: false,
            show_previews: true,
            play_sounds: false,
            show_on_lock_screen: true,
        });
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(
            NotificationsPane::read_bool(&doc, &root, DND_KEY),
            Some(true)
        );
        assert_eq!(
            NotificationsPane::read_bool(&doc, &root, BANNERS_KEY),
            Some(false)
        );
        assert_eq!(
            NotificationsPane::read_bool(&doc, &root, LOCK_SCREEN_KEY),
            Some(true)
        );
    }

    #[test]
    fn toggling_dnd_updates_and_persists_prefs() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("notifications_dnd");
        let mut pane = NotificationsPane::with_prefs(NotificationPrefs::default());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert!(!pane.prefs().do_not_disturb);
        // User flips DND on.
        let _ = doc.put(&root, DND_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        assert!(pane.prefs().do_not_disturb, "the pane adopted the toggle");
        // And it persisted to the shared store (loaded back through the store).
        assert!(
            SystemPrefs::load().notifications.do_not_disturb,
            "persisted"
        );
    }

    /// Single source of truth: an on-screen toggle flip dispatches the exact
    /// same [`EncodedAction`] the Ctrl+K palette's flip spec builds — proven at
    /// the same pre-flip document state so both target the same value.
    #[test]
    fn on_screen_toggle_dispatches_same_action_as_palette() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("notifications_affordance");
        let mut pane = NotificationsPane::with_prefs(NotificationPrefs::default()); // DND off
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root); // seeds both keys; nothing to dispatch
        assert!(
            pane.last_dispatched().is_none(),
            "seeding dispatches no action"
        );

        // The palette's DND flip spec, read at the pre-flip state (DND off).
        let app = crate::SettingsApp::new();
        let res = Widget::resources(&app, &doc);
        let notif = res[0]
            .children
            .iter()
            .find(|r| r.id.as_str() == "settings.section:notifications")
            .unwrap();
        let dnd_spec = notif.actions.iter().find(|a| a.id == "dnd").unwrap();
        let from_spec = match &dnd_spec.input {
            qos_actions::ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("the DND flip is an immediate action"),
        };

        // Simulate the Toggle writing its scratch key on a flip, then sync.
        let scratch = NotificationsPane::scratch_key(DND_KEY);
        let _ = doc.put(
            &root,
            scratch.as_str(),
            automerge::ScalarValue::Boolean(true),
        );
        pane.sync(&mut doc, &root);

        let dispatched = pane
            .last_dispatched()
            .expect("the flip dispatched an action")
            .clone();
        let from_builder = crate::actions::encode(&crate::actions::set_bool_action(DND_KEY, true));
        assert_eq!(
            dispatched, from_builder,
            "on-screen flip == the shared builder"
        );
        assert_eq!(
            dispatched, from_spec,
            "on-screen flip == the palette ActionSpec"
        );

        // The action landed on the canonical key and the pane adopted + persisted it.
        assert_eq!(
            NotificationsPane::read_bool(&doc, &root, DND_KEY),
            Some(true)
        );
        assert!(pane.prefs().do_not_disturb);
    }

    /// The full undo covenant on the on-screen path: a toggle flip dispatched
    /// through the executor records exactly one undo boundary, and Ctrl+Z reverts
    /// precisely that setting — after which the pane mirrors the reverted value
    /// back into the toggle.
    #[test]
    fn on_screen_toggle_records_one_undo_boundary_and_reverts() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("notifications_undo");
        let mut pane = NotificationsPane::with_prefs(NotificationPrefs::default());
        let mut state = ui::State::new();
        pane.sync(&mut state.doc, &state.root); // seed

        // Flip the toggle's scratch key, then sync → the pane dispatches.
        let scratch = NotificationsPane::scratch_key(DND_KEY);
        let _ = state.doc.put(
            &state.root,
            scratch.as_str(),
            automerge::ScalarValue::Boolean(true),
        );
        pane.sync(&mut state.doc, &state.root);

        // The host's own recorder drains the pane's captured bracket and records
        // the boundary. Going through it rather than pushing an entry by hand is
        // the point: attribution happens in there, and undo is author-scoped.
        // The node map is empty because the pane is the root widget here.
        let local_author = ui::collaboration::local_user_id();
        let root: *mut dyn ui::widget::Widget = &mut pane;
        assert!(
            ui::undo::record_dispatched_action(
                &mut state,
                root,
                &std::collections::HashMap::new(),
                &local_author,
            ),
            "the host recorded an undo boundary for the dispatched flip"
        );
        assert_eq!(
            NotificationsPane::read_bool(&state.doc, &state.root, DND_KEY),
            Some(true),
            "the action set DND on"
        );
        assert!(pane.prefs().do_not_disturb);
        let entry = state
            .peek_last_undo()
            .expect("exactly one undo boundary recorded");
        assert_eq!(
            entry.author.as_ref(),
            Some(&local_author),
            "attributed to the local user, so their Ctrl+Z can reach it"
        );
        assert!(
            pane.take_committed_action().is_none(),
            "the bracket is rising-edge"
        );

        // Ctrl+Z reverts precisely the setting.
        state
            .undo_as(Some(&local_author), None, None)
            .expect("an undo entry to pop");
        assert_eq!(
            NotificationsPane::read_bool(&state.doc, &state.root, DND_KEY),
            Some(false),
            "undo reverted the setting"
        );
        // Next frame the pane mirrors the reverted canonical back into the scratch
        // and adopts it, so the toggle follows — and it does not re-dispatch.
        pane.sync(&mut state.doc, &state.root);
        assert!(!pane.prefs().do_not_disturb, "the pane adopted the revert");
        assert_eq!(
            NotificationsPane::read_bool(&state.doc, &state.root, scratch.as_str()),
            Some(false),
            "the toggle's scratch mirrors the reverted value"
        );
        assert!(
            pane.take_committed_action().is_none(),
            "mirroring the revert dispatches nothing"
        );
    }

    #[test]
    fn status_reflects_the_toggle_combination() {
        let active = NotificationsPane::with_prefs(NotificationPrefs::default());
        assert_eq!(active.status().0, "Active");
        let silenced = NotificationsPane::with_prefs(NotificationPrefs {
            do_not_disturb: true,
            ..NotificationPrefs::default()
        });
        assert_eq!(silenced.status().0, "Silenced");
        let banners_off = NotificationsPane::with_prefs(NotificationPrefs {
            show_banners: false,
            ..NotificationPrefs::default()
        });
        assert_eq!(banners_off.status().0, "Banners off");
    }

    #[test]
    fn builds_layout() {
        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }
}
