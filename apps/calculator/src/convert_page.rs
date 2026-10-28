//! The conversion page: a quantity, two units and an amount, converted.
//!
//! It is a page of the same window the calculator and the graphs are, reached
//! from the bar's mode switcher the way they are. The amount is a field of its
//! own, so a conversion never disturbs the expression being typed on the
//! calculator page.
//!
//! Everything on it is drawn here from the theme, like the keypad, and every
//! target it publishes is also a semantic action
//! (`calculator.convert:…`), so assistive technology reaches the same
//! conversion a press does.

use ui::TextRole;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    Button, ButtonFace, ButtonRole, ControlSize, TextInput, Widget as _, text_control_height,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::convert::{self, Dimension, Unit};
use crate::draw::{self, Align};

/// What a press on the conversion page asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConvertAction {
    /// Show this quantity's units.
    Quantity(Dimension),
    /// Ask for money: the one quantity Tilo has no rate source for.
    Currency,
    /// Step the source unit by `-1` or `1` within the quantity.
    From(i32),
    /// Step the target unit by `-1` or `1`.
    To(i32),
    /// Exchange the two units.
    Swap,
    /// Copy the converted result.
    Copy,
    /// Focus the amount field.
    Focus,
}

/// The measurements the page lays itself out with, cached from the last draw
/// so the event path places everything the same way.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    pad: f64,
    gap: f64,
    chip_h: f64,
    field_h: f64,
    result_h: f64,
}

impl Metrics {
    fn of(theme: &Theme) -> Self {
        let s = &theme.tokens.spacing;
        Self {
            pad: f64::from(s.s4),
            gap: f64::from(s.s3),
            chip_h: text_control_height(f64::from(s.s8 + s.s2), theme).max(28.0),
            field_h: text_control_height(f64::from(s.s8 + s.s4), theme).max(34.0),
            result_h: 2.0 * f64::from(theme.text(TextRole::Title).line_height_px()),
        }
    }
}

/// The conversion page: its state is the quantity, the two units and the
/// amount being typed. None of it is the shared document.
pub struct ConvertPage {
    dimension: Dimension,
    from: usize,
    to: usize,
    field: TextInput,
    /// What the last copy did, for the mark beside the result.
    copied: Option<(bool, ui::time::Instant)>,
    /// Geometry, laid out by [`Self::layout`].
    area: Rect,
    chips: Vec<(Dimension, Rect)>,
    from_rect: Rect,
    from_next: Rect,
    to_rect: Rect,
    to_next: Rect,
    swap: Rect,
    field_rect: Rect,
    result: Rect,
    copy: Rect,
    note: Rect,
    currency_chip: Rect,
    /// Money is asked for: the page says why it cannot answer.
    currency: bool,
    m: Metrics,
}

impl Default for ConvertPage {
    fn default() -> Self {
        Self::new()
    }
}

impl ConvertPage {
    pub fn new() -> Self {
        let dimension = Dimension::Length;
        let (from, to) = dimension.default_pair();
        Self {
            dimension,
            from,
            to,
            field: TextInput::local()
                .with_placeholder("Amount")
                .with_accessible_name("Amount to convert")
                .with_id("calculator.amount"),
            copied: None,
            area: Rect::ZERO,
            chips: Vec::new(),
            from_rect: Rect::ZERO,
            from_next: Rect::ZERO,
            to_rect: Rect::ZERO,
            to_next: Rect::ZERO,
            swap: Rect::ZERO,
            field_rect: Rect::ZERO,
            result: Rect::ZERO,
            copy: Rect::ZERO,
            note: Rect::ZERO,
            currency_chip: Rect::ZERO,
            currency: false,
            m: Metrics::of(&Theme::dark()),
        }
    }

    /// The quantity shown.
    pub fn dimension(&self) -> Dimension {
        self.dimension
    }

