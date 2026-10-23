//! `chart`: a bar chart drawn from numbers, so a caller gives labels and
//! values rather than computing each bar's rect. The chart is ordinary marks
//! (rects, text, a line) in one group, drawn as one `draw`: everything that
//! works on marks works on it, and one undo takes it away.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema, Refusal};

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
// ratchet-allow(pixel_const): the chart is the person's content, in canvas units, not chrome
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
    pub fn draw(&self, art: &Artwork) -> Result<Draw, Refusal> {
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

    /// The values checked, and the value the box's full length stands for.
    fn scale(&self) -> Result<f64, Refusal> {
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
        match self.max {
            Some(max) if !max.is_finite() || max <= 0.0 => {
                Err(refuse(format!("max is a number above 0, not {max}")))
            }
            Some(max) if max < largest => Err(refuse(format!(
                "max {max} is under the largest value {largest}; leave it out to scale to the largest"
            ))),
            Some(max) => Ok(max),
            // All zero: no bars, labels still on the axis.
            None if largest > 0.0 => Ok(largest),
            None => Ok(1.0),
        }
    }

    /// The box, bar thickness and scale, checked.
    fn layout(&self, art: &Artwork) -> Result<Layout, Refusal> {
        let max = self.scale()?;
        let n = self.values.len();
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
        let shown = self.values_shown == Some(true);
        let mut top = height * 0.08;
        if self.title.as_deref().is_some_and(|title| !title.is_empty()) {
            top += height * 0.08;
        }
        if shown && !horizontal {
            top += height * 0.06;
        }
        let (left, right, bottom) = if horizontal {
            let right = if shown { 0.12 } else { 0.06 };
            (width * 0.22, width * right, height * 0.06)
        } else {
            (width * 0.08, width * 0.08, height * 0.14)
        };
        [left, top, width - left - right, height - top - bottom]
    }

    /// The id prefix: the one given, else the first of `chart`, `chart-2`, …
    /// no mark or group of `art` uses.
    fn prefix(&self, art: &Artwork) -> Result<String, Refusal> {
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
    pub(crate) fn apply(&self, doc: &mut Artwork, art: &Artwork) -> Result<(), Refusal> {
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
#[allow(
    clippy::float_cmp,
    reason = "whole-number boxes and values lay out exactly"
)]
mod tests {
    use crate::actions::{Draw, NewMark};
    use crate::model::{Artwork, Shape};
    use serde_json::{Value, json};
    use ui::prelude::{Replica, TestApp};

    fn rect_of(art: &Artwork, id: &str) -> [f64; 4] {
        match art.mark(id).map(|mark| &mark.shape) {
            Some(Shape::Rect {
                x,
                y,
                width,
                height,
                ..
            }) => [*x, *y, *width, *height],
            other => panic!("{id} is no rect: {other:?}"),
        }
    }

    fn chart(app: &mut TestApp<crate::App>, arguments: Value) -> Result<Value, String> {
        app.call("chart", arguments)
            .map(Option::unwrap_or_default)
            .map_err(|error| error.to_string())
    }

    #[test]
    fn bars_are_proportional_equal_and_stand_on_one_baseline() {
        let mut app = TestApp::<crate::App>::new().unwrap();
        let answer = chart(
            &mut app,
            json!({"labels": ["North", "South", "East"], "values": [40, 20, 10], "box": [100, 100, 600, 400], "title": "Q3", "values_shown": true}),
        )
        .unwrap();
        let added: Vec<&str> = answer["added"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            added.contains(&"chart-bar-1") && added.contains(&"chart-title"),
            "{answer}"
        );
        let art = app.state();
        let bars: Vec<[f64; 4]> = (1..=3)
            .map(|n| rect_of(&art, &format!("chart-bar-{n}")))
            .collect();
        assert_eq!(bars[0][3], 400.0, "the largest value fills the box");
        assert_eq!(bars[1][3], 200.0);
        assert_eq!(bars[2][3], 100.0);
        assert!(
            bars.iter().all(|bar| bar[1] + bar[3] == 500.0),
            "one baseline"
        );
        assert!(bars.iter().all(|bar| bar[2] == bars[0][2]), "equal widths");
        assert_eq!(
            art.members("chart").count(),
            answer["added"].as_array().unwrap().len()
        );
    }

    #[test]
    fn mismatched_or_invalid_chart_input_is_refused_saying_which() {
        let mut app = TestApp::<crate::App>::new().unwrap();
        let mismatch = chart(&mut app, json!({"labels": ["a"], "values": [1, 2]})).unwrap_err();
        assert!(mismatch.contains("one label per value"), "{mismatch}");
        let negative =
            chart(&mut app, json!({"labels": ["a", "b"], "values": [1, -2]})).unwrap_err();
        assert!(negative.contains("item 1 is -2"), "{negative}");
        let kind = chart(
            &mut app,
            json!({"labels": ["a"], "values": [1], "kind": "pie"}),
        )
        .unwrap_err();
        assert!(kind.contains("bar or hbar"), "{kind}");
    }

    #[test]
    fn a_second_chart_takes_the_next_free_prefix_and_hbar_runs_along_x() {
        let mut app = TestApp::<crate::App>::new().unwrap();
        chart(&mut app, json!({"labels": ["a"], "values": [1]})).unwrap();
        chart(
            &mut app,
            json!({"labels": ["done"], "values": [3], "max": 4, "kind": "hbar", "box": [0, 0, 400, 40]}),
        )
        .unwrap();
        let bar = rect_of(&app.state(), "chart-2-bar-1");
        assert_eq!(bar[2], 300.0, "three quarters of the box's width");
    }

    fn draw(marks: Vec<NewMark>) -> Result<Artwork, String> {
        let mut art = Replica::<Artwork>::new();
        art.apply(Artwork::DRAW.with(crate::resource::artwork::Draw {
            shapes: Draw { layer: None, marks },
        }))
        .map_err(|error| error.to_string())?;
        Ok(art.state().unwrap())
    }

    fn rect(id: Option<&str>, width: f64, height: f64) -> NewMark {
        NewMark {
            id: id.map(Into::into),
            ..NewMark::new(Shape::Rect {
                x: 90.0,
                y: 440.0,
                width,
                height,
                radius: 0.0,
            })
        }
    }

    #[test]
    fn a_rect_with_a_negative_side_reaches_back_from_its_corner() {
        let art = draw(vec![rect(Some("bar"), 120.0, -360.0)]).unwrap();
        assert_eq!(rect_of(&art, "bar"), [90.0, 80.0, 120.0, 360.0]);
    }

    #[test]
    fn a_shape_under_one_unit_is_refused_naming_the_mark_and_the_side() {
        let named = draw(vec![
            rect(Some("ok"), 10.0, 10.0),
            rect(Some("bar-south"), 40.0, 0.0),
        ])
        .unwrap_err();
        assert!(
            named.contains(
                "mark `bar-south` (item 1): the shape is under one unit across: height is 0;"
            ),
            "{named}"
        );
        let fractions = draw(vec![rect(None, 0.38, 0.5)]).unwrap_err();
        assert!(
            fractions.contains(
                "mark item 0: the shape is under one unit across: width is 0.38 and height is 0.5;"
            ),
            "{fractions}"
        );
        let ellipse = draw(vec![NewMark {
            id: Some("sun".into()),
            ..NewMark::new(Shape::Ellipse {
                cx: 50.0,
                cy: 50.0,
                rx: 20.0,
                ry: 0.2,
            })
        }])
        .unwrap_err();
        assert!(
            ellipse.contains("ry is 0.2") && ellipse.contains("each radius at least 0.5"),
            "{ellipse}"
        );
    }
}
