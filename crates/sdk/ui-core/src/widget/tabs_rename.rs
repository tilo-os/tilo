//! Naming a tab in place: the tab's title becomes a field inside its own
//! pill, set in the bar's own text role, holding the title selected. Enter
//! or a press anywhere else commits; Escape puts the title back. A blank
//! name is committed as empty, which the host reads as "no name": the tab
//! shows its window's own title again.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};

use super::TabBar;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{EditOutcome, EditableLabel, Widget};

/// A tab being named: which one, and its field.
pub(super) struct TabRename {
    tab: u64,
    /// The title the edit started from: committing it unchanged names
    /// nothing.
    from: String,
    label: Box<EditableLabel>,
}

impl TabBar {
    /// Turn tab `id`'s title into a field holding it, all of it selected,
    /// where the title stands. `name` is what a reader announces the field
    /// as. Returns whether the edit is open; a tab drawn as an icon alone
    /// has no title to edit.
    pub fn begin_rename(&mut self, id: u64, name: &str) -> bool {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else {
            return false;
        };
        if self.icon_only(id) {
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

    /// The tab whose name is being edited in place, if any.
    #[must_use]
    pub fn renaming(&self) -> Option<u64> {
        self.rename.as_ref().map(|rename| rename.tab)
    }

    /// Whether `p` is on the name field: a press there places the caret
    /// rather than ending the edit.
    #[must_use]
    pub fn rename_hit(&self, p: Point) -> bool {
        self.rename
            .as_ref()
            .is_some_and(|rename| rename.label.get_rect().contains(p))
    }

    /// The name field's text while it is edited.
    #[must_use]
    pub fn rename_text(&self) -> Option<&str> {
        self.rename
            .as_ref()
            .map(|rename| rename.label.current_text())
    }

    /// Hand an event to the name field while a tab is being named. The
    /// keyboard is the field's whole (typing never reaches the window under
    /// it); a press on the field is the field's, and a press anywhere else
    /// commits the name and goes on to what is under it. Returns whether the
    /// field took the event.
    pub fn rename_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
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
            // The field follows the pointer (for a drag selection) without
            // claiming it, so the bar keeps its hover.
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

    /// Commit the name being edited, as Enter does. Returns whether a tab
    /// was being named.
    pub fn commit_rename(&mut self) -> bool {
        let Some(rename) = self.rename.as_mut() else {
            return false;
        };
        rename.label.commit();
        self.settle_rename();
        true
    }

    /// Put the title back, ending the edit, as Escape does. Returns whether
    /// a tab was being named.
    pub fn cancel_rename(&mut self) -> bool {
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

    /// Read how the edit ended, if it did: a committed name that differs
    /// from the title it started from is pending for the host, and shown at
    /// once so the pill does not flash the old title for a frame.
    fn settle_rename(&mut self) {
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

    /// Paint the name field over its pill, in place of the title.
    pub(super) fn draw_rename(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.lay_out_rename();
        if let Some(rename) = self.rename.as_mut() {
            rename
                .label
                .draw(scene, transform, text_renderer, theme, presence);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic_input::{char_key, cursor_moved, mouse_press, named_key};
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
        let pill = bar.tab_rects[0];
        assert!(bar.rename_hit(pill.center()), "the field is in the pill");
        assert!(!bar.rename_hit(bar.tab_rects[1].center()));
        assert!(!bar.begin_rename(9, "Tab name"), "no such tab");
    }

    #[test]
    fn enter_commits_the_name_and_the_tab_shows_it() {
        let mut bar = bar();
        bar.begin_rename(1, "Tab name");
        // The title is selected: the first key replaces it.
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
    fn a_press_elsewhere_commits_and_goes_on() {
        let mut bar = bar();
        bar.begin_rename(1, "Tab name");
        type_text(&mut bar, "Q");
        let other = bar.tab_rects[1].center();
        dispatch(&mut bar, &cursor_moved(other.x, other.y));
        assert_eq!(bar.renaming(), Some(1), "moving the pointer ends nothing");
        // The press commits, and the bar goes on to handle it as a press on
        // the other tab.
        dispatch(&mut bar, &mouse_press());
        assert_eq!(bar.renaming(), None);
        assert_eq!(bar.take_rename(), Some((1, "Q".into())));
    }
}
