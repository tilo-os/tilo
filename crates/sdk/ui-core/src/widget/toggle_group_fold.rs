//! A segmented control whose labels do not fit folds into a picker: the
//! chosen segment's label and a chevron, on the same face the window bar's
//! location and views menu use ([`super::super::window_toolbar_row::paint_picker`]),
//! and a menu of every segment, the chosen one checked. No segment label is
//! ever cut short.
//!
//! A [`ToggleGroup`] folds on its own. A host that paints a
//! [`SegmentedFace`] asks [`SegmentedFace::folds`] whether its strip folds,
//! paints it with [`SegmentedFace::folded`], and opens the shared
//! [`ContextMenu`] with [`SegmentedFace::menu_items`] when the folded strip
//! is pressed.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::super::context_menu::{ContextMenu, MenuItem};
use super::super::window_toolbar_row::{LocationLift, PickerText, paint_picker};
use super::{
    NONE_CHOSEN, Orientation, Segment, SegmentedFace, SegmentedStyle, StripMetrics, ToggleGroup,
};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{ControlSize, Widget};

/// Whether segments of natural widths `naturals` (full label, short label)
/// all show a whole label in `width`: their full labels, or their short
/// forms where they have them.
pub(super) fn labels_fit(width: f64, naturals: &[(f64, f64)]) -> bool {
    let needed: f64 = naturals.iter().map(|(full, short)| full.min(*short)).sum();
    needed <= width.max(0.0) + 1e-6
}

impl SegmentedFace<'_> {
    /// Whether a strip of plain `labels` laid out in `segments` folds into a
    /// picker: some label, even in its short form, does not fit whole in the
    /// segment it would paint in.
    #[must_use]
    pub fn folds(
        segments: &[Rect],
        labels: &[&str],
        size: ControlSize,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> bool {
        let shown: Vec<Segment> = labels.iter().map(|label| Segment::new(*label)).collect();
        let naturals =
            StripMetrics::of(size, theme).naturals(&shown, SegmentedStyle::Segmented, text);
        labels.len() == segments.len()
            && labels.iter().any(|label| !label.is_empty())
            && segments
                .iter()
                .zip(&naturals)
                .any(|(segment, (full, short))| full.min(*short) > segment.width() + 0.5)
    }

    /// Paint the strip folded, as a picker: `hovered` lifts it, `open` says
    /// its menu is up.
    #[must_use]
    pub fn folded(mut self, hovered: bool, open: bool) -> Self {
        self.fold = Some(LocationLift {
            hovered,
            pressed: open,
            focused: false,
        });
        self
    }

    /// The menu a folded strip of `labels` opens: every segment, the
    /// `selected` one checked. Row `i` chooses segment `i`.
    #[must_use]
    pub fn menu_items(labels: &[&str], selected: usize) -> Vec<MenuItem> {
        labels
            .iter()
            .enumerate()
            .map(|(i, label)| MenuItem::selection(*label).with_checked(i == selected))
            .collect()
    }

    /// The whole strip: every segment's rect, joined.
    fn strip(&self) -> Option<Rect> {
        let first = self.segments.first()?;
        Some(
            self.segments
                .iter()
                .fold(*first, |strip, segment| strip.union(*segment)),
        )
    }

    /// What a folded strip says: the chosen segment's label, or, with none
    /// chosen, that one is to be chosen (never a segment it is not).
    fn folded_label(&self) -> std::borrow::Cow<'_, str> {
        match self.items.get(self.selected) {
            Some(item) if self.selected != NONE_CHOSEN => std::borrow::Cow::Borrowed(item.label),
            _ => {
                std::borrow::Cow::Owned(crate::i18n::I18nManager::lookup_plain("segmented-choose"))
            }
        }
    }

    /// Paint the folded strip, if it is folded; whether it was.
    pub(super) fn paint_folded(
        &self,
        scene: &mut Scene,
        transform: Affine,
        (text, theme): (&mut TextRenderer, &Theme),
    ) -> bool {
        let (Some(mut lift), Some(strip)) = (self.fold, self.strip()) else {
            return false;
        };
        // Keyboard focus rings the picker as it rings the whole strip.
        lift.focused = self.focused && !self.disabled;
        let mut ink = theme.tokens.colors.text_primary;
        if self.disabled {
            ink = crate::color_compat::ColorCompat::with_alpha_factor(
                ink,
                crate::widget::DISABLED_ALPHA,
            );
        }
        paint_picker(
            scene,
            transform,
            (text, theme),
            PickerText::new(&self.folded_label(), ink),
            strip,
            lift,
        );
        true
    }
}

