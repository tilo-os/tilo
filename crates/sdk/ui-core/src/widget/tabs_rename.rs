//! Naming a tab in place: the tab's title becomes a field inside its own pill, set in the bar's
//! own text role, holding the title selected.

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{ElementState, WindowEvent};

use super::TabBar;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::DrawChild;
use crate::widget::{EditOutcome, EditableLabel, Widget};

pub(super) struct TabRename {
    tab: u64,
    from: String,
    label: Box<EditableLabel>,
}

impl TabBar {
    /// Turn tab `id`'s title into a field holding it, all of it selected, where the title
    /// stands. `name` is what a reader announces the field as.
    pub fn begin_rename(&mut self, id: u64, name: &str) -> bool {
        self.touch();
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else {
            return false;
        };
        if self.pinned_ids.contains(&id) {
            return false;
        }
        if self.renaming() == Some(id) {
            return true;
        }
        let mut label = EditableLabel::new(&tab.title)
            .with_text_role(TextRole::Body)
            .clears_on_blank()
            .with_accessible_name(name)
            .with_id("tab-rename");
        label.begin_edit();
        self.rename = Some(TabRename {
            tab: id,
            from: tab.title.clone(),
            label: Box::new(label),
        });
        self.lay_out_rename();
        true
    }

    #[must_use]
    pub fn renaming(&self) -> Option<u64> {
        self.rename.as_ref().map(|rename| rename.tab)
    }

    #[must_use]
    pub fn rename_text(&self) -> Option<&str> {
        self.rename
            .as_ref()
            .map(|rename| rename.label.current_text())
    }

