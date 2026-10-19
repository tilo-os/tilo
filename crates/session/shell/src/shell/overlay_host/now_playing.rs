//! The now-playing card beside the Quick Settings root page.
//!
//! The card is its own Panel, attached under the popover (or beside it when
//! the display is too short), opened and closed with it and hit-tested in
//! the same entrance transform. Its model comes from a
//! [`NowPlayingSource`](crate::shell::now_playing::NowPlayingSource); outputs
//! come from the device fabric's route targets, named by their continuity
//! tokens.

use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;

use ui::text_render::TextRenderer;
use ui::theme::Theme;

use super::{DeviceFabricUiAction, ShellOverlayHost};
use crate::shell::now_playing::{NowPlayingView, output_targets, resolve_output, tr};
use crate::shell::quick_settings::QsLevel;
use crate::shell::quick_settings::now_playing::CardAction;
use tilo_desktop::drill_in::{PANEL_PAD, SIBLING_GAP};

/// How often an open card re-reads players and advances its playhead.
const REFRESH: Duration = Duration::from_secs(1);

/// The MPRIS bus behind a portable fabric media session.
fn controller_of(session_id: &str) -> Option<String> {
    media::handoff::playback_report(session_id).and_then(|report| report.controller_bus_name)
}

impl ShellOverlayHost {
    /// Whether the popover is open on its root page, the only page the card
    /// accompanies.
    fn now_playing_visible(&self) -> bool {
        self.chrome.as_ref().is_some_and(|chrome| {
            let state = chrome.borrow();
            state.overlays.quick_settings
                && matches!(state.quick_settings.current_level(), QsLevel::Root)
        })
    }

    /// This frame's model: `None` when the popover is elsewhere or nothing
    /// is playing or paused.
    pub(super) fn now_playing_view(&self) -> Option<NowPlayingView> {
        if !self.now_playing_visible() {
            return None;
        }
        let players = self.now_playing_source.players();
        let preferred = self.now_playing_preferred.as_deref();
        let active = crate::shell::now_playing::active_index(&players, preferred)?;
        let bus = players.get(active).map(|player| player.bus_name.clone());
        let fabric = self.device_fabric_call(DeviceFabricUiAction::Snapshot);
        let outputs = fabric
            .as_ref()
            .map(|view| output_targets(view, bus.as_deref(), &controller_of))
            .unwrap_or_default();
        let speakers = fabric
            .as_ref()
            .ok()
            .and_then(crate::shell::now_playing::SpeakerPlayback::from_view);
        NowPlayingView::new(players, preferred, outputs, Instant::now())
            .map(|view| view.with_speakers(speakers))
    }

    /// Where the card sits: under the popover when it fits, else beside it,
    /// else nowhere — it never covers the popover or leaves the display.
    pub(super) fn now_playing_rect(&self, view: &NowPlayingView) -> Option<Rect> {
        let qs = self.quick_settings_rect();
        let height = self.now_playing_card.preferred_height(view);
        let below = Rect::new(
            qs.x0,
            qs.y1 + SIBLING_GAP,
            qs.x1,
            qs.y1 + SIBLING_GAP + height,
        );
        let area = self.overlay_area();
        if below.y1 <= area.y1 - PANEL_PAD {
            return Some(below);
        }
        let beside = Rect::new(
            qs.x0 - SIBLING_GAP - qs.width(),
            qs.y0,
            qs.x0 - SIBLING_GAP,
            qs.y0 + height,
        );
        (beside.x0 >= area.x0 + PANEL_PAD && beside.y1 <= area.y1 - PANEL_PAD).then_some(beside)
    }

    /// The card's laid-out rect and model together, for input and semantics.
    pub(super) fn now_playing_layout(&mut self) -> Option<(NowPlayingView, Rect)> {
        let view = self.now_playing_view()?;
        let rect = self.now_playing_rect(&view)?;
        self.now_playing_card.layout(rect);
        Some((view, rect))
    }

    /// The popover opened or closed: forget card focus and pickers, and ask
    /// for a fresh observation so the card is current when it appears.
    pub(super) fn reconcile_now_playing_open_edge(&mut self, opened: bool) {
        self.now_playing_card.reset();
        if opened {
            self.now_playing_source.refresh();
        }
    }

    /// Keep the model fresh while the popover is open.
    pub(super) fn refresh_now_playing(&self) {
        if self.now_playing_visible() {
            self.now_playing_source.refresh();
        }
    }

