//! What the engine's own theme paints of a form control, drawn from what a
//! layout snapshot says of the control: the hairline of a field the page
//! left alone, a select's mark, the picker of a date, a ticked box in the
//! page's accent, a slider's thumb, the ring around the field being edited.
//! Each is an item a client already draws (a fill, a border, a vector), so a
//! client that knows nothing of controls draws them all the same.
//!
//! What a field holds is drawn as text in the room the field has for it
//! ([`SLOT`]), never what a secret field holds: of that a client is sent a
//! dot for each character and no character.

use super::{
    CHOICE_BOX, CHOICE_MARK, Converter, FIELD_MARK, FIELD_VALUE, MAX_FIELD_LINES, OUTLINE,
    PART_CHOICE_FILL, PART_FIELD, PART_LINE, PLACEHOLDER, S, SLOT_CENTRE, SLOT_END, css_color, px,
};
use crate::model::{Color, Item, ItemKey, Rect, Shape};

/// The edge the engine's theme draws a control with.
const EDGE: Color = Color([118, 118, 118, 255]);
/// What a ticked box is filled with where its page names no accent.
const ACCENT: Color = Color([0, 117, 255, 255]);
const WHITE: Color = Color([255, 255, 255, 255]);
/// The groove a slider's thumb runs in.
const TRACK: Color = Color([239, 239, 239, 255]);
/// How much of its colour a control keeps that takes no answer.
const DISABLED: f32 = 0.3;
/// One dot for each character of a secret.
const DOT: &str = "\u{2022}";

/// The edge of a box: where its background and border are drawn, and with
/// what.
pub(super) struct Edge {
    pub rect: Rect,
    pub radius: f32,
    pub widths: [f32; 4],
    pub colors: [Color; 4],
    /// The inner half of a border drawn in two tones, and its colours.
    pub inner: Option<(Rect, [Color; 4])>,
    /// Where the border is not drawn: behind a fieldset's legend.
    pub gap: Option<Rect>,
}

fn faded(color: Color, keep: f32) -> Color {
    let [r, g, b, a] = color.0;
    Color([r, g, b, super::byte(f32::from(a) * keep)])
}

fn shaded(color: Color, keep: f32) -> Color {
    let [r, g, b, a] = color.0;
    let shade = |channel: u8| super::byte(f32::from(channel) * keep);
    Color([shade(r), shade(g), shade(b), a])
}

fn stroke(path: &str, color: Color, width: f32) -> Shape {
    Shape {
        path: path.to_owned(),
        fill: None,
        stroke: Some(color),
        stroke_width: width,
        even_odd: false,
    }
}

/// `text` as it fits a field `room` wide, set at `size`: what is past the
/// field's end is cut, as the field shows it, and marked as cut. A client
/// shapes the text itself, so the room is counted by an average letter, and
/// a client draws narrower what is a little long.
fn fitted(text: &str, size: f32, room: f32) -> String {
    let letters = (room / (size * 0.5)).max(1.0);
    let held = text.chars().count();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a count of letters a field is wide: at least one, far below any integer's range"
    )]
    let most = letters.floor() as usize;
    if f32::from(u16::try_from(held).unwrap_or(u16::MAX)) <= letters * 1.6 {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(most.saturating_sub(1).max(1)).collect();
    cut.push('\u{2026}');
    cut
}

/// `#rrggbb`, as a colour field holds its colour.
fn hex_color(value: &str) -> Option<Color> {
    let digits = value.strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(digits.get(at..at.checked_add(2)?)?, 16).ok();
    Some(Color([channel(0)?, channel(2)?, channel(4)?, 255]))
}