    /// Hand an event to the name field while a tab is being named.
    pub fn rename_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        self.touch();
        let Some(rename) = self.rename.as_mut() else {
            return false;
        };
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = ctx.logical(position);
        }
        let inside = rename.label.get_rect().contains(self.last_cursor);
        let took = match event {
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => {
                rename.label.handle_event(event, ctx);
                *ctx.handled = true;
                true
            }
            WindowEvent::MouseInput { state, .. } => {
                if inside {
                    rename.label.handle_event(event, ctx);
                    *ctx.handled = true;
                    true
                } else {
                    if *state == ElementState::Pressed {
                        rename.label.commit();
                    }
                    false
                }
            }
            WindowEvent::CursorMoved { .. } => {
                let was = *ctx.handled;
                rename.label.handle_event(event, ctx);
                *ctx.handled = was;
                false
            }
            _ => false,
        };
        self.settle_rename();
        took
    }

    pub fn cancel_rename(&mut self) -> bool {
        self.touch();
        let Some(rename) = self.rename.as_mut() else {
            return false;
        };
        rename.label.cancel();
        self.settle_rename();
        true
    }

    /// A name committed in place since the last call: the tab, and the name
    /// (empty when the person cleared it).
    pub fn take_rename(&mut self) -> Option<(u64, String)> {
        self.pending_rename.take()
    }

    /// The tab a person double-clicked since the last call, asking to name
    /// it: the host opens the field where naming it is theirs to do.
    pub fn take_rename_ask(&mut self) -> Option<u64> {
        self.rename_ask.take()
    }

    /// A press on tab `id`'s pill: the second one soon after the first asks
    /// to name the tab.
    pub(super) fn note_tab_press(&mut self, id: u64) {
        self.touch();
        let now = crate::time::now();
        let second = self.last_tab_press.is_some_and(|(tab, at)| {
            tab == id && now.saturating_duration_since(at) < crate::gesture::multi_click_interval()
        });
        if second {
            self.last_tab_press = None;
            self.rename_ask = Some(id);
        } else {
            self.last_tab_press = Some((id, now));
        }
    }

    /// Read how the edit ended, if it did: a committed name that differs
    /// from the title it started from is pending for the host, and shown at
    /// once so the pill does not flash the old title for a frame.
    fn settle_rename(&mut self) {
        self.touch();
        let Some(rename) = self.rename.as_mut() else {
            return;
        };
        match rename.label.take_outcome() {
            EditOutcome::None => {}
            EditOutcome::Cancelled => self.rename = None,
            EditOutcome::Committed(name) => {
                let tab = rename.tab;
                if name != rename.from {
                    if !name.is_empty()
                        && let Some(shown) = self.tabs.iter_mut().find(|t| t.id == tab)
                    {
                        shown.title.clone_from(&name);
                    }
                    self.pending_rename = Some((tab, name));
                }
                self.rename = None;
            }
        }
    }

    /// Stand the field where the tab's title stands now. A tab that went
    /// away, or is now drawn as an icon alone, ends the edit unchanged.
    pub(super) fn lay_out_rename(&mut self) {
        self.touch();
        let Some(tab) = self.renaming() else {
            return;
        };
        let place = self
            .tabs
            .iter()
            .position(|t| t.id == tab)
            .filter(|_| !self.icon_only(tab))
            .and_then(|i| Some((i, *self.tab_rects.get(i)?)));
        let Some((i, pill)) = place else {
            self.rename = None;
            return;
        };
        let (x0, x1) = self.title_zone(i, pill);
        let field = Rect::new(x0, pill.y0, x1.min(pill.x1).max(x0), pill.y1);
        if let Some(rename) = self.rename.as_mut() {
            rename.label.apply_layout_external(field);
        }
    }

    pub(super) fn draw_rename(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.touch();
        self.lay_out_rename();
        if let Some(rename) = self.rename.as_mut() {
            rename
                .label
                .draw_child(scene, transform, text_renderer, theme, presence);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic_input::{char_key, cursor_moved, mouse_press, mouse_release, named_key};
    use crate::testing::dispatch;
    use crate::widget::TabDescriptor;
    use winit::keyboard::NamedKey;

    /// Two tabs of 200px each, laid out by hand.
    fn bar() -> TabBar {
        let mut bar = TabBar::new(
            "g",
            vec![
                TabDescriptor::new(1, "~/src"),
                TabDescriptor::new(2, "Logs"),
            ],
            1,
        );
        bar.rect = Rect::new(0.0, 0.0, 400.0, 40.0);
        bar.tab_rects = vec![
            Rect::new(0.0, 6.0, 200.0, 34.0),
            Rect::new(200.0, 6.0, 400.0, 34.0),
        ];
        bar
    }

    fn type_text(bar: &mut TabBar, text: &str) {
        for c in text.chars() {
            assert!(
                dispatch(bar, &char_key(&c.to_string())),
                "typing goes to the field"
            );
        }
    }

    #[test]
    fn the_field_stands_in_the_tabs_pill_holding_its_title() {
        let mut bar = bar();
        assert!(bar.begin_rename(1, "Tab name"));
        assert_eq!(bar.renaming(), Some(1));
        assert_eq!(bar.rename_text(), Some("~/src"));
        assert!(!bar.begin_rename(9, "Tab name"), "no such tab");
    }

    #[test]
    fn enter_commits_the_name_and_the_tab_shows_it() {
        let mut bar = bar();
        bar.begin_rename(1, "Tab name");
        type_text(&mut bar, "Build");
        assert!(dispatch(&mut bar, &named_key(NamedKey::Enter)));
        assert_eq!(bar.renaming(), None);
        assert_eq!(bar.take_rename(), Some((1, "Build".into())));
        assert_eq!(bar.take_rename(), None, "once");
        assert_eq!(bar.tabs()[0].title, "Build");
    }

    #[test]
    fn escape_puts_the_title_back() {
        let mut bar = bar();
        bar.begin_rename(1, "Tab name");
        type_text(&mut bar, "Nope");
        assert!(dispatch(&mut bar, &named_key(NamedKey::Escape)));
        assert_eq!(bar.renaming(), None);
        assert_eq!(bar.take_rename(), None);
        assert_eq!(bar.tabs()[0].title, "~/src");
    }

    #[test]
    fn an_emptied_field_clears_the_name() {
        let mut bar = bar();
        bar.begin_rename(1, "Tab name");
        assert!(dispatch(&mut bar, &named_key(NamedKey::Backspace)));
        assert_eq!(bar.rename_text(), Some(""));
        assert!(dispatch(&mut bar, &named_key(NamedKey::Enter)));
        assert_eq!(bar.take_rename(), Some((1, String::new())));
    }

    #[test]
    fn an_unchanged_title_names_nothing() {
        let mut bar = bar();
        bar.begin_rename(1, "Tab name");
        assert!(dispatch(&mut bar, &named_key(NamedKey::Enter)));
        assert_eq!(bar.renaming(), None);
        assert_eq!(bar.take_rename(), None);
    }

    #[test]
    fn a_double_click_on_a_tab_asks_to_name_it() {
        let mut bar = bar();
        let first = bar.tab_rects[0].center();
        let second = bar.tab_rects[1].center();
        let click = |bar: &mut TabBar, at: vello::kurbo::Point| {
            dispatch(bar, &cursor_moved(at.x, at.y));
            dispatch(bar, &mouse_press());
            dispatch(bar, &mouse_release());
        };
        click(&mut bar, first);
        assert_eq!(bar.take_rename_ask(), None, "one click selects");
        click(&mut bar, second);
        assert_eq!(
            bar.take_rename_ask(),
            None,
            "a click on another tab is no double click"
        );
        click(&mut bar, second);
        assert_eq!(
            bar.take_rename_ask(),
            Some(2),
            "the second click asks to name it"
        );
        assert_eq!(bar.take_rename_ask(), None, "once");
        click(&mut bar, second);
        assert_eq!(bar.take_rename_ask(), None, "a third click starts over");
    }

    #[test]
    fn a_press_elsewhere_commits_and_goes_on() {
        let mut bar = bar();
        bar.begin_rename(1, "Tab name");
        type_text(&mut bar, "Q");
        let other = bar.tab_rects[1].center();
        dispatch(&mut bar, &cursor_moved(other.x, other.y));
        assert_eq!(bar.renaming(), Some(1), "moving the pointer ends nothing");
        dispatch(&mut bar, &mouse_press());
        assert_eq!(bar.renaming(), None);
        assert_eq!(bar.take_rename(), Some((1, "Q".into())));
    }
}
