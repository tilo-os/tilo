//! Geometry of every Settings pane at a wide and a narrow window.
//!
//! A pane is one toolkit form on the toolkit's page frame, so these properties
//! hold for all of them by construction; the test keeps them that way. Every
//! assertion reads the production widget tree after a real layout pass.

use settings::categories::Category;
use ui::widget::{LabelPlacement, LayoutInfo, Widget};
use vello::kurbo::Rect;

use crate::composition_balance::lay_out_at;

const WIDE: (f32, f32) = (1200.0, 760.0);
const NARROW: (f32, f32) = (440.0, 820.0);
/// The toolkit's readable width, which bounds the form on a wide window.
const READABLE: f64 = ui::tokens::Metrics::BASE.readable_width as f64;

struct FieldGeometry {
    rect: Rect,
    placement: LabelPlacement,
    label_width: f64,
}

fn collect(
    widget: &dyn Widget,
    fields: &mut Vec<FieldGeometry>,
    overflow: &mut Vec<String>,
    width: f64,
) {
    let rect = widget.get_rect();
    if !rect.is_zero_area() && rect.x1 > width + 1.0 {
        overflow.push(format!("{} {rect:?}", widget.name()));
    }
    if let Some(LayoutInfo::Field(layout)) = widget.layout_info() {
        fields.push(FieldGeometry {
            rect,
            placement: layout.resolved_placement,
            label_width: f64::from(layout.label_width),
        });
    }
    for child in widget.children() {
        collect(child, fields, overflow, width);
    }
}

#[test]
fn every_pane_keeps_one_label_column_and_fits_at_wide_and_narrow_widths() {
    for (width, height) in [WIDE, NARROW] {
        for &category in Category::ALL {
            let config = crate::support::ScratchConfig::new();
            let app = lay_out_at(config.app().with_category(category), width, height);
            let mut fields = Vec::new();
            let mut overflow = Vec::new();
            collect(&*app, &mut fields, &mut overflow, f64::from(width));
            let at = format!("{category:?} at {width}x{height}");
            assert!(
                overflow.is_empty(),
                "{at}: content overflows the window horizontally: {overflow:?}"
            );
            let beside: Vec<_> = fields
                .iter()
                .filter(|field| field.placement == LabelPlacement::Beside)
                .collect();
            if let Some(first) = beside.first() {
                for field in &beside {
                    assert!(
                        (field.label_width - first.label_width).abs() < 0.5,
                        "{at}: label columns differ ({} vs {})",
                        field.label_width,
                        first.label_width
                    );
                    assert!(
                        (field.rect.x0 - first.rect.x0).abs() < 0.5
                            && (field.rect.x1 - first.rect.x1).abs() < 0.5,
                        "{at}: rows are not aligned ({:?} vs {:?})",
                        field.rect,
                        first.rect
                    );
                }
            }
            if width > 900.0 {
                for field in &fields {
                    assert!(
                        field.rect.width() <= READABLE + 1.0,
                        "{at}: a row is {} wide, wider than the readable column",
                        field.rect.width()
                    );
                }
            }
        }
    }
}