    /// The source unit: the quantity's first when an index somehow outlives
    /// the quantity it was chosen in.
    pub fn from(&self) -> &'static Unit {
        let units = self.dimension.units();
        units
            .get(self.from)
            .or_else(|| units.first())
            .unwrap_or(&crate::convert::FALLBACK_UNIT)
    }

    pub fn to(&self) -> &'static Unit {
        let units = self.dimension.units();
        units
            .get(self.to)
            .or_else(|| units.first())
            .unwrap_or(&crate::convert::FALLBACK_UNIT)
    }

    /// The amount as typed.
    pub fn amount(&self) -> &str {
        self.field.value()
    }

    /// The amount as a number, `None` while it is not one.
    pub fn value(&self) -> Option<f64> {
        let text = self.amount().trim();
        if text.is_empty() {
            return None;
        }
        text.parse::<f64>().ok().filter(|value| value.is_finite())
    }

    /// The converted amount as the display would write it, or what is
    /// missing.
    pub fn result_text(&self) -> Option<String> {
        let value = self.value()?;
        let converted = convert::convert(value, self.from(), self.to());
        converted.is_finite().then(|| {
            format!(
                "{} {}",
                convert::format_quantity(converted),
                self.to().symbol
            )
        })
    }

    /// The field's widget, for typing.
    /// Put `text` in the amount, as a capture's scenario does; a live window
    /// only ever types into it.
    pub fn set_field_text(&mut self, text: &str) {
        self.field.set_text(text);
    }

    pub fn field_mut(&mut self) -> &mut TextInput {
        &mut self.field
    }

    pub fn field_focused(&self) -> bool {
        self.field.is_focused()
    }

    pub fn set_field_focus(&mut self, focused: bool) {
        self.field.set_focus(focused);
    }

    /// The field's rect: a press inside it lands the caret.
    pub fn field_rect(&self) -> Rect {
        self.field_rect
    }

    /// Say what a copy of the result did.
    pub fn say_copied(&mut self, copied: bool) {
        self.copied = Some((copied, ui::time::now()));
    }

    /// Whether the page is showing the currency note instead of units.
    pub fn currency_note_shown(&self) -> bool {
        self.currency
    }

    /// The copied mark, for the page's tests and its paint.
    pub fn copied(&self) -> Option<bool> {
        self.copied
            .filter(|(_, at)| ui::time::now().saturating_duration_since(*at).as_secs_f32() < 1.4)
            .map(|(copied, _)| copied)
    }

    /// Where the control `action` stands, once the page has laid out.
    pub fn control_rect(&self, action: &ConvertAction) -> Option<Rect> {
        Some(match action {
            ConvertAction::Quantity(dimension) => self
                .chips
                .iter()
                .find(|(shown, _)| shown == dimension)
                .map(|(_, rect)| *rect)?,
            ConvertAction::Currency => self.currency_chip,
            ConvertAction::From(_) => self.from_rect,
            ConvertAction::To(_) => self.to_rect,
            ConvertAction::Swap => self.swap,
            ConvertAction::Copy => self.copy,
            ConvertAction::Focus => self.field_rect,
        })
    }

    /// What a press at `p` asks for.
    pub fn action_at(&self, p: Point) -> Option<ConvertAction> {
        if self.field_rect.contains(p) {
            return Some(ConvertAction::Focus);
        }
        if self.currency_chip.contains(p) {
            return Some(ConvertAction::Currency);
        }
        for (dimension, rect) in &self.chips {
            if rect.contains(p) {
                return Some(ConvertAction::Quantity(*dimension));
            }
        }
        if self.swap.contains(p) {
            return Some(ConvertAction::Swap);
        }
        if self.copy.contains(p) {
            return Some(ConvertAction::Copy);
        }
        // A unit is one target: pressing it steps to the next unit of its
        // quantity. Assistive technology reaches the step back as well.
        if self.from_rect.contains(p) {
            return Some(ConvertAction::From(1));
        }
        if self.to_rect.contains(p) {
            return Some(ConvertAction::To(1));
        }
        None
    }

    /// Do `action`: the page's whole vocabulary, shared by a press, a key and
    /// assistive technology.
    pub fn apply(&mut self, action: &ConvertAction) {
        let units = self.dimension.units().len();
        match action {
            ConvertAction::Currency => self.currency = true,
            ConvertAction::Quantity(dimension) => {
                self.currency = false;
                if self.dimension != *dimension {
                    self.dimension = *dimension;
                    let (from, to) = dimension.default_pair();
                    self.from = from;
                    self.to = to;
                }
            }
            ConvertAction::From(step) => {
                self.from = step_index(self.from, *step, units);
                if self.from == self.to {
                    self.to = (self.to + 1) % units;
                }
            }
            ConvertAction::To(step) => {
                self.to = step_index(self.to, *step, units);
                if self.to == self.from {
                    self.from = (self.from + 1) % units;
                }
            }
            ConvertAction::Swap => std::mem::swap(&mut self.from, &mut self.to),
            ConvertAction::Copy | ConvertAction::Focus => {}
        }
    }

    /// The semantic target the page publishes for `action`, the same address
    /// a press runs.
    pub fn semantic_target(action: &ConvertAction) -> String {
        match action {
            ConvertAction::Quantity(dimension) => {
                format!("calculator.convert:quantity:{}", dimension.slug())
            }
            ConvertAction::Currency => "calculator.convert:currency".into(),
            ConvertAction::From(step) => format!("calculator.convert:from:{step}"),
            ConvertAction::To(step) => format!("calculator.convert:to:{step}"),
            ConvertAction::Swap => "calculator.convert:swap".into(),
            ConvertAction::Copy => "calculator.convert:copy".into(),
            ConvertAction::Focus => "calculator.convert:amount".into(),
        }
    }

    /// The action a published target stands for.
    pub fn action_for_target(target: &str) -> Option<ConvertAction> {
        let rest = target.strip_prefix("calculator.convert:")?;
        let (kind, value) = rest.split_once(':').unwrap_or((rest, ""));
        Some(match kind {
            "quantity" => ConvertAction::Quantity(
                convert::DIMENSIONS
                    .into_iter()
                    .find(|dimension| dimension.slug() == value)?,
            ),
            "from" => ConvertAction::From(value.parse().ok()?),
            "to" => ConvertAction::To(value.parse().ok()?),
            "currency" => ConvertAction::Currency,
            "swap" => ConvertAction::Swap,
            "copy" => ConvertAction::Copy,
            "amount" => ConvertAction::Focus,
            _ => return None,
        })
    }

    /// Lay the page out in `area`, in the theme's own measurements.
    pub fn layout(&mut self, area: Rect, theme: &Theme) {
        self.m = Metrics::of(theme);
        self.area = area;
        let m = self.m;
        let inner = Rect::new(
            area.x0 + m.pad,
            area.y0 + m.pad,
            area.x1 - m.pad,
            area.y1 - m.pad,
        );
        let width = inner.width();
        // The quantity chips wrap: eight of them do not fit a phone at a
        // readable size, and a chip nobody can read is not a choice.
        let chip_gap = m.gap * 0.5;
        // Each chip is as wide as its own word, measured in the face it is
        // drawn in, so a row of them reads as one row of choices.
        let chip_widths = ui::text_render::with_layout_renderer(theme.text_scale, |text| {
            let mut widths = Vec::with_capacity(convert::DIMENSIONS.len() + 1);
            for label in convert::DIMENSIONS
                .iter()
                .map(|dimension| dimension.label())
                .chain(std::iter::once("Currency"))
            {
                let face = ButtonFace::new(Rect::ZERO, label)
                    .control_size(ControlSize::Small)
                    .role(ButtonRole::Secondary);
                widths.push(face.intrinsic_width(text, theme));
            }
            widths
        });
        let mut chips = Vec::with_capacity(convert::DIMENSIONS.len());
        let mut x = inner.x0;
        let mut y = inner.y0;
        for (dimension, w) in convert::DIMENSIONS.into_iter().zip(&chip_widths) {
            let w = w.min(width);
            if x > inner.x0 && x + w > inner.x1 {
                x = inner.x0;
                y += m.chip_h + chip_gap;
            }
            chips.push((dimension, Rect::new(x, y, x + w, y + m.chip_h)));
            x += w + chip_gap;
        }
        // Money last, on the same flow, because it is the quantity people
        // look for and the one this page cannot convert.
        let w = chip_widths
            .last()
            .copied()
            .unwrap_or(m.chip_h * 3.0)
            .min(width);
        if x > inner.x0 && x + w > inner.x1 {
            x = inner.x0;
            y += m.chip_h + chip_gap;
        }
        self.currency_chip = Rect::new(x, y, x + w, y + m.chip_h);
        self.chips = chips;
        let chips_bottom = self
            .chips
            .iter()
            .map(|(_, rect)| rect.y1)
            .fold(self.currency_chip.y1, f64::max);

        // The two units side by side with a swap between them, then the
        // amount and the result.
        let top = chips_bottom + m.gap * 2.0;
        let swap_w = m.field_h;
        let unit_w = ((width - swap_w - 2.0 * m.gap) / 2.0).max(0.0);
        self.from_rect = Rect::new(inner.x0, top, inner.x0 + unit_w, top + m.field_h);
        self.swap = Rect::new(
            self.from_rect.x1 + m.gap,
            top,
            self.from_rect.x1 + m.gap + swap_w,
            top + m.field_h,
        );
        self.to_rect = Rect::new(
            self.swap.x1 + m.gap,
            top,
            self.swap.x1 + m.gap + unit_w,
            top + m.field_h,
        );
        self.from_next = Rect::ZERO;
        self.to_next = Rect::ZERO;

        let field_top = top + m.field_h + m.gap;
        self.field_rect = Rect::new(inner.x0, field_top, inner.x1, field_top + m.field_h);
        self.field.apply_layout_external(self.field_rect);
        let result_top = self.field_rect.y1 + m.gap;
        self.result = Rect::new(
            inner.x0,
            result_top,
            inner.x1 - m.field_h - m.gap,
            (result_top + m.result_h).min(inner.y1),
        );
        self.copy = Rect::new(
            self.result.x1 + m.gap,
            result_top,
            inner.x1,
            (result_top + m.result_h).min(inner.y1),
        );
        // The note sits under the result, where the currency sentence goes.
        self.note = Rect::new(
            inner.x0,
            self.result.y1 + m.gap,
            inner.x1,
            (self.result.y1 + m.gap + m.result_h).min(inner.y1),
        );
    }

    /// Paint the page.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let m = self.m;
        // The quantity chips.
        for (dimension, rect) in &self.chips {
            let face = ButtonFace::new(*rect, dimension.label())
                .control_size(ControlSize::Small)
                .role(ButtonRole::Secondary)
                .toggled(*dimension == self.dimension);
            Button::paint(scene, transform, text, theme, &face);
        }
        // Money: the one quantity that needs a rate, and the page says so
        // where the units would be rather than converting with numbers it
        // would have had to invent.
        let currency = ButtonFace::new(self.currency_chip, "Currency")
            .control_size(ControlSize::Small)
            .role(ButtonRole::Secondary)
            .toggled(self.currency);
        Button::paint(scene, transform, text, theme, &currency);
        if self.currency {
            let mut ctx = PaintCtx::new(scene, transform, text, theme);
            draw::text_block(
                &mut ctx,
                Rect::new(
                    self.from_rect.x0,
                    self.from_rect.y0,
                    self.to_rect.x1,
                    self.note.y1,
                ),
                convert::CURRENCY_NOTE,
                &theme.text(TextRole::Body),
                theme.tokens.colors.text_dim,
                Align::Left,
                6,
            );
            return;
        }

        // The two units, each a button that steps to the next unit of its
        // quantity, so the pair is chosen without a list to open.
        for (unit, rect) in [(self.from(), self.from_rect), (self.to(), self.to_rect)] {
            let face = ButtonFace::new(rect, unit.symbol)
                .control_size(ControlSize::Regular)
                .role(ButtonRole::Secondary)
                .accessible_name(unit.name);
            Button::paint(scene, transform, text, theme, &face);
        }
        // Two arrows, one each way: the units exchange places.
        let swap = ButtonFace::new(self.swap, "")
            .control_size(ControlSize::Small)
            .role(ButtonRole::Secondary)
            .icon(ui::icons::Icon::ArrowUpDown);
        Button::paint(scene, transform, text, theme, &swap);

        self.field.draw(scene, transform, text, theme, presence);

        // The answer, in the display's own numerals, with the copy target on
        // its end.
        let mut ctx = PaintCtx::new(scene, transform, text, theme);
        let style = theme.text(TextRole::Title).numeric();
        let (text_value, colour) = match self.result_text() {
            Some(result) => (result, theme.text_primary),
            None => (
                // Nothing convert yet: the page still says what it is set to.
                format!("{} to {}", self.from().symbol, self.to().symbol),
                theme.tokens.colors.text_dim,
            ),
        };
        draw::text_in(
            &mut ctx,
            self.result,
            &text_value,
            &style,
            colour,
            Align::Right,
        );
        let copy = ButtonFace::new(self.copy, "")
            .control_size(ControlSize::Small)
            .role(ButtonRole::Secondary)
            .icon(ui::icons::Icon::Copy);
        Button::paint(scene, transform, text, theme, &copy);
        let note = match self.copied() {
            Some(true) => "Copied",
            Some(false) => "Couldn't copy the result",
            None => "",
        };
        if !note.is_empty() {
            let mut ctx = PaintCtx::new(scene, transform, text, theme);
            draw::text_in(
                &mut ctx,
                self.note,
                note,
                &theme.text(TextRole::Label),
                theme.tokens.colors.text_dim,
                Align::Left,
            );
        }
        let _ = m;
    }
}