impl Converter<'_> {
    fn input_kind(&self, node: i64) -> String {
        self.doc
            .attr(node, "type")
            .unwrap_or("")
            .to_ascii_lowercase()
    }

    /// Whether what `node` holds is a secret by what the field is: a
    /// password field, or one that was; one its page says holds a card's
    /// number or security code, a password or a one-time code; one its page
    /// draws as dots. The rule is the one the tab's record keeps.
    fn secret(&self, node: i64, li: usize) -> bool {
        if self.input_kind(node) == "password"
            || self.told.secret.contains(&self.doc.node_key(node))
        {
            return true;
        }
        let said = self.doc.attr(node, "autocomplete").unwrap_or("");
        let named = said.split_whitespace().any(|token| {
            let token = token.to_ascii_lowercase();
            (token.len() > 3 && token.starts_with("cc-"))
                || matches!(
                    token.as_str(),
                    "one-time-code" | "current-password" | "new-password"
                )
        });
        named || !matches!(self.doc.style(li, S::TextSecurity), "" | "none")
    }

    /// Whether the control takes no answer now.
    fn disabled(&self, node: i64) -> bool {
        self.doc.attr(node, "disabled").is_some()
    }

    /// Where `node`'s background and border are drawn, and with what. A
    /// field, a select or a button the page left alone is drawn by the
    /// engine's theme with a hairline and a small radius, whatever border
    /// its style computes to; a fieldset's border starts halfway down its
    /// legend and is not drawn behind it; a groove or a ridge is two tones.
    pub(super) fn edge(&self, node: i64, li: usize, rect: Rect) -> Edge {
        let name = self.doc.name(node);
        let mut radius = self.len(li, S::Radius);
        let mut widths = [
            S::BorderTopW,
            S::BorderRightW,
            S::BorderBottomW,
            S::BorderLeftW,
        ]
        .map(|s| self.len(li, s));
        let mut colors = [
            S::BorderTopC,
            S::BorderRightC,
            S::BorderBottomC,
            S::BorderLeftC,
        ]
        .map(|s| css_color(self.doc.style(li, s)).unwrap_or_default());
        let style = self.doc.style(li, S::BorderTopStyle);
        let boxed = match name {
            "TEXTAREA" | "SELECT" | "BUTTON" => true,
            "INPUT" => !matches!(
                self.input_kind(node).as_str(),
                "checkbox" | "radio" | "range" | "color" | "file" | "image" | "hidden"
            ),
            _ => false,
        };
        let mut edge = Edge {
            rect,
            radius,
            widths,
            colors,
            inner: None,
            gap: None,
        };
        if boxed && self.themed(li) {
            if radius <= 0.0 {
                radius = 2.0 * self.zoom;
            }
            if matches!(style, "inset" | "outset") {
                widths = [self.zoom; 4];
                colors = colors.map(|color| faded(EDGE, f32::from(color.0[3]) / 255.0));
            }
            return Edge {
                radius,
                widths,
                colors,
                ..edge
            };
        }
        if name == "FIELDSET"
            && widths[0] > 0.0
            && let Some(legend) = self.legend(node)
        {
            let lift = (legend.y + legend.h / 2.0 - widths[0] / 2.0 - rect.y).clamp(0.0, rect.h);
            edge.rect = Rect::new(rect.x, rect.y + lift, rect.w, rect.h - lift);
            edge.gap = Some(Rect::new(legend.x, edge.rect.y, legend.w, widths[0]));
        }
        if matches!(style, "groove" | "ridge") && widths.iter().all(|width| *width >= 2.0) {
            let dark = colors.map(|color| shaded(color, 0.67));
            let (outer, inner) = if style == "groove" {
                (dark, colors)
            } else {
                (colors, dark)
            };
            let half = widths.map(|width| width / 2.0);
            let [top, right, bottom, left] = half;
            let within = Rect::new(
                edge.rect.x + left,
                edge.rect.y + top,
                (edge.rect.w - left - right).max(0.0),
                (edge.rect.h - top - bottom).max(0.0),
            );
            edge.widths = half;
            edge.colors = outer;
            edge.inner = Some((within, inner));
        }
        edge
    }

    /// The box of a fieldset's legend.
    fn legend(&self, fieldset: i64) -> Option<Rect> {
        let legend = self
            .doc
            .children
            .get(&fieldset)?
            .iter()
            .find(|child| self.doc.name(**child) == "LEGEND")?;
        let li = self.doc.layout_of.get(legend)?;
        self.doc
            .bounds
            .get(*li)
            .copied()
            .flatten()
            .filter(|held| held.w > 0.0 && held.h > 0.0)
    }

    /// The ring an element is outlined with. The engine draws its own around
    /// the element that has the focus, which is how whoever watches tells
    /// which field is being edited; a page draws others. The engine's own
    /// ring is two pixels wide whatever width its style computes to.
    pub(super) fn outline(&mut self, node: i64, li: usize, order: i64, rect: Rect, radius: f32) {
        let style = self.doc.style(li, S::OutlineStyle);
        if matches!(style, "" | "none") {
            return;
        }
        let Some(color) = css_color(self.doc.style(li, S::OutlineColor)) else {
            return;
        };
        let width = if style == "auto" {
            self.len(li, S::OutlineWidth).max(2.0 * self.zoom)
        } else {
            self.len(li, S::OutlineWidth)
        };
        if width <= 0.0 {
            return;
        }
        let grow = width + self.len(li, S::OutlineOffset);
        let ring = Rect::new(
            rect.x - grow,
            rect.y - grow,
            grow.mul_add(2.0, rect.w).max(0.0),
            grow.mul_add(2.0, rect.h).max(0.0),
        );
        if ring.w <= 0.0 || ring.h <= 0.0 {
            return;
        }
        let key = ItemKey {
            node: self.doc.node_key(node),
            part: OUTLINE,
        };
        self.push_decoration(
            node,
            order,
            6,
            Item::Border {
                key,
                rect: ring,
                widths: [width; 4],
                colors: [color; 4],
                radius: (radius + grow).max(0.0),
            },
        );
    }

    /// Form fields draw their value through the engine's shadow tree, which
    /// the snapshot omits: draw value or placeholder from the node instead.
    ///
    /// What a viewer is shown is what the page shows: the text of an input,
    /// the lines of a textarea, the option a select shows with the mark that
    /// says it opens, a date as the engine words it, a slider's thumb where
    /// its value puts it, and whether a checkbox or a radio button is chosen.
    pub(super) fn field(&mut self, node: i64, li: usize, order: i64, rect: Rect) {
        let name = self.doc.name(node);
        let kind = self.input_kind(node);
        let themed = self.themed(li);
        if name == "TEXTAREA" {
            self.lines(node, li, order, rect);
            return;
        }
        if name == "INPUT" {
            match kind.as_str() {
                "radio" => return self.choice(node, li, order, rect, true),
                "checkbox" => return self.choice(node, li, order, rect, false),
                "range" if themed => return self.slider(node, li, order, rect),
                "color" if themed => return self.swatch(node, li, order, rect),
                // A picture: what it holds is no text it shows.
                "image" => return,
                _ => {}
            }
        }
        let select = name == "SELECT";
        let chosen;
        let held = if select {
            // A list shown open lays its options out as boxes of its own.
            let open = self.doc.attr(node, "multiple").is_some()
                || self
                    .doc
                    .attr(node, "size")
                    .and_then(|size| size.trim().parse::<u32>().ok())
                    .is_some_and(|size| size > 1);
            if open {
                return;
            }
            chosen = self.doc.chosen_option(node);
            chosen.as_str()
        } else {
            self.doc.input_values.get(&node).copied().unwrap_or("")
        };
        let node_key = self.doc.node_key(node);
        let picker = match kind.as_str() {
            "date" | "datetime-local" | "month" | "week" => Some(false),
            "time" => Some(true),
            _ => None,
        };
        let fg = css_color(self.doc.style(li, S::Fg)).unwrap_or_default();
        let shown = self.shown(node, li, (kind.as_str(), select), held);
        let font = self.font(li);
        let start = self.len(li, S::PaddingLeft) + self.len(li, S::BorderLeftW);
        let end = self.len(li, S::PaddingRight) + self.len(li, S::BorderRightW);
        let mark =
            if themed && (select || picker.is_some()) && rect.w > 16.0 * self.zoom + start + end {
                self.opening_mark(node, order, (rect, end), picker, fg)
            } else {
                0.0
            };
        let Some(shown) = shown else {
            return;
        };
        let (text, color) = if shown.is_empty() {
            let placeholder = self.doc.attr(node, "placeholder").unwrap_or("");
            (placeholder.to_owned(), PLACEHOLDER)
        } else {
            (shown, fg)
        };
        if text.is_empty() {
            return;
        }
        let line = (font.size * 1.25).min(rect.h);
        let room = (rect.w - start - end - mark).max(0.0);
        let side = self.side(li, matches!(kind.as_str(), "submit" | "reset" | "button"));
        self.push(
            node,
            order,
            3,
            Item::Text {
                key: ItemKey {
                    node: node_key,
                    part: PART_FIELD | side,
                },
                rect: Rect::new(rect.x + start, rect.y + (rect.h - line) / 2.0, room, line),
                text: fitted(&text, font.size, room),
                font,
                color,
            },
        );
    }

    /// What a field shows: what it holds, a dot for each character where
    /// that is a secret, and what the engine said where the engine words it.
    /// `None` while the engine has still to say. `kind` is the field's type
    /// and whether it is a select.
    fn shown(&mut self, node: i64, li: usize, kind: (&str, bool), held: &str) -> Option<String> {
        let (kind, select) = kind;
        let node_key = self.doc.node_key(node);
        if kind == "password" {
            self.passwords.push(node_key);
        }
        // A secret never leaves the host: viewers see a dot for each
        // character, as a page shows a password, and no character.
        if !select && self.secret(node, li) {
            return Some(DOT.repeat(held.chars().count()));
        }
        // What these show is not what they hold: the engine words a date, a
        // time and a number for whoever reads the page, and a button the
        // page gave no words has the engine's.
        let asked = !select
            && (matches!(
                kind,
                "date" | "datetime-local" | "month" | "week" | "time" | "number"
            ) || (matches!(kind, "submit" | "reset")
                && self.doc.attr(node, "value").is_none()));
        if !asked {
            return Some(held.to_owned());
        }
        self.asks.push((node_key, held.to_owned()));
        match self.told.shown.get(&node_key) {
            Some((value, said)) if value == held => {
                Some(said.clone().unwrap_or_else(|| held.to_owned()))
            }
            _ if self.told.asking => None,
            _ => Some(held.to_owned()),
        }
    }

    /// The side of its room a field's text is drawn from, as a part's bits:
    /// the end for a field written right to left or aligned so, the middle
    /// for the words on a button.
    fn side(&self, li: usize, button: bool) -> u32 {
        let right_to_left = self.doc.style(li, S::Direction) == "rtl";
        match self.doc.style(li, S::TextAlign) {
            "center" | "-webkit-center" => SLOT_CENTRE,
            "right" | "-webkit-right" => SLOT_END,
            "end" if !right_to_left => SLOT_END,
            "left" | "-webkit-left" | "end" => 0,
            _ if button => SLOT_CENTRE,
            _ if right_to_left => SLOT_END,
            _ => 0,
        }
    }

    /// The mark that says a field opens, at the field's end: a select's
    /// drop-down mark, or the picker of a date (`Some(false)`) or of a time
    /// (`Some(true)`). `field` is the field's box and what its end keeps
    /// clear. The room the mark takes of the field is returned: the field's
    /// text does not run under it.
    fn opening_mark(
        &mut self,
        node: i64,
        order: i64,
        field: (Rect, f32),
        picker: Option<bool>,
        fg: Color,
    ) -> f32 {
        let (rect, end) = field;
        let room = 16.0 * self.zoom;
        let side = (rect.h - 4.0 * self.zoom).clamp(1.0, room);
        let at = Rect::new(
            rect.x + rect.w - end.max(self.zoom) - room + (room - side) / 2.0,
            rect.y + (rect.h - side) / 2.0,
            side,
            side,
        );
        let shapes = match picker {
            // A drop-down mark.
            None => vec![stroke("M4 6L8 10L12 6", fg, 1.5)],
            // A clock.
            Some(true) => vec![
                stroke(&super::ellipse(8.0, 8.0, 5.5, 5.5), fg, 1.2),
                stroke("M8 4.5V8L10.5 9.5", fg, 1.2),
            ],
            // A calendar.
            Some(false) => vec![
                stroke("M2.5 3.5H13.5V13.5H2.5Z", fg, 1.2),
                Shape {
                    path: "M2.5 3.5H13.5V6.5H2.5Z".to_owned(),
                    fill: Some(fg),
                    stroke: None,
                    stroke_width: 0.0,
                    even_odd: false,
                },
                stroke("M5 1.5V3.5M11 1.5V3.5", fg, 1.2),
            ],
        };
        self.push(
            node,
            order,
            3,
            Item::Vector {
                key: ItemKey {
                    node: self.doc.node_key(node),
                    part: FIELD_MARK,
                },
                rect: at,
                view_box: Rect::new(0.0, 0.0, 16.0, 16.0),
                shapes,
            },
        );
        room
    }

    /// A checkbox or a radio button is painted by the engine's own theme,
    /// which a snapshot does not carry. Its box is drawn here when the page
    /// gave it none of its own, and its mark when it is chosen, so whoever
    /// watches is shown the answer the page holds. Its value is what the page
    /// sends for it, not words it shows, and is never drawn.
    ///
    /// Where the snapshot says the engine's theme paints it, it is drawn as
    /// that theme draws it: a grey ring while it is empty, and filled with
    /// the page's accent once it is chosen.
    fn choice(&mut self, node: i64, li: usize, order: i64, rect: Rect, round: bool) {
        let side = rect.w.min(rect.h);
        if side <= 0.0 {
            return;
        }
        let node_key = self.doc.node_key(node);
        let key = |part| ItemKey {
            node: node_key,
            part,
        };
        let checked = self.doc.checked.contains(&node);
        let inset = (side / 4.0).max(1.0);
        let dot = Rect::new(
            rect.x + inset,
            rect.y + inset,
            (rect.w - inset * 2.0).max(1.0),
            (rect.h - inset * 2.0).max(1.0),
        );
        if !self.themed(li) {
            self.plain_choice(node, li, order, (rect, dot), round);
            return;
        }
        let keep = if self.disabled(node) { DISABLED } else { 1.0 };
        let accent = match self.doc.style(li, S::Accent) {
            "" | "auto" => ACCENT,
            named => css_color(named).unwrap_or(ACCENT),
        };
        let radius = if round { side / 2.0 } else { 2.0 * self.zoom };
        let ring = faded(if checked { accent } else { EDGE }, keep);
        self.push(
            node,
            order,
            0,
            Item::Fill {
                key: key(PART_CHOICE_FILL),
                rect,
                color: faded(if checked && !round { accent } else { WHITE }, keep),
                radius,
            },
        );
        self.push(
            node,
            order,
            1,
            Item::Border {
                key: key(CHOICE_BOX),
                rect,
                widths: [self.zoom; 4],
                colors: [ring; 4],
                radius,
            },
        );
        if !checked {
            return;
        }
        let mark = if round {
            Item::Fill {
                key: key(CHOICE_MARK),
                rect: dot,
                color: faded(accent, keep),
                radius: dot.w.min(dot.h) / 2.0,
            }
        } else {
            Item::Vector {
                key: key(CHOICE_MARK),
                rect,
                view_box: Rect::new(0.0, 0.0, 13.0, 13.0),
                shapes: vec![stroke("M3 6.8L5.6 9.4L10.2 4.2", WHITE, 1.8)],
            }
        };
        self.push(node, order, 3, mark);
    }

    /// A checkbox or a radio button of a page whose snapshot does not say the
    /// engine's theme paints it: a box in the page's text colour where the
    /// page gave it none, and its mark in that colour when it is chosen.
    /// `boxes` is the control's box and the box of its mark.
    fn plain_choice(&mut self, node: i64, li: usize, order: i64, boxes: (Rect, Rect), round: bool) {
        let (rect, dot) = boxes;
        let side = rect.w.min(rect.h);
        let checked = self.doc.checked.contains(&node);
        let node_key = self.doc.node_key(node);
        let key = |part| ItemKey {
            node: node_key,
            part,
        };
        let color = css_color(self.doc.style(li, S::Fg)).unwrap_or_default();
        // A computed border width is zero unless its style is a visible
        // one, so the widths alone say whether the page gave the box a
        // border.
        let bordered = [
            S::BorderTopW,
            S::BorderRightW,
            S::BorderBottomW,
            S::BorderLeftW,
        ]
        .into_iter()
        .any(|width| px(self.doc.style(li, width)) > 0.0);
        let filled = css_color(self.doc.style(li, S::Bg)).is_some_and(|bg| bg.0[3] > 0);
        if !bordered && !filled {
            self.push(
                node,
                order,
                1,
                Item::Border {
                    key: key(CHOICE_BOX),
                    rect,
                    widths: [1.0; 4],
                    colors: [color; 4],
                    radius: if round { side / 2.0 } else { 2.0 },
                },
            );
        }
        if checked {
            self.push(
                node,
                order,
                3,
                Item::Fill {
                    key: key(CHOICE_MARK),
                    rect: dot,
                    color,
                    radius: if round { dot.w.min(dot.h) / 2.0 } else { 1.0 },
                },
            );
        }
    }

    /// A slider as the engine's theme draws it: a groove, filled in the
    /// page's accent up to the thumb, and the thumb where the value puts it
    /// between the least and the most the field takes.
    fn slider(&mut self, node: i64, li: usize, order: i64, rect: Rect) {
        let number = |name: &str, or: f32| {
            self.doc
                .attr(node, name)
                .and_then(|said| said.trim().parse::<f32>().ok())
                .filter(|said| said.is_finite())
                .unwrap_or(or)
        };
        let least = number("min", 0.0);
        let most = number("max", 100.0).max(least);
        let value = self
            .doc
            .input_values
            .get(&node)
            .and_then(|held| held.trim().parse::<f32>().ok())
            .filter(|held| held.is_finite())
            .unwrap_or_else(|| least.midpoint(most));
        let along = if most > least {
            ((value - least) / (most - least)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let keep = if self.disabled(node) { DISABLED } else { 1.0 };
        let accent = match self.doc.style(li, S::Accent) {
            "" | "auto" => ACCENT,
            named => css_color(named).unwrap_or(ACCENT),
        };
        let thumb = rect.h.min(16.0 * self.zoom).min(rect.w);
        let groove = (thumb / 2.0).max(1.0);
        let top = rect.y + (rect.h - groove) / 2.0;
        let centre = (rect.w - thumb).mul_add(along, rect.x + thumb / 2.0);
        let node_key = self.doc.node_key(node);
        let key = |part| ItemKey {
            node: node_key,
            part,
        };
        self.push(
            node,
            order,
            0,
            Item::Fill {
                key: key(CHOICE_BOX),
                rect: Rect::new(rect.x, top, rect.w, groove),
                color: faded(TRACK, keep),
                radius: groove / 2.0,
            },
        );
        self.push(
            node,
            order,
            1,
            Item::Fill {
                key: key(PART_CHOICE_FILL),
                rect: Rect::new(rect.x, top, centre - rect.x, groove),
                color: faded(accent, keep),
                radius: groove / 2.0,
            },
        );
        self.push(
            node,
            order,
            3,
            Item::Fill {
                key: key(FIELD_VALUE),
                rect: Rect::new(
                    centre - thumb / 2.0,
                    rect.y + (rect.h - thumb) / 2.0,
                    thumb,
                    thumb,
                ),
                color: faded(accent, keep),
                radius: thumb / 2.0,
            },
        );
    }

    /// The colour a colour field holds, as the swatch the field shows.
    fn swatch(&mut self, node: i64, li: usize, order: i64, rect: Rect) {
        let held = self.doc.input_values.get(&node).copied().unwrap_or("");
        let Some(color) = hex_color(held) else {
            return;
        };
        let inset = self.len(li, S::BorderLeftW) + 4.0 * self.zoom;
        let within = Rect::new(
            rect.x + inset,
            rect.y + inset,
            inset.mul_add(-2.0, rect.w),
            inset.mul_add(-2.0, rect.h),
        );
        if within.w <= 0.0 || within.h <= 0.0 {
            return;
        }
        let key = ItemKey {
            node: self.doc.node_key(node),
            part: FIELD_VALUE,
        };
        self.push(
            node,
            order,
            3,
            Item::Fill {
                key,
                rect: within,
                color,
                radius: 0.0,
            },
        );
    }

    /// A textarea's text, a line box to each line. The client shapes the
    /// text itself, so lines are broken here at about the width the box
    /// gives by an average letter, and as many are drawn as the box shows:
    /// text past them ends in an ellipsis, as it is scrolled out of sight.
    fn lines(&mut self, node: i64, li: usize, order: i64, rect: Rect) {
        let value = self.doc.text_values.get(&node).copied().unwrap_or("");
        let masked;
        let value = if self.secret(node, li) {
            masked = value
                .chars()
                .map(|held| if held == '\n' { "\n" } else { DOT })
                .collect::<String>();
            masked.as_str()
        } else {
            value
        };
        let (text, color) = if value.is_empty() {
            (
                self.doc.attr(node, "placeholder").unwrap_or(""),
                PLACEHOLDER,
            )
        } else {
            (
                value,
                css_color(self.doc.style(li, S::Fg)).unwrap_or_default(),
            )
        };
        if text.is_empty() {
            return;
        }
        let font = self.font(li);
        let pad = self.len(li, S::PaddingLeft) + self.len(li, S::BorderLeftW);
        let line = font.size * 1.25;
        let width = (rect.w - pad * 2.0).max(font.size);
        let most = (width / (font.size * 0.5)).max(1.0);
        let letters_of =
            |word: &str| f32::from(u16::try_from(word.chars().count()).unwrap_or(u16::MAX));
        let mut rows: Vec<String> = Vec::new();
        for paragraph in text.split('\n') {
            let mut row = String::new();
            let mut letters = 0.0_f32;
            for word in paragraph.split(' ') {
                let length = letters_of(word);
                if letters > 0.0 && letters + 1.0 + length > most {
                    rows.push(std::mem::take(&mut row));
                    letters = 0.0;
                }
                if letters > 0.0 {
                    row.push(' ');
                    letters += 1.0;
                }
                row.push_str(word);
                letters += length;
            }
            rows.push(row);
            if rows.len() > MAX_FIELD_LINES {
                break;
            }
        }
        let bottom = rect.y + rect.h;
        let mut fits = 0_usize;
        let mut below = rect.y + pad;
        while fits < MAX_FIELD_LINES && (fits == 0 || below + line <= bottom) {
            fits = fits.saturating_add(1);
            below += line;
        }
        if rows.len() > fits {
            rows.truncate(fits);
            if let Some(last) = rows.last_mut() {
                last.push('\u{2026}');
            }
        }
        let node_key = self.doc.node_key(node);
        let mut top = rect.y + pad;
        for (index, row) in rows.into_iter().enumerate() {
            let part = PART_LINE.saturating_add(u32::try_from(index).unwrap_or(u32::MAX));
            if !row.is_empty() {
                self.push(
                    node,
                    order,
                    3,
                    Item::Text {
                        key: ItemKey {
                            node: node_key,
                            part,
                        },
                        rect: Rect::new(rect.x + pad, top, width, line),
                        text: row,
                        font: font.clone(),
                        color,
                    },
                );
            }
            top += line;
        }
    }
}