impl ToggleGroup {
    /// Whether this strip may fold: one choice of labelled segments across
    /// a row. A tab strip scrolls instead; a multi-select strip wraps; a
    /// rail stacks.
    pub(super) fn can_fold(&self) -> bool {
        self.orientation == Orientation::Horizontal
            && self.chosen.is_none()
            && self.style != SegmentedStyle::Tabs
            && self
                .segments
                .iter()
                .any(|segment| !segment.label.is_empty())
    }

    /// Fold or unfold for the room the strip has.
    pub(super) fn refold(&mut self, measured: bool) {
        let folded = self.can_fold() && measured && !labels_fit(self.rect.width(), &self.naturals);
        if folded != self.folded {
            self.folded = folded;
            self.cache_dirty = true;
            if !folded {
                self.menu.close();
            }
        }
    }

    /// Whether the strip is folded into a picker.
    #[must_use]
    pub fn is_folded(&self) -> bool {
        self.folded
    }

    /// Whether the folded strip's menu is open.
    #[must_use]
    pub fn menu_is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Open the folded strip's menu under it: every segment, the chosen one
    /// checked, a disabled one disabled.
    pub(super) fn open_fold_menu(&mut self) {
        let items = self
            .segments
            .iter()
            .enumerate()
            .map(|(i, segment)| {
                MenuItem::selection(segment.label.clone())
                    .with_checked(i == self.selected)
                    .with_enabled(!segment.disabled)
            })
            .collect();
        self.menu.set_items(items);
        self.menu.set_align(super::super::PanelAlign::Leading);
        let viewport = self.viewport.unwrap_or(self.outer);
        self.menu.open_anchored(self.rect, viewport);
        self.cache_dirty = true;
    }