/// `index` stepped by `step` and wrapped into `0..len`.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a list of a handful of units, stepped by one"
)]
fn step_index(index: usize, step: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as i64;
    (index as i64 + i64::from(step)).rem_euclid(len) as usize
}

impl crate::app::CalculatorApp {
    /// Take `event` for the amount field: typing lands in it, and Escape
    /// leaves it.
    pub(crate) fn convert_key(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && matches!(
                key.logical_key,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            )
        {
            self.convert_page_mut().set_field_focus(false);
            return;
        }
        if !self.convert_page().field_focused() {
            self.convert_page_mut().set_field_focus(true);
        }
        self.convert_page_mut().field_mut().handle_event(event, ctx);
    }

    /// Do what a press on the conversion page asked for.
    pub(crate) fn on_press_convert(&mut self, p: Point) {
        let Some(action) = self.convert_page().action_at(p) else {
            return;
        };
        self.apply_convert_action(&action);
    }

    /// Do `action` on the conversion page: the one path a press, a key and an
    /// assistive action all take.
    pub(crate) fn apply_convert_action(&mut self, action: &ConvertAction) {
        match action {
            ConvertAction::Focus => self.convert_page_mut().set_field_focus(true),
            ConvertAction::Copy => self.copy_convert_result(),
            other => {
                self.convert_page_mut().apply(other);
                self.convert_page_mut().set_field_focus(false);
            }
        }
    }

