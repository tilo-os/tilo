//! Reported macro footprints with shared manual layout and retained cards.
use crate::resource::{ChipProject, ProjectState};
use crate::verification::placement::Placement;
use crate::view::ChipEffect;
use ui::framework::ViewCx;
use ui::i18n::I18nManager;
use ui::num::f32_of_f64;
use ui::widget::{
    Button, Card, Column, Container, Element, Manual, Property, PropertyList, ScrollAxis,
    Scrollable, Text,
};

fn tr(key: &str) -> String {
    I18nManager::lookup_plain(key)
}

pub fn item(project: &ProjectState, report: &str, id: &str) -> Option<serde_json::Value> {
    let (stage, instance) = id.split_once('/')?;
    let flow = project
        .verifications
        .get(report)?
        .receipt
        .evidence_flow
        .as_ref()?;
    let placement = flow
        .stages
        .iter()
        .find(|entry| entry.id == stage)?
        .placement
        .as_ref()?;
    let item = placement.macros.iter().find(|entry| entry.id == instance)?;
    Some(serde_json::json!({"placement":placement, "macro":item}))
}

pub fn body(
    cx: &ViewCx<ChipProject>,
    project: &ProjectState,
    report: &str,
    kind: &str,
    id: &str,
) -> Option<Element> {
    let (stage, selected) = match kind {
        "stage" => (id, None),
        "placement" => {
            let (stage, selected) = id.split_once('/')?;
            (stage, Some(selected))
        }
        _ => return None,
    };
    let flow = project
        .verifications
        .get(report)?
        .receipt
        .evidence_flow
        .as_ref()?;
    let placement = flow
        .stages
        .iter()
        .find(|entry| entry.id == stage)?
        .placement
        .as_ref()?;
    let mut page = Column::new()
        .push(Box::new(Text::new(&tr("chip-placement-title"))))
        .push(Box::new(Text::new(&tr("chip-placement-disclaimer"))))
        .push(Box::new(PropertyList::new(vec![
            Property::new(
                tr("chip-placement-total"),
                placement.total_instances.to_string(),
            ),
            Property::new(
                tr("chip-placement-omitted"),
                placement.omitted_instances.to_string(),
            ),
            Property::new(
                tr("chip-placement-die"),
                coordinates(&placement.die_dbu, placement.dbu_per_micron),
            ),
        ])))
        .push(canvas(cx, report, stage, placement, selected));
    for item in &placement.macros {
        page = page.push(Box::new(button(cx, report, stage, item, false)));
        if selected == Some(item.id.as_str()) {
            page = page.push(Box::new(PropertyList::new(vec![
                Property::new(tr("chip-placement-master"), &item.master),
                Property::new(
                    tr("chip-placement-bounds"),
                    coordinates(&item.bounds_dbu, placement.dbu_per_micron),
                ),
                Property::new(
                    tr("chip-placement-orientation"),
                    format!("{:?}", item.orientation),
                ),
                Property::new(tr("chip-placement-status"), format!("{:?}", item.status)),
            ])));
        }
    }
    Some(Box::new(page))
}

fn coordinates(bounds: &[i32; 4], units: u32) -> String {
    bounds
        .iter()
        .map(|value| format!("{:.3}", f64::from(*value) / f64::from(units)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn canvas(
    cx: &ViewCx<ChipProject>,
    report: &str,
    stage: &str,
    placement: &Placement,
    selected: Option<&str>,
) -> Element {
    let [left, bottom, right, top] = placement.die_dbu;
    let width = f64::from(right) - f64::from(left);
    let height = f64::from(top) - f64::from(bottom);
    let scale = 900.0 / width.max(height);
    let extent = ui::taffy::Size {
        width: f32_of_f64(width * scale),
        height: f32_of_f64(height * scale),
    };
    let rectangles: Vec<_> = placement
        .macros
        .iter()
        .map(|item| {
            let [x0, y0, x1, y1] = item.bounds_dbu;
            ui::kurbo::Rect::new(
                (f64::from(x0) - f64::from(left)) * scale,
                (f64::from(top) - f64::from(y1)) * scale,
                (f64::from(x1) - f64::from(left)) * scale,
                (f64::from(top) - f64::from(y0)) * scale,
            )
        })
        .collect();
    let mut canvas = Manual::new(move |layout| {
        for (index, rectangle) in rectangles.iter().enumerate() {
            layout.place(index, *rectangle)?;
        }
        Ok(extent)
    });
    for item in &placement.macros {
        let mut card = Card::new(Box::new(Text::new(&item.name)))
            .with_accessible_name(&item.name)
            .selected(selected == Some(item.id.as_str()));
        if let Some(binding) = cx.actions().effect(&ChipEffect::InspectEvidence {
            report: report.into(),
            item: format!("placement/{stage}/{}", item.id),
        }) {
            card = card.on_action(format!("placement-{stage}-{}", item.id), binding);
        }
        canvas = canvas.child(Box::new(card));
    }
    Box::new(
        Scrollable::new(ScrollAxis::Both)
            .with_height(extent.height)
            .with_child(Box::new(
                Container::new()
                    .with_background()
                    .with_no_padding()
                    .with_size(extent.width, extent.height)
                    .with_child(Box::new(canvas)),
            )),
    )
}

fn button(
    cx: &ViewCx<ChipProject>,
    report: &str,
    stage: &str,
    item: &crate::verification::placement::Macro,
    graph: bool,
) -> Button {
    let prefix = if graph { "placement" } else { "placement-list" };
    cx.actions().bind_effect_invocation(
        Button::new(&format!("{prefix}-{stage}-{}", item.id), &item.name),
        &ChipEffect::InspectEvidence {
            report: report.into(),
            item: format!("placement/{stage}/{}", item.id),
        },
    )
}
