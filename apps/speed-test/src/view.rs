//! The screen: the dial and its controls beside the results, which stack
//! below them in a narrow window. Built from the toolkit's composites; the
//! only thing the app draws itself is nothing.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use ui::TextRole;
use ui::taffy::JustifyContent;
use ui::theme::Theme;
use ui::tilo_actions::ActionBinding;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Banner, Button, ButtonRole, Column, Container, Decorator, Element, ListRow, ProgressRing,
    ProgressRingSize, Property, PropertyList, Row, Scrollable, Section, StatGroup, StatTile,
    Stepper, Text, Tone, Widget,
};

use crate::model::{DialReading, Model, Phase, Quality, age_label, data_label, qualities};

/// The model, shared with the buttons that drive it and the dial that reads it.
pub(crate) type Shared = Rc<RefCell<Model>>;

/// What the scroll position of the page is, kept across rebuilds.
pub(crate) type ScrollOffset = Rc<Cell<(f32, f32)>>;

/// The page for the model's current state.
/// `run` is what the run button invokes; without it the button is disabled.
pub(crate) fn build(
    shared: &Shared,
    scroll: &ScrollOffset,
    run: Option<ActionBinding<()>>,
    theme: &Theme,
) -> Element {
    let model = shared.borrow();
    let min_column = theme.tokens.metrics.content_min_width;
    let columns = Row::new()
        .align_start()
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_min_width(min_column)
                .with_child(Box::new(test_column(shared, &model, run, theme))),
        ))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_min_width(min_column)
                .with_child(Box::new(results_column(&model, theme))),
        ));
    let mut page = Column::new().with_intrinsic_height().with_no_padding();
    if let Some(error) = &model.error {
        let gutter = theme.tokens.spacing.s6;
        page = page.push(Box::new(
            Column::new()
                .with_intrinsic_height()
                .with_horizontal_padding(gutter, gutter)
                .with_vertical_padding(0.0)
                .with_extra_top_padding(gutter)
                .push(Box::new(
                    Banner::error("Test interrupted").with_message(error.clone()),
                )),
        ));
    }
    let page = page.push(Box::new(TestColumns {
        row: columns,
        preferred_minimum: min_column,
    }));
    Box::new(
        Scrollable::new_vertical()
            .with_persisted_offset(Rc::clone(scroll))
            .with_child(Box::new(page)),
    )
}

/// The preferred two-column measure is capped by the actual available row.
/// At phone widths the shared composites must see the viewport width, so
/// their own wrapping (result tiles and progress stations) can operate.
struct TestColumns {
    row: Row,
    preferred_minimum: f32,
}

impl Decorator for TestColumns {
    type Inner = Row;

    fn inner(&self) -> &Row {
        &self.row
    }
    fn inner_mut(&mut self) -> &mut Row {
        &mut self.row
    }

    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        self.preferred_minimum = theme.tokens.metrics.content_min_width;
        let node = self.row.build_layout(tree, theme, nodes);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn adjust_layout_style(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        text: &mut ui::TextRenderer,
    ) -> bool {
        let Ok(layout) = tree.layout(node) else {
            return false;
        };
        // Intrinsic descendants may initially widen the row beyond its
        // Scrollable. Cap against the enclosing layout's measured viewport,
        // rather than accepting that overflow as the available space.
        let mut available = layout.size.width;
        let mut ancestor = tree.parent(node);
        while let Some(parent) = ancestor {
            if let Ok(layout) = tree.layout(parent) {
                available = available.min(layout.size.width);
            }
            ancestor = tree.parent(parent);
        }
        let minimum = ui::taffy::Dimension::length(self.preferred_minimum.min(available));
        let children = tree.children(node).unwrap_or_default();
        let mut changed = false;
        for child in children {
            let Ok(style) = tree.style(child) else {
                continue;
            };
            if style.min_size.width == minimum && style.size.width == minimum {
                continue;
            }
            let mut style = style.clone();
            style.min_size.width = minimum;
            style.size.width = minimum;
            changed |= tree.set_style(child, style).is_ok();
        }
        // This adapter stands in for the row's node; the row adjusts it too.
        self.row.adjust_layout_style(tree, node, text) | changed
    }
}

/// The dial, what it is doing, and the one action.
fn test_column(
    shared: &Shared,
    model: &Model,
    run: Option<ActionBinding<()>>,
    theme: &Theme,
) -> Column {
    let status = match model.phase {
        Phase::Idle => "Ready to test",
        Phase::Ping => "Measuring latency",
        Phase::Download => "Measuring download",
        Phase::Upload => "Measuring upload",
        Phase::Complete => "Test complete",
        Phase::Error => "Ready to try again",
    };
    let (label, role) = if model.is_running() {
        ("Stop test", ButtonRole::Secondary)
    } else {
        ("Run speed test", ButtonRole::Primary)
    };
    let button = Button::new("run", label).role(role);
    let run = match run {
        Some(binding) => button.on_action(binding),
        None => button.with_enabled(false),
    };

    let mut column = Column::new()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Section)
        .push(centred(Box::new(Dial::new(shared, &model.dial()))))
        .push(centred(Box::new(
            Text::new(status).with_role(TextRole::Subheading),
        )))
        .push(centred(Box::new(run)));
    if model.phase != Phase::Idle && model.phase != Phase::Error {
        column = column.push(Box::new(
            Container::new()
                .with_no_padding()
                .with_max_width(theme.tokens.metrics.content_min_width)
                .with_child(Box::new(
                    Stepper::new(
                        ["Ping", "Download", "Upload"]
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                    )
                    .current(model.stage()),
                )),
        ));
    }
    column
}

