//! `chart`: a bar chart drawn from numbers, so a caller gives labels and
//! values rather than computing each bar's rect. The chart is ordinary marks
//! (rects, text, a line) in one group, drawn as one `draw`: everything that
//! works on marks works on it, and one undo takes it away.

use serde::{Deserialize, Serialize};
use ui::tilo_actions::ActionError;
use ui::tilo_resource::{self, Doc, JsonSchema};

use crate::actions::{Draw, NewMark, refuse};
use crate::model::{Artwork, Shape, valid_id};

/// Draw a bar chart from labels and values.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Chart {
    /// One per value.
    pub labels: Vec<String>,
    /// >= 0.
    pub values: Vec<f64>,
    /// bar or hbar.
    pub kind: Option<String>,
    /// Bars' area [x, y, w, h] on its bottom (hbar: left).
    #[serde(rename = "box")]
    pub area: Option<[f64; 4]>,
    /// Full-bar value (default largest).
    pub max: Option<f64>,
    /// Default: gap 2/3 of it.
    pub bar_width: Option<f64>,
    pub colour: Option<String>,
    pub label_size: Option<f64>,
    pub values_shown: Option<bool>,
    pub title: Option<String>,
    /// Prefix (default chart).
    pub id: Option<String>,
    pub layer: Option<String>,
}

/// The most charts an artwork names `chart`, `chart-2`, … by itself.
const MOST_CHARTS: u32 = 999;
/// The most bars one chart draws.
const MOST_BARS: usize = 200;
/// The bars' colour when none is given.
const BAR: &str = "#4e79a7";
/// Labels, values and the title.
const INK: &str = "#000000";
/// The axis the bars stand on.
const AXIS: &str = "#666666";
/// The axis line's width in units.
const AXIS_WIDTH: f64 = 2.0;
/// The gap between bars as a share of a bar's width, when it is not given.
const GAP: f64 = 2.0 / 3.0;

/// Where every bar goes: the plot box, along which axis, and how thick.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    /// `[x, y, w, h]`.
    area: [f64; 4],
    horizontal: bool,
    /// Across a bar: its width (a row's height for hbar).
    thickness: f64,
    /// From one bar's start to the next's.
    pitch: f64,
    /// Where the first bar starts across the axis.
    first: f64,
    /// The value the box's full length stands for.
    max: f64,
    label_size: f64,
}

impl Layout {
    /// The box's length along the bars.
    fn length(&self) -> f64 {
        let [_, _, w, h] = self.area;
        if self.horizontal { w } else { h }
    }

    /// The `index`th bar's rect `[x, y, w, h]`, its length exactly
    /// `value / max` of the box (a sliver under one unit is drawn one unit).
    fn bar(&self, index: usize, value: f64) -> [f64; 4] {
        let [x, y, _, h] = self.area;
        let length = if value > 0.0 {
            (value / self.max * self.length()).max(1.0)
        } else {
            0.0
        };
        let across = self.first + count(index) * self.pitch;
        if self.horizontal {
            [x, y + across, length, self.thickness]
        } else {
            [x + across, y + h - length, self.thickness, length]
        }
    }
}