    /// Hand an event to the open menu; a row chosen chooses its segment.
    /// Whether the menu took the event.
    pub(super) fn fold_menu_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        if !self.menu.is_open() {
            return false;
        }
        let mut handled = false;
        self.menu.handle_event(
            event,
            &mut crate::widget::EventCtx {
                handled: &mut handled,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        if let Some(row) = self.menu.take_selection() {
            self.menu.close();
            if row < self.segments.len() {
                self.request_selection(row);
            }
        }
        if !self.menu.is_open() {
            self.cache_dirty = true;
        }
        // The menu is modal while it is up: the keyboard is its, and so is
        // the press that closes it.
        *ctx.handled = true;
        true
    }

    /// Paint the open menu above everything after it.
    pub(super) fn draw_fold_menu(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        (text, theme, presence): (&mut TextRenderer, &Theme, &crate::presence::PresenceCtx),
    ) {
        if let Some(viewport) = presence.viewport.get() {
            // An open menu stays inside the window as it is now.
            if self.menu.is_open() && self.viewport != Some(viewport) {
                self.menu.constrain_to(viewport);
            }
            self.viewport = Some(viewport);
        }
        crate::widget::Widget::draw_overlay(
            &mut self.menu,
            scene,
            transform,
            text,
            theme,
            presence,
        );
    }

    /// A fresh menu for a new strip.
    pub(super) fn fold_menu() -> ContextMenu {
        ContextMenu::new()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::synthetic_input::{cursor_moved, mouse_press, mouse_release};
    use crate::testing::dispatch;

    const LABELS: [&str; 5] = ["Cut", "Dissolve", "Fade", "Dip to black", "Wipe"];

    /// A transition chooser laid out `width` wide, as a host lays it out.
    fn chooser(width: f64, chosen: Rc<RefCell<Vec<usize>>>) -> ToggleGroup {
        let mut group = ToggleGroup::new(LABELS.iter().map(|l| Segment::new(*l)).collect(), 1)
            .on_change(move |index| chosen.borrow_mut().push(index));
        let mut text = TextRenderer::new();
        group.adopt_theme(&Theme::dark());
        group.measure_naturals(&mut text);
        group.apply_layout_external(Rect::new(0.0, 0.0, width, 36.0));
        group
    }

    /// One frame of a 640 by 480 window: the strip, then its overlay, as a
    /// window paints them.
    fn frame(group: &mut ToggleGroup) -> Vec<u32> {
        let mut text = TextRenderer::new();
        let theme = Theme::high_contrast();
        let presence = crate::presence::PresenceCtx::default();
        presence
            .viewport
            .set(Some(Rect::new(0.0, 0.0, 640.0, 480.0)));
        let mut scene = Scene::new();
        Widget::draw(
            group,
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &presence,
        );
        Widget::draw_overlay(
            group,
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &presence,
        );
        scene.encoding().draw_data.clone()
    }

    fn click(group: &mut ToggleGroup, at: vello::kurbo::Point) {
        dispatch(group, &cursor_moved(at.x, at.y));
        dispatch(group, &mouse_press());
        dispatch(group, &mouse_release());
    }

    #[test]
    fn a_strip_too_narrow_for_its_labels_folds_into_a_menu_of_every_segment() {
        let chosen = Rc::new(RefCell::new(Vec::new()));
        let wide = chooser(640.0, Rc::clone(&chosen));
        assert!(!wide.is_folded(), "with room, every label shows whole");

        let mut narrow = chooser(160.0, Rc::clone(&chosen));
        assert!(narrow.is_folded(), "too narrow: a picker, not cut labels");
        frame(&mut narrow);
        let center = narrow.rect.center();
        click(&mut narrow, center);
        assert!(narrow.menu_is_open(), "a press opens the menu");
        let rows: Vec<&str> = narrow
            .menu
            .items()
            .iter()
            .filter_map(MenuItem::label)
            .collect();
        assert_eq!(rows, LABELS, "the menu lists every segment, whole");
        assert_eq!(narrow.menu.items()[1].checked(), Some(true));
        let node = Widget::accessibility(&narrow).expect("the strip reaches assistive technology");
        assert_eq!(node.state.expanded, Some(true));
        assert_eq!(
            node.children.len(),
            LABELS.len(),
            "each segment stays choosable"
        );

        for index in [3, 0, 4] {
            if !narrow.menu_is_open() {
                click(&mut narrow, center);
            }
            frame(&mut narrow);
            let row = narrow.menu.item_rect(index).expect("the row is laid out");
            click(&mut narrow, row.center());
            assert_eq!(narrow.selected(), index, "row {index} chooses its segment");
            assert!(!narrow.menu_is_open(), "choosing closes the menu");
        }
        assert_eq!(*chosen.borrow(), [3, 0, 4]);
    }

    #[test]
    fn a_folded_strip_shows_keyboard_focus() {
        let mut narrow = chooser(160.0, Rc::new(RefCell::new(Vec::new())));
        assert!(narrow.is_folded());
        let unfocused = frame(&mut narrow);
        Widget::set_focus(&mut narrow, true);
        assert!(narrow.focus_visible(), "keyboard focus shows the ring");
        assert_ne!(frame(&mut narrow), unfocused, "focus rings the picker");
    }

    #[test]
    fn a_painted_strip_folds_where_its_labels_do_not_fit() {
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        let size = ControlSize::Regular;
        let mut laid_out = |width: f64| {
            let strip = Rect::new(0.0, 0.0, width, 32.0);
            SegmentedFace::partition(strip, &LABELS, size, &mut text, &theme)
        };
        let wide = laid_out(640.0);
        let narrow = laid_out(160.0);
        assert!(!SegmentedFace::folds(
            &wide, &LABELS, size, &mut text, &theme
        ));
        assert!(SegmentedFace::folds(
            &narrow, &LABELS, size, &mut text, &theme
        ));
        // Equal shares of a strip that holds the labels laid out by need cut
        // the longest one: the strip folds rather than paint it cut.
        let even: Vec<Rect> = (0..LABELS.len())
            .map(|i| {
                let x = f64::from(u8::try_from(i).unwrap_or(0)) * 70.0;
                Rect::new(x, 0.0, x + 70.0, 32.0)
            })
            .collect();
        assert!(SegmentedFace::folds(
            &even, &LABELS, size, &mut text, &theme
        ));
        assert!(!SegmentedFace::folds(
            &even,
            &["", "", "", "", ""],
            size,
            &mut text,
            &theme
        ));
        let items = SegmentedFace::menu_items(&LABELS, 2);
        assert_eq!(items.len(), LABELS.len());
        assert_eq!(items[2].checked(), Some(true));
        assert_eq!(items[0].checked(), Some(false));
    }
}