    /// Copy the converted amount, and say whether it reached the clipboard.
    fn copy_convert_result(&mut self) {
        let Some(text) = self.convert_page().result_text() else {
            return;
        };
        let copied = self.copy_watch_mut().watch(ui::try_clipboard_copy(&text));
        self.convert_page_mut().say_copied(copied);
    }

    /// The conversion page as assistive technology reads it: the amount, the
    /// quantity, each unit, and what the conversion came to.
    pub(crate) fn convert_accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        let page = self.convert_page();
        let mut children = Vec::new();
        let mut amount = AccessNode::new(Role::TextInput)
            .with_name("Amount to convert")
            .with_value(page.amount().to_owned())
            .with_action_target(ConvertPage::semantic_target(&ConvertAction::Focus))
            .with_state(AccessState::new().focusable(true));
        if page.field_rect() != Rect::ZERO {
            amount = amount.with_bounds(page.field_rect());
        }
        children.push(amount);

        let quantities = convert::DIMENSIONS
            .into_iter()
            .map(|dimension| {
                let target = ConvertAction::Quantity(dimension);
                let mut node = AccessNode::new(Role::RadioButton)
                    .with_name(dimension.label())
                    .with_action_target(ConvertPage::semantic_target(&target))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(page.dimension() == dimension && !page.currency),
                    );
                if let Some(bounds) = page.control_rect(&target) {
                    node = node.with_bounds(bounds);
                }
                node
            })
            .chain(std::iter::once({
                let target = ConvertAction::Currency;
                let mut node = AccessNode::new(Role::RadioButton)
                    .with_name("Currency")
                    .with_action_target(ConvertPage::semantic_target(&target))
                    .with_state(AccessState::new().focusable(true).checked(page.currency));
                if let Some(bounds) = page.control_rect(&target) {
                    node = node.with_bounds(bounds);
                }
                node
            }))
            .collect();
        children.push(
            AccessNode::new(Role::Group)
                .with_name("Quantity")
                .with_hashed_key("calculator.convert.quantities")
                .with_children(quantities),
        );

        for (name, unit, step) in [
            ("From", page.from(), ConvertAction::From(1)),
            ("To", page.to(), ConvertAction::To(1)),
        ] {
            let mut node = AccessNode::new(Role::Button)
                .with_name(format!("{name} unit: {}", unit.name))
                .with_description(format!("Change {} to the next unit", unit.symbol))
                .with_action_target(ConvertPage::semantic_target(&step))
                .with_state(AccessState::new().focusable(true));
            if let Some(bounds) = page.control_rect(&step) {
                node = node.with_bounds(bounds);
            }
            children.push(node);
        }
        for (name, target) in [
            ("Swap units", ConvertAction::Swap),
            ("Copy result", ConvertAction::Copy),
        ] {
            let mut node = AccessNode::new(Role::Button)
                .with_name(name)
                .with_action_target(ConvertPage::semantic_target(&target))
                .with_state(AccessState::new().focusable(true));
            if let Some(bounds) = page.control_rect(&target) {
                node = node.with_bounds(bounds);
            }
            children.push(node);
        }
        let outcome = match page.result_text() {
            Some(result) => result,
            None if page.currency => convert::CURRENCY_NOTE.to_owned(),
            None => format!("{} to {}", page.from().symbol, page.to().symbol),
        };
        children.push(
            AccessNode::new(Role::StaticText)
                .with_name(outcome)
                .with_live(Politeness::Polite)
                .with_hashed_key("calculator.convert.result"),
        );
        AccessNode::new(Role::Group)
            .with_name("Unit conversion")
            .with_description("Convert an amount from one unit to another, offline.")
            .with_hashed_key("calculator.convert")
            .with_children(children)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page laid out in a phone-width window, as a live one is.
    fn laid_out() -> ConvertPage {
        let mut page = ConvertPage::new();
        page.layout(Rect::new(0.0, 0.0, 390.0, 700.0), &Theme::dark());
        page
    }

    #[test]
    fn a_quantity_chip_chooses_its_own_units() {
        let mut page = laid_out();
        let temperature = page
            .control_rect(&ConvertAction::Quantity(Dimension::Temperature))
            .expect("the temperature chip is laid out");
        let press = page.action_at(temperature.center());
        assert_eq!(press, Some(ConvertAction::Quantity(Dimension::Temperature)));
        page.apply(&press.unwrap());
        assert_eq!(page.dimension(), Dimension::Temperature);
        assert_eq!(page.from().symbol, "°C");
        assert_eq!(page.to().symbol, "°F");
    }

    #[test]
    fn pressing_a_unit_steps_it_and_swapping_exchanges_the_pair() {
        let mut page = laid_out();
        let from = page.from().symbol;
        let to = page.to().symbol;
        let press = page.action_at(page.control_rect(&ConvertAction::From(1)).unwrap().center());
        assert_eq!(press, Some(ConvertAction::From(1)));
        page.apply(&press.unwrap());
        assert_ne!(page.from().symbol, from, "the unit stepped on");
        assert_ne!(page.from().symbol, to, "and never onto the other one");

        let (before_from, before_to) = (page.from().symbol, page.to().symbol);
        page.apply(&ConvertAction::Swap);
        assert_eq!(
            (page.to().symbol, page.from().symbol),
            (before_from, before_to)
        );
    }

    #[test]
    fn the_amount_reads_as_a_number_and_answers_in_the_target_unit() {
        let mut page = laid_out();
        page.apply(&ConvertAction::Quantity(Dimension::Temperature));
        assert_eq!(page.result_text(), None, "no amount yet");
        page.set_field_text("40");
        assert_eq!(page.result_text(), Some("104 °F".to_owned()));
        page.apply(&ConvertAction::Swap);
        assert_eq!(page.result_text(), Some("4.44444 °C".to_owned()));
        page.set_field_text("not a number");
        assert_eq!(
            page.result_text(),
            None,
            "what is not a number has no answer"
        );
    }

    #[test]
    fn every_published_target_names_its_action_and_back() {
        let actions = [
            ConvertAction::Quantity(Dimension::Data),
            ConvertAction::Currency,
            ConvertAction::From(1),
            ConvertAction::From(-1),
            ConvertAction::To(1),
            ConvertAction::Swap,
            ConvertAction::Copy,
            ConvertAction::Focus,
        ];
        for action in actions {
            let target = ConvertPage::semantic_target(&action);
            assert_eq!(
                ConvertPage::action_for_target(&target),
                Some(action.clone()),
                "{target}"
            );
        }
        assert_eq!(
            ConvertPage::action_for_target("calculator.convert:nope"),
            None
        );
    }

    #[test]
    fn money_says_what_it_needs_instead_of_converting() {
        let mut page = laid_out();
        page.apply(&ConvertAction::Currency);
        assert!(page.currency_note_shown());
        // The units stay where they were for when the person comes back.
        assert_eq!(page.dimension(), Dimension::Length);
        page.apply(&ConvertAction::Quantity(Dimension::Mass));
        assert!(!page.currency_note_shown(), "a quantity leaves the note");
    }
}
