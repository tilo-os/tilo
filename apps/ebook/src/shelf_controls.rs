//! Library shelf sorting and filtering controls.
//!
//! The component owns the sort dropdown (the toolkit's [`Dropdown`], hosted),
//! the hide-finished switch, their drawing and their hit stack. Persisted
//! preference changes are emitted as semantic actions for the app.
//!
//! [`Dropdown`]: ui::widget::Dropdown

use automerge::ObjId;
use vello::kurbo::{Point, Rect};

use ui::TextRole;
use ui::paint::PaintCtx;
use ui::widget::{ControlSize, Toggle, ToggleFace};

use crate::hosted_dropdown::HostedDropdown;
use crate::input_controls::InputEventCtx;
use crate::model::{LibraryPrefs, LibrarySort};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShelfControlAction {
    ToggleHideRead,
}

/// How a sort order reads in the dropdown, which shows it alone.
fn sort_choice(sort: LibrarySort) -> &'static str {
    match sort {
        LibrarySort::Title => "By title",
        LibrarySort::Author => "By author",
        LibrarySort::Recent => "Recently opened",
        LibrarySort::Progress => "By progress",
    }
}

pub(super) struct ShelfControls {
    sort: HostedDropdown,
    hits: Vec<(Rect, ShelfControlAction)>,
    /// The "Hide finished" switch, sliding as a click turns it.
    faces: ui::widget::FaceMotions,
}

impl Default for ShelfControls {
    fn default() -> Self {
        Self {
            sort: HostedDropdown::new("books.library.sort", "Sort books"),
            hits: Vec::new(),
            faces: ui::widget::FaceMotions::default(),
        }
    }
}

impl ShelfControls {
    /// Whether its switch must be painted again: on its way, or not yet
    /// painted at rest. Its frames come from the motion clock.
    pub(super) fn needs_redraw(&self) -> bool {
        self.faces.needs_redraw(ui::motion::frame_now())
    }