/// `n` as a float; a chart's counts are small.
fn count(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

impl Chart {
    /// The marks of this chart on `art`, as one `draw`.
    ///
    /// # Errors
    /// Refuses mismatched or invalid values, box or id, saying which.
    pub fn draw(&self, art: &Artwork) -> Result<Draw, ActionError> {
        let layout = self.layout(art)?;
        let prefix = self.prefix(art)?;
        let id = |part: &str| format!("{prefix}-{part}");
        let mut marks = Vec::new();
        let size = layout.label_size;
        let [x, y, w, h] = layout.area;
        if let Some(title) = self.title.as_deref().filter(|title| !title.is_empty()) {
            let title_size = (size * 1.4).round();
            let above = if self.values_shown == Some(true) && !layout.horizontal {
                size * 1.4
            } else {
                0.0
            };
            marks.push(text(
                id("title"),
                (x + w / 2.0, y - above - title_size * 0.6),
                (title, title_size, "center"),
                Some(700),
            ));
        }
        // The axis just outside the box, so the bars cover none of it.
        let half = AXIS_WIDTH / 2.0;
        let points = if layout.horizontal {
            vec![[x - half, y], [x - half, y + h]]
        } else {
            vec![[x, y + h + half], [x + w, y + h + half]]
        };
        marks.push(NewMark {
            id: Some(id("axis")),
            stroke: Some(AXIS.into()),
            stroke_width: Some(AXIS_WIDTH),
            ..NewMark::new(Shape::Line { points })
        });
        let colour = self.colour.clone().unwrap_or_else(|| BAR.into());
        for (index, (label, value)) in self.labels.iter().zip(&self.values).enumerate() {
            let place = index.saturating_add(1);
            let bar = layout.bar(index, *value);
            if *value > 0.0 {
                let name = Some(label.clone()).filter(|label| !label.is_empty());
                marks.push(rect(id(&format!("bar-{place}")), bar, colour.clone(), name));
            }
            marks.extend(self.texts(&layout, bar, (label, *value), &|part| {
                id(&format!("{part}-{place}"))
            }));
        }
        Ok(Draw {
            layer: self.layer.clone(),
            marks: marks
                .into_iter()
                .map(|mark| NewMark {
                    group: Some(prefix.clone()),
                    ..mark
                })
                .collect(),
        })
    }

    /// A bar's label beside its base and, when shown, its value at its end.
    fn texts(
        &self,
        layout: &Layout,
        [bx, by, bw, bh]: [f64; 4],
        (label, value): (&str, f64),
        id: &dyn Fn(&str) -> String,
    ) -> Vec<NewMark> {
        let size = layout.label_size;
        let [x, y, _, h] = layout.area;
        let mut marks = Vec::new();
        let (label_at, label_align, value_at, value_align) = if layout.horizontal {
            let middle = by + bh / 2.0 + size * 0.35;
            (
                (x - AXIS_WIDTH - size * 0.4, middle),
                "end",
                (bx + bw + size * 0.4, middle),
                "start",
            )
        } else {
            let centre = bx + bw / 2.0;
            (
                (centre, y + h + size * 1.2),
                "center",
                (centre, by - size * 0.35),
                "center",
            )
        };
        if !label.is_empty() {
            marks.push(text(
                id("label"),
                label_at,
                (label, size, label_align),
                None,
            ));
        }
        if self.values_shown == Some(true) {
            marks.push(text(
                id("value"),
                value_at,
                (&number(value), size, value_align),
                None,
            ));
        }
        marks
    }

    /// The box, bar thickness and scale, checked.
    fn layout(&self, art: &Artwork) -> Result<Layout, ActionError> {
        let n = self.values.len();
        if n == 0 {
            return Err(refuse("give at least one value"));
        }
        if self.labels.len() != n {
            return Err(refuse(format!(
                "labels has {} items and values {n}; give one label per value",
                self.labels.len()
            )));
        }
        if n > MOST_BARS {
            return Err(refuse(format!("at most {MOST_BARS} bars per chart")));
        }
        if let Some((index, value)) = self
            .values
            .iter()
            .enumerate()
            .find(|(_, value)| !value.is_finite() || **value < 0.0)
        {
            return Err(refuse(format!(
                "values are finite numbers >= 0; item {index} is {value}"
            )));
        }
        let largest = self.values.iter().copied().fold(0.0, f64::max);
        let max = match self.max {
            Some(max) if !max.is_finite() || max <= 0.0 => {
                return Err(refuse(format!("max is a number above 0, not {max}")));
            }
            Some(max) if max < largest => {
                return Err(refuse(format!(
                    "max {max} is under the largest value {largest}; leave it out to scale to the largest"
                )));
            }
            Some(max) => max,
            // All zero: no bars, labels still on the axis.
            None if largest > 0.0 => largest,
            None => 1.0,
        };
        let horizontal = match self.kind.as_deref() {
            None | Some("bar") => false,
            Some("hbar") => true,
            Some(other) => return Err(refuse(format!("kind {other:?} is bar or hbar"))),
        };
        let area = self
            .area
            .unwrap_or_else(|| self.default_area(art, horizontal));
        let [_, _, w, h] = area;
        if !area.iter().all(|v| v.is_finite()) || w < 1.0 || h < 1.0 {
            return Err(refuse("box is [x, y, w, h] in units, w and h at least 1"));
        }
        let across = if horizontal { h } else { w };
        let thickness = match self.bar_width {
            Some(width) if !width.is_finite() || width < 1.0 => {
                return Err(refuse("bar_width is at least 1 unit"));
            }
            Some(width) if width * count(n) > across + 1e-9 => {
                return Err(refuse(format!(
                    "{n} bars {width} wide do not fit in the box's {across}"
                )));
            }
            Some(width) => width,
            None => across / (count(n) + count(n.saturating_sub(1)) * GAP),
        };
        // Bars edge to edge: equal gaps between, none outside; one bar sits
        // in the middle.
        let (pitch, first) = if n > 1 {
            let gap = (across - thickness * count(n)) / count(n.saturating_sub(1));
            (thickness + gap, 0.0)
        } else {
            (0.0, (across - thickness) / 2.0)
        };
        let label_size = match self.label_size {
            Some(size) if !size.is_finite() || size <= 0.0 => {
                return Err(refuse("label_size is above 0"));
            }
            Some(size) => size,
            None => (art.width.min(art.height) / 25.0).clamp(10.0, 48.0).round(),
        };
        Ok(Layout {
            area,
            horizontal,
            thickness,
            pitch,
            first,
            max,
            label_size,
        })
    }

    /// The artwork less margins for the labels, the title and the values.
    fn default_area(&self, art: &Artwork, horizontal: bool) -> [f64; 4] {
        let (width, height) = (art.width, art.height);
        let mut top = height * 0.08;
        if self.title.as_deref().is_some_and(|title| !title.is_empty()) {
            top += height * 0.08;
        }
        if self.values_shown == Some(true) && !horizontal {
            top += height * 0.06;
        }
        let (left, right, bottom) = if horizontal {
            (
                width * 0.22,
                width
                    * if self.values_shown == Some(true) {
                        0.12
                    } else {
                        0.06
                    },
                height * 0.06,
            )
        } else {
            (width * 0.08, width * 0.08, height * 0.14)
        };
        [left, top, width - left - right, height - top - bottom]
    }

    /// The id prefix: the one given, else the first of `chart`, `chart-2`, …
    /// no mark or group of `art` uses.
    fn prefix(&self, art: &Artwork) -> Result<String, ActionError> {
        if let Some(id) = &self.id {
            return if valid_id(id) && id.len() <= 36 {
                Ok(id.clone())
            } else {
                Err(refuse(format!(
                    "id {id:?} must be 1-36 of a-z, 0-9, '-' and '_'"
                )))
            };
        }
        let used = |prefix: &str| {
            let start = format!("{prefix}-");
            art.marks
                .iter()
                .any(|mark| mark.id == prefix || mark.id.starts_with(&start))
                || art.members(prefix).next().is_some()
        };
        Ok((1..=MOST_CHARTS)
            .map(|n: u32| {
                if n == 1 {
                    "chart".to_owned()
                } else {
                    format!("chart-{n}")
                }
            })
            .find(|prefix| !used(prefix))
            .unwrap_or_else(|| "chart".to_owned()))
    }

    /// Draw this chart on `doc`, as one step.
    ///
    /// # Errors
    /// As `draw`, or the draw's own refusal.
    pub fn apply(&self, doc: &mut Doc, art: &Artwork) -> Result<(), ActionError> {
        self.draw(art)?.apply(doc, art)
    }
}

fn rect(
    id: String,
    [x, y, width, height]: [f64; 4],
    fill: String,
    name: Option<String>,
) -> NewMark {
    NewMark {
        id: Some(id),
        name,
        fill: Some(fill),
        ..NewMark::new(Shape::Rect {
            x,
            y,
            width,
            height,
            radius: 0.0,
        })
    }
}

fn text(
    id: String,
    (x, y): (f64, f64),
    (text, size, align): (&str, f64, &str),
    weight: Option<u16>,
) -> NewMark {
    NewMark {
        id: Some(id),
        fill: Some(INK.into()),
        ..NewMark::new(Shape::Text {
            x,
            y,
            text: text.into(),
            size,
            font: None,
            weight,
            italic: false,
            align: Some(align.into()),
            line_height: None,
            width: None,
            tracking: None,
            path: None,
        })
    }
}

/// A value as written above its bar: whole numbers without a point, others
/// to at most two decimals.
fn number(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use ui::tilo_actions::{Action as _, ActionArgs};
    use ui::tilo_resource::Resource as _;

    use super::*;
    use crate::actions::{CanvasAction, SetArtwork};
    use crate::model::Mark;
    use crate::resource::ArtworkResource;

    /// A white artwork `width` by `height`.
    fn artwork(width: f64, height: f64) -> Doc {
        let mut doc = Doc::new();
        ArtworkResource::initialize(&mut doc).expect("a new artwork");
        CanvasAction::SetArtwork(SetArtwork {
            width: Some(width),
            height: Some(height),
            texture: Some("smooth".into()),
            ground_colour: Some("#ffffff".into()),
            ..SetArtwork::default()
        })
        .apply(&mut doc)
        .expect("sized");
        doc
    }

    /// `chart` as an agent invokes it by name: arguments bound against its
    /// published parameters, built, then applied.
    fn chart(doc: &mut Doc, arguments: &Value) -> Result<(), String> {
        let declarations = ArtworkResource::declarations();
        let chart = declarations
            .iter()
            .find(|decl| decl.id == "chart")
            .expect("chart is declared");
        let args = ActionArgs::bind_json(&chart.params, arguments.as_object().expect("an object"))
            .map_err(|error| error.to_string())?;
        (chart.build)(&args, 0)?
            .apply(doc)
            .map_err(|error| error.to_string())
    }

    fn mark<'a>(art: &'a Artwork, id: &str) -> &'a Mark {
        art.mark(id).unwrap_or_else(|| panic!("no mark {id}"))
    }

    fn rect_of(art: &Artwork, id: &str) -> [f64; 4] {
        match &mark(art, id).shape {
            Shape::Rect {
                x,
                y,
                width,
                height,
                ..
            } => [*x, *y, *width, *height],
            other => panic!("{id} is {other:?}"),
        }
    }

    fn text_of(art: &Artwork, id: &str) -> (f64, f64, String, Option<String>) {
        match &mark(art, id).shape {
            Shape::Text {
                x, y, text, align, ..
            } => (*x, *y, text.clone(), align.clone()),
            other => panic!("{id} is {other:?}"),
        }
    }

    fn near(a: [f64; 4], b: [f64; 4]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-9)
    }

    const TOTALS: [f64; 4] = [402.0, 284.0, 454.0, 309.0];

    fn q3() -> Value {
        json!({
            "labels": ["North", "South", "East", "West"],
            "values": TOTALS,
            "box": [90, 80, 720, 360],
            "colour": "#2A6F97",
            "title": "Q3 totals",
        })
    }

    /// The x3 chart: bars 120 wide at 90, 290, 490 and 690 on y = 440, the
    /// tallest 360, the rest exactly in proportion, each label centred
    /// under its bar.
    #[test]
    fn bars_are_proportional_equal_and_on_one_baseline() {
        let mut doc = artwork(900.0, 500.0);
        chart(&mut doc, &q3()).expect("drawn");
        let art = crate::doc::read(&doc);
        for (index, value) in TOTALS.iter().enumerate() {
            let n = index + 1;
            let [x, y, width, height] = rect_of(&art, &format!("chart-bar-{n}"));
            assert!(
                (x - (90.0 + 200.0 * count(index))).abs() < 1e-9,
                "bar {n} x {x}"
            );
            assert!((width - 120.0).abs() < 1e-9, "bar {n} width {width}");
            assert!((y + height - 440.0).abs() < 1e-9, "bar {n} stands on 440");
            assert!(
                (height - 360.0 * value / 454.0).abs() < 1e-9,
                "bar {n} height {height}"
            );
            let (lx, ly, label, align) = text_of(&art, &format!("chart-label-{n}"));
            assert!((lx - (x + 60.0)).abs() < 1e-9, "label {n} centred");
            assert!(ly > 440.0 && ly < 500.0, "label {n} under the bar at {ly}");
            assert_eq!(align.as_deref(), Some("center"));
            assert_eq!(label, ["North", "South", "East", "West"][index]);
            assert_eq!(
                mark(&art, &format!("chart-bar-{n}")).style.fill.as_deref(),
                Some("#2A6F97")
            );
        }
        let (_, title_y, title, _) = text_of(&art, "chart-title");
        assert!(
            title == "Q3 totals" && title_y < 80.0,
            "{title} at {title_y}"
        );
        // The axis runs under the bars, never in their colour.
        let axis = mark(&art, "chart-axis");
        assert!(
            matches!(&axis.shape, Shape::Line { points } if points.iter().all(|p| p[1] > 440.0))
        );
        assert_ne!(axis.style.stroke.as_deref(), Some("#2A6F97"));
        // One group, one undoable step.
        assert_eq!(art.members("chart").count(), 4 + 4 + 2);
        assert_eq!(art.marks.len(), 10);
    }

    /// `max` sets what the box's full height stands for; zero draws no bar,
    /// its label still in place; hbar lays the same rules along x.
    #[test]
    fn max_scales_zero_draws_nothing_and_hbar_runs_along_x() {
        let mut doc = artwork(800.0, 400.0);
        chart(
            &mut doc,
            &json!({"labels": ["a", "b", "c"], "values": [50, 0, 25], "max": 100,
                    "box": [100, 50, 600, 300], "values_shown": true}),
        )
        .expect("drawn");
        let art = crate::doc::read(&doc);
        assert!((rect_of(&art, "chart-bar-1")[3] - 150.0).abs() < 1e-9);
        assert!((rect_of(&art, "chart-bar-3")[3] - 75.0).abs() < 1e-9);
        assert!(art.mark("chart-bar-2").is_none(), "a zero draws no bar");
        assert_eq!(text_of(&art, "chart-label-2").2, "b");
        assert_eq!(text_of(&art, "chart-value-1").2, "50");
        assert_eq!(text_of(&art, "chart-value-2").2, "0");
        // A second chart takes the next free prefix.
        chart(
            &mut doc,
            &json!({"kind": "hbar", "labels": ["x", "y"], "values": [3, 1.5],
                    "box": [100, 50, 600, 300], "bar_width": 100}),
        )
        .expect("drawn");
        let art = crate::doc::read(&doc);
        let first = rect_of(&art, "chart-2-bar-1");
        let second = rect_of(&art, "chart-2-bar-2");
        assert!(near(first, [100.0, 50.0, 600.0, 100.0]), "{first:?}");
        assert!(near(second, [100.0, 250.0, 300.0, 100.0]), "{second:?}");
        let (lx, _, _, align) = text_of(&art, "chart-2-label-1");
        assert!(lx < 100.0 && align.as_deref() == Some("end"));
    }

    /// One hbar fills the box's height: a progress bar.
    #[test]
    fn one_hbar_fills_the_box_like_a_progress_bar() {
        let mut doc = artwork(800.0, 200.0);
        chart(
            &mut doc,
            &json!({"kind": "hbar", "labels": [""], "values": [0.63], "max": 1,
                    "box": [50, 80, 700, 40], "colour": "#2E7D32"}),
        )
        .expect("drawn");
        let art = crate::doc::read(&doc);
        let bar = rect_of(&art, "chart-bar-1");
        assert!(near(bar, [50.0, 80.0, 441.0, 40.0]), "{bar:?}");
        assert!(
            art.mark("chart-label-1").is_none(),
            "an empty label writes nothing"
        );
    }

    /// Wrong input is refused saying what to give, and draws nothing.
    #[test]
    fn mismatched_or_invalid_input_is_refused_clearly() {
        let mut doc = artwork(900.0, 500.0);
        let refused =
            |doc: &mut Doc, arguments: Value| chart(doc, &arguments).expect_err("refused");
        assert_eq!(
            refused(&mut doc, json!({"labels": ["a", "b"], "values": [1, 2, 3]})),
            "labels has 2 items and values 3; give one label per value"
        );
        assert!(
            refused(&mut doc, json!({"labels": [], "values": []})).contains("at least one value")
        );
        assert!(
            refused(&mut doc, json!({"labels": ["a"], "values": [-1]})).contains("item 0 is -1")
        );
        assert!(
            refused(&mut doc, json!({"labels": ["a"], "values": [5], "max": 4}))
                .contains("under the largest value 5")
        );
        assert!(
            refused(
                &mut doc,
                json!({"labels": ["a"], "values": [5], "kind": "pie"})
            )
            .contains("bar or hbar")
        );
        assert!(
            refused(
                &mut doc,
                json!({"labels": ["a", "b"], "values": [1, 2], "colour": "blue-ish"})
            )
            .contains("is not a colour")
        );
        assert!(
            refused(&mut doc, json!({"labels": ["a", "b"], "values": [1, 2], "box": [0, 0, 100, 100], "bar_width": 60}))
                .contains("do not fit")
        );
        assert!(crate::doc::read(&doc).marks.is_empty());
    }

    /// Without a box the chart fits the artwork with room for its text.
    #[test]
    fn the_default_box_keeps_the_chart_and_its_labels_on_the_artwork() {
        let mut doc = artwork(900.0, 500.0);
        chart(
            &mut doc,
            &json!({"labels": ["a", "b"], "values": [2, 1], "title": "T"}),
        )
        .expect("drawn");
        let art = crate::doc::read(&doc);
        let [x, y, width, height] = rect_of(&art, "chart-bar-1");
        assert!(x > 0.0 && y > 0.0 && x + width < 900.0 && y + height < 500.0);
        let (_, ly, _, _) = text_of(&art, "chart-label-1");
        let (_, ty, _, _) = text_of(&art, "chart-title");
        assert!(ly < 500.0 && ty > 0.0 && ty < y);
    }

    /// The exported PNG shows the bars as solid rects of their colour, on
    /// one baseline, at their heights, with dark labels under them.
    #[test]
    fn an_exported_chart_shows_its_bars() {
        let mut doc = artwork(900.0, 500.0);
        chart(&mut doc, &q3()).expect("drawn");
        let art = crate::doc::read(&doc);
        let mut renderer = crate::render::Renderer::default();
        let (picture, _) = crate::view_ops::export(
            &mut renderer,
            &mut ui::TextRenderer::new(),
            &art,
            &crate::resource::Export::default(),
        )
        .expect("exported");
        assert_eq!(picture.format, "png");
        let png = image::load_from_memory(&picture.bytes)
            .expect("a png")
            .to_rgba8();
        assert_eq!(png.dimensions(), (900, 500));
        let bar = |p: &image::Rgba<u8>| {
            let [r, g, b, _] = p.0;
            r.abs_diff(0x2A) <= 40 && g.abs_diff(0x6F) <= 40 && b.abs_diff(0x97) <= 40
        };
        for (index, value) in TOTALS.iter().enumerate() {
            let left = 90 + 200 * u32::try_from(index).expect("small");
            let centre = left + 60;
            let column: Vec<u32> = (0..500)
                .filter(|&y| bar(png.get_pixel(centre, y)))
                .collect();
            let (top, bottom) = (column[0], column[column.len() - 1] + 1);
            let want = 360.0 * value / 454.0;
            assert_eq!(bottom, 440, "bar {index} stands on 440");
            assert!(
                (f64::from(bottom - top) - want).abs() <= 1.0,
                "bar {index}: {top}..{bottom}, want {want}"
            );
            assert_eq!(
                u32::try_from(column.len()).expect("small"),
                bottom - top,
                "bar {index} is solid"
            );
            let row = 430;
            let across: Vec<u32> = (0..900)
                .filter(|&x| bar(png.get_pixel(x, row)) && x >= left - 10 && x < left + 130)
                .collect();
            assert!(
                across.len().abs_diff(120) <= 1,
                "bar {index} is 120 wide: {}",
                across.len()
            );
            let dark = (444..500)
                .flat_map(|y| (left - 20..left + 140).map(move |x| (x, y)))
                .filter(|&(x, y)| png.get_pixel(x, y).0[..3].iter().all(|c| *c < 110))
                .count();
            assert!(dark >= 40, "a label under bar {index}: {dark} dark pixels");
        }
        // Nothing else in the bar colour.
        let stray = png
            .enumerate_pixels()
            .filter(|(x, y, p)| {
                let in_a_bar = (80..440).contains(y)
                    && (0..4).any(|i| (90 + 200 * i..210 + 200 * i).contains(x));
                bar(p) && !in_a_bar
            })
            .count();
        assert_eq!(stray, 0, "bar colour outside the bars");
    }
}
