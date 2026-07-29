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

use automerge::{transaction::Transactable, AutoCommit, ObjId, ReadDoc, Value};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Toggle, Widget};
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

use crate::prefs::{NotificationPrefs, SystemPrefs};
use crate::widgets::{card, caption, rows, setting_row_described, status_pill, El};

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

    fn toggle(id: &str, key: &str) -> El {
        Box::new(Toggle::new(id, key).with_id(id))
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
            let seed = |state: &mut AutoCommit, key: &str, v: bool| {
                let _ = state.put(root, key, automerge::ScalarValue::Boolean(v));
            };
            seed(state, DND_KEY, self.prefs.do_not_disturb);
            seed(state, BANNERS_KEY, self.prefs.show_banners);
            seed(state, PREVIEWS_KEY, self.prefs.show_previews);
            seed(state, SOUNDS_KEY, self.prefs.play_sounds);
            seed(state, LOCK_SCREEN_KEY, self.prefs.show_on_lock_screen);
            self.needs_seed = false;
        }

        let mut changed = false;
        let mut adopt = |cur: &mut bool, v: Option<bool>| {
            if let Some(b) = v {
                if b != *cur {
                    *cur = b;
                    changed = true;
                }
            }
        };
        adopt(&mut self.prefs.do_not_disturb, Self::read_bool(state, root, DND_KEY));
        adopt(&mut self.prefs.show_banners, Self::read_bool(state, root, BANNERS_KEY));
        adopt(&mut self.prefs.show_previews, Self::read_bool(state, root, PREVIEWS_KEY));
        adopt(&mut self.prefs.play_sounds, Self::read_bool(state, root, SOUNDS_KEY));
        adopt(
            &mut self.prefs.show_on_lock_screen,
            Self::read_bool(state, root, LOCK_SCREEN_KEY),
        );

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
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node) {
                if let Some(&cn) = children.first() {
                    c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
                }
            }
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
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&cn) = children.first() {
                    c.handle_event(
                        event, scale_factor, modifiers, state, path, handled, focus_target, cn,
                        taffy,
                    );
                }
            }
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

    fn name(&self) -> &str {
        "NotificationsPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
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
        assert_eq!(NotificationsPane::read_bool(&doc, &root, DND_KEY), Some(true));
        assert_eq!(NotificationsPane::read_bool(&doc, &root, BANNERS_KEY), Some(false));
        assert_eq!(NotificationsPane::read_bool(&doc, &root, LOCK_SCREEN_KEY), Some(true));
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
        assert!(SystemPrefs::load().notifications.do_not_disturb, "persisted");
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