fn centred(child: Element) -> Element {
    Box::new(Row::new().with_justify(JustifyContent::Center).push(child))
}

/// The measured numbers, the facts about the test, and the recent tests.
fn results_column(model: &Model, theme: &Theme) -> Column {
    let readings = &model.readings;
    let pending = "—";
    let number = |value: Option<f64>, decimals: usize| {
        value.map_or_else(|| pending.to_owned(), |v| format!("{v:.decimals$}"))
    };
    let tiles = StatGroup::new(vec![
        Box::new(tile("Ping", number(readings.ping, 0), readings.ping, "ms")),
        Box::new(tile(
            "Download",
            number(readings.download, 1),
            readings.download,
            "Mbps",
        )),
        Box::new(tile(
            "Upload",
            number(readings.upload, 1),
            readings.upload,
            "Mbps",
        )),
    ]);
    let server = readings.location.as_ref().map_or_else(
        || "Cloudflare".to_owned(),
        |city| format!("Cloudflare, {city}"),
    );
    let facts = PropertyList::new(vec![
        Property::new("Server", server),
        Property::new(
            "Jitter",
            readings
                .jitter
                .map_or_else(|| pending.to_owned(), |v| format!("{v:.1} ms")),
        )
        .with_kind(ui::TextKind::Numeric),
        Property::new(
            "Data used",
            if readings.bytes > 0 {
                data_label(readings.bytes)
            } else {
                pending.to_owned()
            },
        )
        .with_kind(ui::TextKind::Numeric),
    ]);

    let spacing = &theme.tokens.spacing;
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_gap(spacing.rhythm.section)
        .push(Box::new(Section::titled("Results", vec![Box::new(tiles)])))
        .push(Box::new(Section::titled(
            "Connection",
            vec![Box::new(facts)],
        )));

    if let Some(latest) = model.latest() {
        let rows = qualities(latest)
            .into_iter()
            .map(|(use_case, quality)| {
                Property::new(use_case, quality.label()).tone(match quality {
                    Quality::Excellent | Quality::Good => Tone::Success,
                    Quality::Limited => Tone::Warning,
                })
            })
            .collect();
        column = column.push(Box::new(Section::titled(
            "Connection quality",
            vec![Box::new(PropertyList::new(rows))],
        )));
    }

    if !model.history.is_empty() {
        let now = ui::clock::local_now().timestamp();
        let rows: Vec<Element> = model
            .history
            .iter()
            .enumerate()
            .map(|(index, result)| -> Element {
                Box::new(
                    ListRow::new(
                        format!("history-{index}"),
                        format!("{:.1} Mbps down", result.download),
                    )
                    .secondary(format!(
                        "{:.1} Mbps up, {:.0} ms ping",
                        result.upload, result.ping
                    ))
                    .meta(age_label(now, result.at)),
                )
            })
            .collect();
        column = column.push(Box::new(Section::titled("Recent tests", rows)));
    }
    column
}

/// One result: its number once measured, a dash until then.
fn tile(label: &str, value: String, measured: Option<f64>, unit: &str) -> StatTile {
    let tile = StatTile::new(label, value);
    if measured.is_some() {
        tile.unit(unit)
    } else {
        tile
    }
}

/// The dial: a progress ring that follows the shared model every frame.
struct Dial {
    ring: ProgressRing,
    shared: Shared,
    shown: DialReading,
}

impl Dial {
    fn new(shared: &Shared, dial: &DialReading) -> Self {
        Self {
            ring: ProgressRing::new(dial.frac)
                .size(ProgressRingSize::Large)
                .value(dial.value.clone())
                .caption(dial.unit),
            shared: Rc::clone(shared),
            shown: dial.clone(),
        }
    }
}

impl Decorator for Dial {
    type Inner = ProgressRing;

    fn inner(&self) -> &ProgressRing {
        &self.ring
    }

    fn inner_mut(&mut self) -> &mut ProgressRing {
        &mut self.ring
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let dial = self.shared.borrow().dial();
        let moved = dial != self.shown;
        if moved {
            self.ring.set(dial.frac, dial.value.clone());
            self.shown = dial;
        }
        Widget::update(&mut self.ring, state, path) | moved
    }
}