    /// While the card is visible, wake once a second so a playing track's
    /// position moves and changes made in the player itself appear.
    pub(super) fn now_playing_wake_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.now_playing_visible() || self.now_playing_source.players().is_empty() {
            self.now_playing_tick.set(None);
            return None;
        }
        // A fixed deadline, advanced only when the card paints: a deadline
        // recomputed from `now` on every poll would never come due.
        let tick = self
            .now_playing_tick
            .get()
            .or_else(|| now.checked_add(REFRESH));
        self.now_playing_tick.set(tick);
        tick
    }

    /// Paint the card, Panel surface included, in the popover's transform.
    /// Returns the rect painted.
    pub(super) fn draw_now_playing(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<Rect> {
        let view = self.now_playing_view()?;
        let rect = self.now_playing_rect(&view)?;
        if self
            .now_playing_tick
            .get()
            .is_none_or(|tick| tick <= view.now)
        {
            self.now_playing_tick.set(view.now.checked_add(REFRESH));
        }
        self.now_playing_card.set_text_scale(theme.text_scale);
        let radius = ui::chrome::ChromeRole::Panel.radius(theme, rect.height());
        ui::surface::shadow(
            scene,
            transform,
            rect,
            radius,
            &theme.tokens.elevation.floating,
        );
        Self::draw_quick_settings_card_background(scene, transform, theme, rect);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &RoundedRect::from_rect(rect, radius),
        );
        self.now_playing_card.draw(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            self.last_cursor,
            &view,
        );
        scene.pop_layer();
        Some(rect)
    }

    /// A click in popover layout coordinates. `true` when the card took it.
    pub(super) fn handle_now_playing_click(&mut self, point: Point) -> bool {
        let Some((view, rect)) = self.now_playing_layout() else {
            return false;
        };
        if !rect.contains(point) {
            return false;
        }
        let action = self.now_playing_card.click(&view, point);
        self.apply_now_playing_action(action, &view);
        true
    }

    /// Keyboard traversal into, through and out of the card. Tab from the
    /// root page's last control enters the card; Tab past the card's last
    /// control returns to the root page's first. `true` when handled.
    pub(super) fn handle_now_playing_tab(&mut self, backwards: bool) -> bool {
        let Some((view, _)) = self.now_playing_layout() else {
            return false;
        };
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        if self.now_playing_card.has_focus() {
            if self.now_playing_card.focus_next(&view, backwards) {
                chrome.borrow_mut().bump();
            } else {
                let mut state = chrome.borrow_mut();
                self.qs_root.focus_next(&state.quick_settings, backwards);
                state.bump();
            }
            return true;
        }
        let wraps = self
            .qs_root
            .focus_would_wrap(&chrome.borrow().quick_settings, backwards);
        if !wraps {
            return false;
        }
        self.qs_root.reset_focus();
        self.now_playing_card.focus_next(&view, backwards);
        chrome.borrow_mut().bump();
        true
    }

    /// Enter/Space and Left/Right while the card holds focus.
    pub(super) fn handle_now_playing_key(&mut self, activate: bool, forward: bool) -> bool {
        if !self.now_playing_card.has_focus() {
            return false;
        }
        let Some((view, _)) = self.now_playing_layout() else {
            return false;
        };
        let action = if activate {
            self.now_playing_card.activate_focused(&view)
        } else {
            self.now_playing_card.adjust_focused(&view, forward)
        };
        self.apply_now_playing_action(action, &view);
        true
    }

    pub(super) fn apply_now_playing_action(&mut self, action: CardAction, view: &NowPlayingView) {
        match action {
            CardAction::None => return,
            CardAction::Redraw => {}
            CardAction::Command { bus_name, command } => {
                if !self.now_playing_source.send(&bus_name, command) {
                    tracing::debug!(target: "tilo::mpris", "now-playing command not dispatched");
                }
            }
            CardAction::SelectPlayer(bus_name) => {
                self.now_playing_preferred = Some(bus_name);
            }
            CardAction::Route(token) => {
                let bus = view.player().bus_name.clone();
                // Resolve against a fresh snapshot: a picker opened a minute
                // ago must not route to a destination that has since gone.
                let resolved = self
                    .device_fabric_call(DeviceFabricUiAction::SnapshotFresh)
                    .ok()
                    .and_then(|fresh| resolve_output(&fresh, &token, Some(&bus), &controller_of));
                let label = view
                    .outputs
                    .iter()
                    .find(|output| output.token == token)
                    .map(|output| output.label.clone())
                    .unwrap_or_default();
                match resolved {
                    Some(action) => self.finish_qs_device_action(action, label),
                    None => {
                        if let Some(chrome) = self.chrome.as_ref() {
                            chrome
                                .borrow_mut()
                                .push_notification(tr("now-playing-route-gone"), label);
                        }
                    }
                }
            }
        }
        self.bump_chrome();
    }

    /// Named geometry for `tilo-control list-targets`.
    pub(super) fn now_playing_semantic_rects(&self) -> Vec<(String, Rect)> {
        use crate::shell::quick_settings::now_playing::{CardTarget, Transport};
        let Some(view) = self.now_playing_view() else {
            return Vec::new();
        };
        let Some(rect) = self.now_playing_rect(&view) else {
            return Vec::new();
        };
        let mut rects = vec![("now_playing".to_string(), rect)];
        rects.extend(
            self.now_playing_card
                .laid_out(rect)
                .targets(&view)
                .into_iter()
                .map(|(target, rect)| {
                    let name = match target {
                        CardTarget::Seek => "now_playing_seek".to_string(),
                        CardTarget::Players => "now_playing_players".to_string(),
                        CardTarget::Outputs => "now_playing_outputs".to_string(),
                        CardTarget::Transport(Transport::Previous) => {
                            "now_playing_previous".to_string()
                        }
                        CardTarget::Transport(Transport::PlayPause) => {
                            "now_playing_play_pause".to_string()
                        }
                        CardTarget::Transport(Transport::Next) => "now_playing_next".to_string(),
                        CardTarget::Player(index) => format!("now_playing_player[{index}]"),
                        CardTarget::Output(index) => format!("now_playing_output[{index}]"),
                    };
                    (name, rect)
                }),
        );
        rects
    }

    pub(super) fn now_playing_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let view = self.now_playing_view()?;
        let rect = self.now_playing_rect(&view)?;
        Some(
            self.now_playing_card
                .laid_out(rect)
                .accessibility_snapshot(&view),
        )
    }

    pub(super) fn now_playing_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let view = self.now_playing_view()?;
        let rect = self.now_playing_rect(&view)?;
        self.now_playing_card
            .laid_out(rect)
            .rect_for_path(&view, path)
    }

    pub(super) fn focus_now_playing_accessibility(&mut self, path: &[usize]) -> bool {
        let Some((view, _)) = self.now_playing_layout() else {
            return false;
        };
        if path.is_empty() {
            return true;
        }
        let focused = self.now_playing_card.focus_path(&view, path);
        if focused {
            self.qs_root.reset_focus();
        }
        focused
    }

    /// Whether the accessibility tree publishes the card this frame.
    pub(super) fn now_playing_published(&self) -> bool {
        self.now_playing_view()
            .is_some_and(|view| self.now_playing_rect(&view).is_some())
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use winit::keyboard::{Key, NamedKey};

    use super::super::LayerId;
    use super::*;
    use crate::shell::chrome_state::{Chrome, ChromeState};
    use crate::shell::now_playing::tests::{FakeSource, player};
    use crate::shell::now_playing::{PlaybackStatus, PlayerCommand, PlayerSnapshot};
    use crate::shell::state::WorkspaceTree;
    use ui::widget::{Container, Widget as _};

    fn open_host(players: Vec<PlayerSnapshot>) -> (ShellOverlayHost, Chrome, Rc<FakeSource>) {
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(chrome.clone());
        host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
        let source = Rc::new(FakeSource {
            players,
            ..FakeSource::default()
        });
        host.now_playing_source = source.clone();
        chrome.borrow_mut().overlays.quick_settings = true;
        host.reconcile_quick_settings_open_edge(true);
        host.force_quick_settings_settled();
        (host, chrome, source)
    }

    fn tab(host: &mut ShellOverlayHost, backwards: bool) {
        let mut handled = false;
        assert!(host.handle_quick_settings_keyboard(
            &Key::Named(NamedKey::Tab),
            backwards,
            false,
            &mut handled,
        ));
        assert!(handled);
    }

    fn target(host: &ShellOverlayHost, name: &str) -> Rect {
        host.now_playing_semantic_rects()
            .into_iter()
            .find(|(candidate, _)| candidate == name)
            .map_or_else(|| panic!("{name} is not on the card"), |(_, rect)| rect)
    }

    #[test]
    fn the_card_accompanies_only_the_root_page_and_only_with_media() {
        let (host, _chrome, _) = open_host(Vec::new());
        assert!(host.now_playing_view().is_none(), "no players, no card");
        assert!(!host.now_playing_published());

        let (host, chrome, _) = open_host(vec![player("music", "Music", PlaybackStatus::Playing)]);
        let view = host.now_playing_view().expect("a player");
        let rect = host
            .now_playing_rect(&view)
            .expect("room under the popover");
        let qs = host.quick_settings_rect();
        assert!(rect.y0 > qs.y1, "under the popover, never over it");
        assert!(rect.y1 <= host.rect.y1);
        assert!(
            host.accessible_overlay_order()
                .contains(&LayerId::NowPlaying)
        );
        assert!(
            host.quick_settings_semantic_rects()
                .iter()
                .any(|(name, _)| name == "now_playing_play_pause")
        );

        chrome
            .borrow_mut()
            .quick_settings
            .push_level(QsLevel::Clipboard);
        assert!(
            host.now_playing_view().is_none(),
            "drill-ins replace the card"
        );
    }

    #[test]
    fn clicking_the_card_drives_the_player_and_keeps_the_popover_open() {
        let (mut host, chrome, source) = open_host(vec![
            player("idle", "Idle", PlaybackStatus::Paused),
            player("music", "Music", PlaybackStatus::Playing),
        ]);
        let mut handled = false;
        let play = target(&host, "now_playing_play_pause");
        host.handle_shell_quick_settings_click(play.center(), &mut handled);
        assert!(handled);
        assert!(
            chrome.borrow().overlays.quick_settings,
            "the popover stays open"
        );
        assert_eq!(
            source.sent.borrow().as_slice(),
            [(
                "org.mpris.MediaPlayer2.music".to_string(),
                PlayerCommand::PlayPause
            )]
        );

        let players = target(&host, "now_playing_players");
        host.handle_shell_quick_settings_click(players.center(), &mut handled);
        let idle = target(&host, "now_playing_player[0]");
        host.handle_shell_quick_settings_click(idle.center(), &mut handled);
        assert_eq!(
            host.now_playing_view()
                .map(|view| view.player().identity.clone()),
            Some("Idle".to_string()),
            "the picked player is shown from now on"
        );
    }

    #[test]
    fn reverse_tab_from_no_selection_opens_power_while_media_plays() {
        let (mut host, chrome, _) =
            open_host(vec![player("music", "Music", PlaybackStatus::Playing)]);
        assert!(
            host.now_playing_view().is_some(),
            "the card is present so a wrap could steal Shift+Tab"
        );
        tab(&mut host, true);
        assert!(
            !host.now_playing_card.has_focus(),
            "Shift+Tab from no selection is Power, not the card"
        );
        let mut handled = false;
        assert!(host.handle_quick_settings_keyboard(
            &Key::Named(NamedKey::Enter),
            false,
            false,
            &mut handled,
        ));
        assert!(handled);
        assert!(
            host.power_menu_open(),
            "Enter on Power must open the menu even when media is playing"
        );
        assert!(!chrome.borrow().overlays.quick_settings);
    }

    #[test]
    fn tab_runs_from_the_root_page_through_the_card_and_back() {
        let (mut host, chrome, source) =
            open_host(vec![player("music", "Music", PlaybackStatus::Playing)]);
        let mut steps = 0;
        tab(&mut host, false);
        while !host
            .qs_root
            .focus_would_wrap(&chrome.borrow().quick_settings, false)
        {
            tab(&mut host, false);
            steps += 1;
            assert!(steps < 64, "the root page ends");
        }
        assert!(!host.now_playing_card.has_focus());
        tab(&mut host, false);
        assert!(host.now_playing_card.has_focus(), "Tab enters the card");
        // Seek, then previous, then play/pause.
        tab(&mut host, false);
        tab(&mut host, false);
        let mut handled = false;
        assert!(host.handle_quick_settings_keyboard(
            &Key::Named(NamedKey::Enter),
            false,
            false,
            &mut handled,
        ));
        assert_eq!(
            source.sent.borrow().last(),
            Some(&(
                "org.mpris.MediaPlayer2.music".to_string(),
                PlayerCommand::PlayPause
            ))
        );
        // Next, then past the card's end back to the root page.
        tab(&mut host, false);
        tab(&mut host, false);
        assert!(!host.now_playing_card.has_focus(), "Tab leaves the card");
        // Shift+Tab from the root page's first control comes back.
        tab(&mut host, true);
        assert!(host.now_playing_card.has_focus());
    }

    #[test]
    fn accessibility_paths_address_the_card_targets() {
        let (mut host, _chrome, _) =
            open_host(vec![player("music", "Music", PlaybackStatus::Playing)]);
        let index = host
            .accessible_overlay_order()
            .iter()
            .position(|overlay| *overlay == LayerId::NowPlaying)
            .expect("the card is published");
        let node = host.now_playing_accessibility().expect("card node");
        assert!(!node.children.is_empty());
        for child in 0..node.children.len() {
            assert!(host.synthetic_accessibility_rect(&[index, child]).is_some());
        }
        assert!(host.focus_synthetic_accessibility(&[index, 1]));
        assert!(host.now_playing_card.has_focus());
    }
}