    pub(super) fn accessibility_nodes(
        &self,
        prefs: LibraryPrefs,
        focused: Option<&str>,
    ) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut nodes: Vec<AccessNode> = self.sort.accessibility().into_iter().collect();
        nodes.extend(
            self.hits
                .iter()
                .map(|(rect, ShelfControlAction::ToggleHideRead)| {
                    let target = "books.library.hide-read";
                    AccessNode::new(Role::Switch)
                        .with_name("Hide finished books")
                        .with_bounds(*rect)
                        .with_state(
                            AccessState::new()
                                .checked(prefs.hide_read)
                                .focusable(true)
                                .focused(focused == Some(target)),
                        )
                        .with_action_target(target)
                }),
        );
        nodes
    }

    pub(super) fn semantic_action(target: &str) -> Option<ShelfControlAction> {
        match target {
            "books.library.hide-read" => Some(ShelfControlAction::ToggleHideRead),
            _ => None,
        }
    }

    pub(super) fn begin_frame(&mut self) {
        self.hits.clear();
        self.sort.begin_frame();
    }

    pub(super) fn close_sort_menu(&mut self) {
        self.sort.close();
    }

    #[cfg(test)]
    pub(super) fn has_action(&self, wanted: ShelfControlAction) -> bool {
        self.hits.iter().any(|(_, action)| *action == wanted)
    }

    pub(super) fn hit_test(&self, point: Point) -> Option<ShelfControlAction> {
        self.hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, action)| *action)
    }

    /// Route one event to the sort dropdown. Returns whether it took the
    /// event, and the order the person picked, if they did.
    pub(super) fn handle_event(
        &mut self,
        ctx: &mut InputEventCtx<'_>,
    ) -> (bool, Option<LibrarySort>) {
        let consumed = self.sort.handle_event(ctx);
        let picked = self
            .sort
            .take_change()
            .map(|value| LibrarySort::from_str(&value));
        (consumed, picked)
    }

    /// An assistive technology's action on the sort dropdown: whether it
    /// was the dropdown's, and the order it picked, if it did.
    pub(super) fn perform_dropdown_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) -> (bool, Option<LibrarySort>) {
        if !self.sort.perform_action(target, action, state, root) {
            return (false, None);
        }
        let picked = self
            .sort
            .take_change()
            .map(|value| LibrarySort::from_str(&value));
        (true, picked)
    }

    /// The shelf's view controls, laid out leading-to-trailing from `start`
    /// along the header `band`: the sort dropdown, then the hide-finished
    /// switch with its label. Returns the x just past the last control.
    pub(super) fn draw_bar(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        band: Rect,
        start: f64,
        cursor: Point,
        prefs: LibraryPrefs,
    ) -> f64 {
        let theme = ctx.theme;
        let rhythm = &theme.tokens.spacing.rhythm;
        let size = ControlSize::Regular;
        let height = size.height(theme);
        let y = band.center().y - height / 2.0;

        self.sort.set(
            LibrarySort::ALL
                .iter()
                .map(|sort| (sort.as_str().to_string(), sort_choice(*sort).to_string()))
                .collect(),
            prefs.sort.as_str(),
        );
        let width = HostedDropdown::width(
            LibrarySort::ALL.iter().map(|sort| sort_choice(*sort)),
            size,
            ctx.text,
            theme,
        );
        let sort = Rect::new(start, y, start + width, y + height);
        self.sort
            .draw(ctx.scene, ctx.transform, ctx.text, theme, sort);

        // The switch and its label read as one control: the whole cluster is
        // the pointer target.
        let mut x = sort.x1 + f64::from(rhythm.sibling);
        let hide = "Hide finished";
        let label_band = Rect::new(x, band.y0, band.x1, band.y1);
        ctx.text_role_in_band(hide, TextRole::Label, theme.text_dim, x, label_band, None);
        x += ctx.measure_role(hide, TextRole::Label, None).width + f64::from(rhythm.related);
        let (track_w, track_h) = crate::app::switch_size(theme);
        let track = Rect::new(
            x,
            band.center().y - track_h / 2.0,
            x + track_w,
            band.center().y + track_h / 2.0,
        );
        let cluster = Rect::new(label_band.x0, y, track.x1, y + height);
        Toggle::paint(
            ctx.scene,
            ctx.transform,
            theme,
            &ToggleFace::new(track, prefs.hide_read)
                .pose(
                    self.faces
                        .switch("hide-read", prefs.hide_read, theme.motion()),
                )
                .hovered(cluster.contains(cursor)),
        );
        self.hits
            .push((cluster, ShelfControlAction::ToggleHideRead));
        track.x1
    }

    /// The open sort list, over everything inside `window`.
    pub(super) fn draw_overlay(&mut self, ctx: &mut PaintCtx<'_>, window: Rect) {
        self.sort
            .draw_overlay(ctx.scene, ctx.transform, ctx.text, ctx.theme, window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use vello::Scene;
    use vello::kurbo::Affine;

    /// The bar exposes the sort dropdown and the hide-finished switch.
    #[test]
    fn shelf_controls_expose_sort_and_filter() {
        let mut controls = ShelfControls::default();
        controls.begin_frame();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        let band = Rect::new(0.0, 0.0, 1280.0, 56.0);
        controls.draw_bar(&mut ctx, band, 12.0, Point::ZERO, LibraryPrefs::default());
        let toggle = controls
            .hits
            .iter()
            .find(|(_, action)| *action == ShelfControlAction::ToggleHideRead)
            .map(|(rect, _)| rect.center())
            .expect("hide-read toggle");
        assert_eq!(
            controls.hit_test(toggle),
            Some(ShelfControlAction::ToggleHideRead)
        );
        let nodes = controls.accessibility_nodes(LibraryPrefs::default(), None);
        assert!(
            nodes
                .iter()
                .any(|node| node.action_target.as_deref() == Some("books.library.sort")),
            "the sort dropdown is in the accessibility tree"
        );
    }
}
