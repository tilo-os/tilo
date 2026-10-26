//! A lens page laid out by the toolkit: its content's widgets, rebuilt
//! when the content changes, and the rect of every control it names.

use std::collections::HashMap;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Column, ControlSize, Embedded, FieldFace, Widget};

/// A page of the lens built from `C`, placed in the lens and drawn there.
pub(super) struct Page<C> {
    built: Option<(C, f64, Embedded<Column>)>,
    /// Each control's rect in the page's own space, from its last layout.
    rects: HashMap<String, Rect>,
    height: f64,
    /// Where the page's own space starts in the lens's.
    offset: Vec2,
}

impl<C> Default for Page<C> {
    fn default() -> Self {
        Self {
            built: None,
            rects: HashMap::new(),
            height: 0.0,
            offset: Vec2::ZERO,
        }
    }
}

impl<C: Clone + PartialEq> Page<C> {
    /// Lay the page out `width` wide for `content`, rebuilding it with
    /// `build` when the content or the theme's field height changed.
    /// Returns the page's height.
    pub(super) fn lay_out_with(
        &mut self,
        content: &C,
        width: f64,
        theme: &Theme,
        text: &mut TextRenderer,
        build: fn(&C, f64) -> Column,
    ) -> f64 {
        let field = FieldFace::height(ControlSize::Regular, theme);
        let stale = self.built.as_ref().is_none_or(|(built, built_field, _)| {
            built != content || built_field.to_bits() != field.to_bits()
        });
        if stale {
            self.built = Some((content.clone(), field, Embedded::new(build(content, field))));
        }
        let Some((_, _, page)) = self.built.as_mut() else {
            return 0.0;
        };
        self.height = page.layout(Rect::new(0.0, 0.0, width, 0.0), theme, text);
        self.rects.clear();
        collect_rects(page.widget(), &mut self.rects);
        self.height
    }
}

impl<C> Page<C> {
    /// Drop `id`'s rect: the control takes no press.
    pub(super) fn forget(&mut self, id: &str) {
        self.rects.remove(id);
    }

    /// Where the page's top-left corner stands in the lens.
    pub(super) fn place(&mut self, origin: Point) {
        self.offset = origin.to_vec2();
        if let Some((_, _, page)) = self.built.as_mut() {
            page.set_transform(Affine::translate(self.offset));
        }
    }

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        if let Some((_, _, page)) = self.built.as_mut() {
            page.draw(scene, transform, text, theme, &PresenceCtx::default());
        }
    }

    pub(super) fn height(&self) -> f64 {
        self.height
    }

    /// The laid-out page, for tests that read what it shows.
    #[cfg(test)]
    pub(super) fn widget(&self) -> Option<&dyn Widget> {
        self.built
            .as_ref()
            .map(|(_, _, page)| page.widget() as &dyn Widget)
    }

    /// The control `id`'s rect in the lens, or zero where the page has none.
    pub(super) fn rect(&self, id: &str) -> Rect {
        self.rects.get(id).map_or(Rect::ZERO, |rect| {
            Affine::translate(self.offset).transform_rect_bbox(*rect)
        })
    }
}

fn collect_rects(widget: &dyn Widget, out: &mut HashMap<String, Rect>) {
    if let Some(id) = widget.test_id() {
        out.insert(id.to_owned(), widget.get_rect());
    }
    for child in widget.children() {
        collect_rects(child, out);
    }
}
