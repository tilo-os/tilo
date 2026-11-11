//! The send-to box: where a context menu's sentence goes.
//!
//! The row that sends what a context menu is about ([`ContextMenu::set_about`])
//! opens this box in the menu's place, with the sentence the person typed into
//! it, so one field takes the typing. The box says whom the message goes to
//! (the agent it makes by default, else an agent, a person or a conversation
//! the host's one search finds), what it is about (the chips, starting with
//! what the menu was opened on) and its words. Send hands one
//! [`SendToRequest`](crate::shared_selection::SendToRequest) to the app
//! runner, which gives it to the session: the session makes the agent, opens
//! or makes the one conversation and posts the message, as the person.
//!
//! It is one focus group: Tab steps from the recipient field to the message
//! and the Send button, and round again.
//!
//! [`ContextMenu::set_about`]: super::ContextMenu::set_about

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::floating_surface::{Recipient, RecipientKind};
use crate::motion::Appearing;
use crate::paint::PaintCtx;
use crate::platform::PlatformConventions;
use crate::query_text::{QueryComposition, QueryEventChange, QueryInk, QueryView};
use crate::shared_selection::{SendToRequest, SharedSelection};
use crate::text_render::TextRenderer;
use crate::text_selection::Selection;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::floating_surface as panel;

/// The rows of the search shown at once: more than a person reads while
/// typing, and few enough that the box stays where it was opened.
const MAX_ROWS: usize = 6;
/// The rows the message field is tall while the words fit on one line: air
/// enough to look like somewhere to write.
const MESSAGE_ROWS: usize = 2;
/// The rows it grows to as the words wrap: past this the box would stand
/// taller than what it is sent with, so its words scroll instead.
const MAX_MESSAGE_ROWS: usize = 4;

/// Which field the keyboard is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendToFocus {
    /// The field that searches for whom the message goes to.
    Recipients,
    /// The message's own field.
    Message,
}

/// Where each part of the box is drawn, worked out when it is placed: the
/// chips and fields are painted rows rather than child widgets, so this is
/// what hit-tests and what assistive technology reads the bounds from.
#[derive(Clone, Debug, Default)]
struct Layout {
    /// The quiet line that names the box.
    header: Rect,
    chips: Vec<Rect>,
    /// Where a chip's × stands, for the chips that carry one.
    removes: Vec<Option<Rect>>,
    search: Rect,
    rows: Vec<Rect>,
    subjects: Vec<Rect>,
    plus: Rect,
    message: Rect,
    send: Rect,
}

/// The box a context menu's send row opens.
pub struct SendToBox {
    open: bool,
    /// What was acted on: the subject the box starts with.
    about: Option<SharedSelection>,
    /// The subjects, chips in the order they show.
    subjects: Vec<SharedSelection>,
    /// Whom it goes to: the agent being made by default, or, when the session
    /// opened the box to a fixed destination, that alone.
    to: Vec<Recipient>,
    /// Whether the session named whom it goes to: the box then searches for
    /// nobody and its chips stay, so a report cannot be addressed elsewhere.
    addressed: bool,
    /// What is typed into the recipient field, and the search it asked for.
    search: String,
    search_selection: Selection,
    search_composition: QueryComposition,
    request: u64,
    rows: Vec<Recipient>,
    hovered: Option<usize>,
    /// The message's words and their editing state.
    text: String,
    text_selection: Selection,
    text_composition: QueryComposition,
    focus: SendToFocus,
    /// Whether the keyboard is in the box: it takes the typing while open.
    focused: bool,
    /// The search answered nothing yet for what is typed.
    answered: bool,
    rect: Rect,
    layout: Layout,
    viewport: Option<Rect>,
    theme: Theme,
    platform: PlatformConventions,
    text_scale: f64,
    /// Set when the person sent: drained by the menu, which goes with it.
    sent: bool,
    /// Whether it is drawn as it is (`motion::Appearing`): it arrives on what
    /// it is about and leaves with it.
    dirty: bool,
    /// How it comes and goes: from what it is about.
    appear: Appearing,
    /// The point it comes from and returns to, on what it is about.
    anchor: Point,
}

impl Default for SendToBox {
    fn default() -> Self {
        Self::new()
    }
}

impl SendToBox {
    #[must_use]
    pub fn new() -> Self {
        Self {
            open: false,
            about: None,
            subjects: Vec::new(),
            to: vec![Recipient::new_agent(String::new())],
            addressed: false,
            search: String::new(),
            search_selection: Selection::default(),
            search_composition: QueryComposition::default(),
            request: 0,
            rows: Vec::new(),
            hovered: None,
            text: String::new(),
            text_selection: Selection::default(),
            text_composition: QueryComposition::default(),
            focus: SendToFocus::Recipients,
            focused: false,
            answered: false,
            rect: Rect::ZERO,
            layout: Layout::default(),
            viewport: None,
            theme: Theme::dark(),
            platform: PlatformConventions::default(),
            text_scale: 1.0,
            sent: false,
            dirty: true,
            appear: Appearing::closed(),
            anchor: Point::ZERO,
        }
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether any of it is on screen: open, arriving, or leaving.
    #[must_use]
    pub fn is_drawn(&self) -> bool {
        self.appear.is_drawn()
    }

    /// Whether it moves now, so the frame pump keeps drawing.
    #[must_use]
    pub fn is_moving(&self) -> bool {
        self.appear.is_moving()
    }

    /// Where the box stands.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// The keyboard is in the box (Tab brought it here, or it opened).
    #[must_use]
    pub fn is_focused(&self) -> bool {
        self.open && self.focused
    }

    /// Whether the person sent what the box held, once.
    pub fn take_sent(&mut self) -> bool {
        std::mem::take(&mut self.sent)
    }

    /// The recipients on their chips, the new agent among them.
    #[must_use]
    pub fn recipients(&self) -> &[Recipient] {
        &self.to
    }

    /// Whether the session named whom it goes to, so nobody is searched for
    /// and the chips stay.
    #[must_use]
    pub fn addressed(&self) -> bool {
        self.addressed
    }

    /// What the message says.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// What the message is about, on its chips.
    #[must_use]
    pub fn subjects(&self) -> &[SharedSelection] {
        &self.subjects
    }

    /// The recipients the search found, in the order shown.
    #[must_use]
    pub fn rows(&self) -> &[Recipient] {
        &self.rows
    }

    /// The row the keyboard is on among the recipients found.
    #[must_use]
    pub fn hovered_row(&self) -> Option<usize> {
        self.hovered
    }

    /// Which field the keyboard is in.
    #[must_use]
    pub fn focus(&self) -> SendToFocus {
        self.focus
    }

    /// Say what the box is about: the chip it starts with.
    pub fn set_about(&mut self, about: Option<SharedSelection>) {
        self.about = about.filter(|about| !about.is_empty());
    }

    /// Show the box about what the menu acts on, with `draft` in its message
    /// field (a sentence typed into the menu that no row answered), and to
    /// `to` when the session names whom it goes to — the one place a report
    /// goes — else to the agent a message makes, which the person may change.
    pub fn open(&mut self, draft: Option<&str>, to: &[Recipient]) {
        self.open = true;
        // It arrives on its own: a box whose host stops drawing it once open
        // (a menu that goes as it arrives) still shows from this moment.
        self.appear.show(true);
        crate::shared_selection::set_box_open(true);
        self.focused = true;
        self.sent = false;
        self.dirty = true;
        self.subjects.clear();
        if let Some(about) = self.about.clone() {
            self.subjects.push(about);
        }
        self.to.clear();
        self.addressed = !to.is_empty();
        if self.addressed {
            self.to.extend_from_slice(to);
        } else {
            self.to.push(Recipient::new_agent(String::new()));
        }
        self.search.clear();
        self.search_selection = Selection::default();
        self.rows.clear();
        self.hovered = None;
        self.answered = false;
        self.ask_search();
        self.focus = if draft.is_some() || self.addressed {
            SendToFocus::Message
        } else {
            SendToFocus::Recipients
        };
        if let Some(text) = draft {
            text.clone_into(&mut self.text);
            self.text_selection = Selection::collapsed(self.text.len());
        } else {
            self.text.clear();
        }
    }

    /// Put it away: it leaves as it stood, so what it showed is kept until
    /// the next [`Self::open`] clears it.
    pub fn close(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        self.focused = false;
        self.hovered = None;
        self.appear.show(false);
        crate::shared_selection::set_box_open(false);
        self.dirty = true;
    }

    pub fn set_viewport(&mut self, viewport: Option<Rect>) {
        if self.viewport != viewport {
            self.viewport = viewport;
            self.dirty = true;
        }
    }

    /// Stand the box beside `beside` — what it is about — on its trailing
    /// side with the same top, or its leading side where the window leaves no
    /// room, or under it where neither side holds it, inside the window.
    pub fn place(&mut self, beside: Rect, viewport: Option<Rect>) {
        self.anchor = beside.center();
        if !self.open && !self.appear.is_drawn() {
            self.rect = Rect::ZERO;
            self.layout = Layout::default();
            return;
        }
        if viewport.is_some() {
            self.viewport = viewport;
        }
        let width = self.width();
        let height = self.height();
        let gap = self.gap();
        // What it is about stands in when nothing knows the window: the box
        // still lands beside it rather than anywhere.
        let viewport = self.viewport.unwrap_or(Rect::new(
            beside.x0,
            beside.y0,
            beside.x1 + gap + width,
            beside.y1.max(beside.y0 + height),
        ));
        let trailing = beside.x1 + gap;
        let leading = beside.x0 - gap - width;
        // Kept inside the window, whatever the window leaves.
        let inside = |x: f64| x.clamp(viewport.x0, (viewport.x1 - width).max(viewport.x0));
        let top = beside
            .y0
            .min((viewport.y1 - height).max(viewport.y0))
            .max(viewport.y0);
        let (x, y) = if trailing + width <= viewport.x1 {
            (trailing, top)
        } else if leading >= viewport.x0 {
            (leading, top)
        } else if beside.y1 + gap + height <= viewport.y1 {
            // Neither side holds it: it stands under what it is about, clear
            // of it, rather than over it.
            (inside(beside.x0), beside.y1 + gap)
        } else {
            // The window is too small even for that: it takes the roomier side
            // and stays inside, over what it is about where it must.
            let x = if viewport.x1 - trailing >= leading - viewport.x0 {
                trailing
            } else {
                leading
            };
            (inside(x), top)
        };
        let rect = Rect::new(x, y, x + width, y + height);
        if self.rect != rect {
            self.dirty = true;
        }
        self.rect = rect;
        self.layout = self.build_layout();
    }

    /// The box's height, from what it holds.
    fn height(&self) -> f64 {
        let row = self.row_height();
        let pad = self.padding();
        let gap = self.gap() / 2.0;
        let results = (self.rows.len().min(MAX_ROWS) as f64) * row;
        pad * 2.0
            + self.header_height() // the quiet line naming the box
            + row // the recipients and the search, on one line
            + if self.recipients_wrap() { row } else { 0.0 } // or the search under them
            + if results > 0.0 { gap + results } else { 0.0 }
            + gap
            + if self.says_what() { row + gap } else { 0.0 } // the subjects
            + self.message_height() // the message, as tall as it wraps
            + gap
            + row // Send
    }

    /// Whether the box has a line saying what the message is about: its
    /// subjects, or the button that adds one. Nothing to say leaves no line.
    fn says_what(&self) -> bool {
        !self.subjects.is_empty() || self.addable_subject().is_some()
    }

    /// Whether the field that finds a recipient takes the line under the chips:
    /// one line does not hold both at the widths they ask for, and a chip with
    /// its name cut reads worse than a field of its own.
    fn recipients_wrap(&self) -> bool {
        if self.addressed {
            return false;
        }
        let room = self.width() - self.padding() * 2.0;
        let gap = self.gap() / 2.0;
        let (_, asks) = self.recipient_asks();
        let chips: f64 = asks.iter().sum::<f64>() + gap * asks.len() as f64;
        chips + gap + self.search_ask() > room
    }

    /// How tall the message stands: two rows while its words fit on one line,
    /// and as much of them as wrap, up to four, so the box makes room for what
    /// is written in it rather than scrolling sideways.
    fn message_height(&self) -> f64 {
        let theme = &self.theme;
        let style = theme.text(TextRole::Body);
        let least = self.row_height() * MESSAGE_ROWS as f64;
        let most = self.row_height() * MAX_MESSAGE_ROWS as f64;
        if self.text.trim().is_empty() {
            return least;
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a finite field width in CSS pixels fits the f32 shaping API"
        )]
        let width =
            (self.width() - self.padding() * 2.0 - f64::from(theme.tokens.spacing.s2) * 2.0) as f32;
        if !(width.is_finite() && width >= 1.0) {
            return least;
        }
        let (_, height) = crate::text_render::with_layout_renderer(theme.text_scale, |renderer| {
            renderer.measure_style(&self.text, Some(width), &style)
        });
        (f64::from(height) + self.message_air() * 2.0).clamp(least, most)
    }

    /// How wide the box stands.
    fn width(&self) -> f64 {
        f64::from(self.theme.tokens.metrics.panel_width_utility) * self.text_scale
    }

    /// The air between the menu and the box.
    fn gap(&self) -> f64 {
        f64::from(self.theme.tokens.spacing.s2) * self.text_scale
    }

    /// The air above and below the message's lines, so its words never touch
    /// the field's edge.
    fn message_air(&self) -> f64 {
        f64::from(self.theme.tokens.spacing.s1) * self.text_scale
    }

    fn row_height(&self) -> f64 {
        f64::from(self.theme.tokens.metrics.menu_row_height) * self.text_scale
    }

    fn padding(&self) -> f64 {
        f64::from(self.theme.tokens.spacing.s2) * self.text_scale
    }

    /// The height of the line that names the box: quieter than a row, so the
    /// recipient chip under it is what the eye lands on.
    fn header_height(&self) -> f64 {
        self.row_height() * 0.8
    }

    /// How far a chip's mark stands from its leading edge, how big it is, and
    /// how far its text starts: past the mark, not on it.
    fn mark_lead(&self) -> f64 {
        self.row_height() * 0.85
    }

    fn mark_size(&self) -> f64 {
        self.row_height() * 0.62
    }

    fn text_lead(&self) -> f64 {
        self.mark_lead() + self.mark_size() / 2.0 + self.row_height() * 0.15
    }

    /// How far a pill's content stands from its leading edge: where a chip's
    /// mark begins, and where a field's own words begin, so the chips and the
    /// field beside them line up.
    fn mark_inset(&self) -> f64 {
        self.mark_lead() - self.mark_size() / 2.0
    }

    /// How wide `label` sets in the box's own text: a chip is as wide as what
    /// it says, so a name neither runs past its pill nor leaves it half empty.
    fn text_width(&self, label: &str) -> f64 {
        let style = self.theme.text(TextRole::Body);
        let theme = &self.theme;
        f64::from(crate::text_render::with_layout_renderer(
            theme.text_scale,
            |renderer| renderer.measure_style(label, None, &style).0,
        ))
    }

    /// The recipients' chips: whether each carries the × that takes it off, and
    /// how wide each asks to be.
    fn recipient_asks(&self) -> (Vec<bool>, Vec<f64>) {
        let removable: Vec<bool> = self
            .to
            .iter()
            .map(|recipient| {
                !(self.addressed
                    || recipient.kind == RecipientKind::NewAgent && recipient.name.is_empty())
            })
            .collect();
        let asks = self
            .to
            .iter()
            .zip(&removable)
            .map(|(recipient, remove)| self.chip_ask(&chip_name(recipient), *remove))
            .collect();
        (removable, asks)
    }

    /// What a chip asks for: the mark it leads with, its label, and the × that
    /// takes it off.
    fn chip_ask(&self, label: &str, remove: bool) -> f64 {
        let row = self.row_height();
        self.text_lead()
            + self.text_width(label)
            + row * 0.45
            + if remove { row * 0.55 } else { 0.0 }
    }

    /// What the field that searches for a recipient asks for: its hint whole,
    /// set in from its edge as a chip's mark is, and never less than the five
    /// rows it held. A hint longer than the box is a hint the box is too narrow
    /// for, not one the person reads half of.
    fn search_ask(&self) -> f64 {
        let hint = crate::i18n::I18nManager::lookup_plain("send-to-search-placeholder");
        (self.row_height() * 5.0).max(self.text_width(&hint) + self.mark_inset())
    }

    /// The button that adds what is selected now to what the message is about,
    /// as it names itself.
    fn add_label(&self) -> String {
        crate::i18n::I18nManager::lookup_plain("send-to-add-subject")
    }

    /// What the button would add: what the person has selected now, when the
    /// message is not about it already. Nothing to add leaves no button.
    fn addable_subject(&self) -> Option<SharedSelection> {
        crate::shared_selection::focused_selection().filter(|focused| {
            !focused.is_empty() && !self.subjects.iter().any(|subject| subject == focused)
        })
    }

    /// Add what is selected now to what the message is about.
    fn add_subject(&mut self) {
        if let Some(subject) = self.addable_subject() {
            self.subjects.push(subject);
            self.changed();
            self.layout = self.build_layout();
        }
    }

    /// Place chips asking for `asks` from the box's leading edge on line `y`,
    /// sharing `room` between them when together they ask for more, and how far
    /// the line reaches. So neither a chip nor the control after it — the
    /// search, the button that adds a subject — stands past the box.
    fn lay_chips(&self, asks: &[f64], y: f64, room: f64) -> (Vec<Rect>, f64) {
        let rect = self.rect;
        let row = self.row_height();
        let gap = self.gap() / 2.0;
        let start = rect.x0 + self.padding();
        let mut chips = Vec::new();
        let narrowest = row * 0.5 + gap;
        // As many chips as the line holds at its narrowest, so a count that
        // would leave nothing to read does not crowd out the control after it.
        let count = (room / narrowest).floor().max(0.0);
        let shown = asks
            .len()
            .min(usize::try_from(count as u64).unwrap_or(usize::MAX));
        if shown == 0 {
            return (chips, start);
        }
        let share = ((room - gap * shown as f64) / shown as f64).max(0.0);
        let mut x = start;
        for ask in &asks[..shown] {
            let width = ask.min(share);
            chips.push(Rect::new(x, y, x + width, y + row));
            x += width + gap;
        }
        (chips, x)
    }

    /// Where every painted row stands, from the box's own box.
    fn build_layout(&self) -> Layout {
        let rect = self.rect;
        let row = self.row_height();
        let pad = self.padding();
        let gap = self.gap() / 2.0;
        let mut y = rect.y0 + pad;
        let header = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + self.header_height());
        y += self.header_height();
        let room = (rect.width() - pad * 2.0).max(0.0);
        // The recipients' chips, from the leading edge, each with room for the
        // mark it leads with and the × that takes it off; the search keeps the
        // rest of the line, enough for what it says when nothing is typed.
        let (removable, asks) = self.recipient_asks();
        // A box opened to whom it sends to has no field to search in: its
        // chips take the line.
        let search_room = if self.addressed {
            0.0
        } else {
            self.search_ask().min(room)
        };
        // The chips and the field that finds one share a line while it holds
        // both at the widths they ask for; when it does not, the chips keep
        // theirs and the field takes the line under them, the width of the
        // box, so neither a name nor the field's hint is cut.
        let wrapped = self.recipients_wrap();
        let (chips, x) = self.lay_chips(
            &asks,
            y,
            (room - if wrapped { 0.0 } else { search_room }).max(0.0),
        );
        let removes: Vec<Option<Rect>> = chips
            .iter()
            .zip(&removable)
            .map(|(chip, remove)| {
                remove.then(|| Rect::new(chip.x1 - row * 0.55, chip.y0, chip.x1, chip.y1))
            })
            .collect();
        let start = (x + gap).max(rect.x0 + pad);
        let search = if self.addressed {
            Rect::new(start, y, start, y + row)
        } else if wrapped {
            Rect::new(rect.x0 + pad, y + row, rect.x1 - pad, y + 2.0 * row)
        } else {
            Rect::new(
                start.min(rect.x1 - pad - search_room).max(rect.x0 + pad),
                y,
                rect.x1 - pad,
                y + row,
            )
        };
        y = search.y1 + if self.rows.is_empty() { 0.0 } else { gap };
        let mut rows = Vec::with_capacity(self.rows.len().min(MAX_ROWS));
        for _ in self.rows.iter().take(MAX_ROWS) {
            rows.push(Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + row));
            y += row;
        }
        y += gap;
        // The subjects share their line with the button that adds one, so both
        // stay inside the box whatever they hold. The button carries its own
        // words, so it asks for the width a chip does. A box that says nothing
        // about what it sends keeps no line for them: its fields stand at its
        // own spacing, with no band held open between.
        let adds = self.addable_subject().is_some();
        let (subjects, plus) = if self.says_what() {
            let asks: Vec<f64> = self
                .subjects
                .iter()
                .map(|subject| self.chip_ask(&subject_label(subject), false))
                .collect();
            let add_ask = if adds {
                self.chip_ask(&self.add_label(), false)
            } else {
                0.0
            };
            let (subjects, x) = self.lay_chips(&asks, y, (room - add_ask).max(0.0));
            let plus = if adds {
                let x0 = x.min((rect.x1 - pad - add_ask).max(rect.x0 + pad));
                Rect::new(x0, y, x0 + add_ask, y + row)
            } else {
                Rect::ZERO
            };
            y += row + gap;
            (subjects, plus)
        } else {
            (Vec::new(), Rect::ZERO)
        };
        let message_height = self.message_height();
        let message = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + message_height);
        y += message_height + gap;
        let send_width = (row * 3.0).min(rect.width() - pad * 2.0);
        let send = Rect::new(rect.x1 - pad - send_width, y, rect.x1 - pad, y + row);
        Layout {
            header,
            chips,
            removes,
            search,
            rows,
            subjects,
            plus,
            message,
            send,
        }
    }

    /// The recipients `query` finds, asked of the host's one search. An empty
    /// field asks nothing: what every app holds would answer it, and nothing
    /// anybody choosing a recipient reads starts with a list of conversations.
    fn ask_search(&mut self) {
        self.answered = false;
        self.rows.clear();
        self.hovered = None;
        self.dirty = true;
        // A box opened to the one place it sends to has nobody to look for.
        if self.addressed || self.search.trim().is_empty() {
            self.layout = self.build_layout();
            return;
        }
        self.request = crate::shared_selection::search_recipients(&self.search);
    }

    /// What the box is called: a report, when it was opened to the one place
    /// reports go, and a message it sends anywhere else.
    fn name(&self) -> String {
        crate::i18n::I18nManager::lookup_plain(if self.addressed {
            "send-to-report-name"
        } else {
            "send-to-name"
        })
    }

    /// What the box paints has changed, so the window draws another frame.
    fn changed(&mut self) {
        self.dirty = true;
        crate::widget::note_resource_presentation_changed();
    }

    /// Take a key, a press or a text event while the box is open. Whether it
    /// took it.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        if !self.open {
            return false;
        }
        if self.drain_answer() {
            return true;
        }
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if matches!(key, Key::Named(NamedKey::Tab)) {
                    let shift = ctx.modifiers.state().shift_key();
                    if self.cycle(shift) {
                        *ctx.handled = true;
                        return true;
                    }
                    // Out of the box, into the menu.
                    self.focused = false;
                    *ctx.handled = true;
                    return true;
                }
                match key {
                    Key::Named(NamedKey::Enter) => {
                        match self.focus {
                            SendToFocus::Recipients => self.pick_or_name(),
                            SendToFocus::Message => self.send(),
                        }
                        *ctx.handled = true;
                        self.changed();
                        return true;
                    }
                    Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp)
                        if self.focus == SendToFocus::Recipients && !self.rows.is_empty() =>
                    {
                        let down = matches!(key, Key::Named(NamedKey::ArrowDown));
                        self.move_row(down);
                        *ctx.handled = true;
                        return true;
                    }
                    Key::Named(NamedKey::Backspace)
                        if self.focus == SendToFocus::Recipients
                            && self.search.is_empty()
                            && !self.addressed =>
                    {
                        if self.to.len() > 1 {
                            self.to.pop();
                            self.changed();
                            self.layout = self.build_layout();
                        } else {
                            // The last chip is the agent a message makes; it
                            // stays, so a box always has somewhere to send.
                        }
                        *ctx.handled = true;
                        return true;
                    }
                    _ => {}
                }
                let edited = self.edit(event, *ctx.modifiers);
                if edited {
                    *ctx.handled = true;
                }
                edited
            }
            WindowEvent::Ime(_) => {
                let edited = self.edit(event, *ctx.modifiers);
                if edited {
                    *ctx.handled = true;
                }
                edited
            }
            _ => false,
        }
    }

    /// Take what the one search answered for this box, once. Whether it
    /// answered.
    pub fn drain_answer(&mut self) -> bool {
        let Some(rows) = crate::shared_selection::take_recipients(self.request) else {
            return false;
        };
        if self.search.trim().is_empty() {
            // Answered after the field was cleared: what it found is not what
            // the person is choosing from now.
            return false;
        }
        self.rows = rows;
        self.answered = true;
        self.hovered = (!self.rows.is_empty()).then_some(0);
        self.dirty = true;
        self.layout = self.build_layout();
        true
    }

    /// A press at `at` on one of the box's parts. Whether it was in the box.
    pub fn press(&mut self, at: Point) -> bool {
        if !self.open || !self.rect.contains(at) {
            return false;
        }
        if self.layout.send.contains(at) && self.can_send() {
            self.send();
            return true;
        }
        if self.layout.plus.width() > 0.0 && self.layout.plus.contains(at) {
            self.add_subject();
            return true;
        }
        if let Some(index) = self.layout.chips.iter().position(|chip| chip.contains(at)) {
            // The × takes a chip off; a recipient the person picked goes on a
            // press anywhere on it, and the agent a message makes stays until
            // it is taken off by its own ×. A box the session addressed keeps
            // the one place it sends to.
            if self.addressed {
                return true;
            }
            let remove = self
                .layout
                .removes
                .get(index)
                .copied()
                .flatten()
                .is_some_and(|remove| remove.contains(at));
            if remove || index > 0 {
                self.to.remove(index);
                self.changed();
                self.layout = self.build_layout();
            }
            return true;
        }
        if let Some(index) = self
            .layout
            .subjects
            .iter()
            .position(|chip| chip.contains(at))
        {
            self.subjects.remove(index);
            self.changed();
            self.layout = self.build_layout();
            return true;
        }
        if let Some(index) = self.layout.rows.iter().position(|row| row.contains(at)) {
            self.hovered = Some(index);
            self.take_row(index);
            return true;
        }
        if self.layout.search.contains(at) {
            self.focus = SendToFocus::Recipients;
            self.focused = true;
            self.changed();
            return true;
        }
        if self.layout.message.contains(at) {
            self.focus = SendToFocus::Message;
            self.focused = true;
            self.changed();
            return true;
        }
        // Inside the panel but between its parts: the box keeps the press so
        // it does not close under the person.
        true
    }

    /// Tab within the box: the recipient field, then the message. `false`
    /// when it was on the last and the keyboard leaves for the menu.
    fn cycle(&mut self, shift: bool) -> bool {
        let next = match (self.focus, shift) {
            (SendToFocus::Recipients, _) => Some(SendToFocus::Message),
            (SendToFocus::Message, false) => None,
            (SendToFocus::Message, true) => Some(SendToFocus::Recipients),
        };
        match next {
            Some(focus) => {
                self.focus = focus;
                self.focused = true;
                self.changed();
                true
            }
            None => false,
        }
    }

    /// Put the keyboard in the box's first field (the menu's Tab brought it
    /// here).
    pub fn focus_first(&mut self) {
        if self.open {
            self.focused = true;
            self.focus = if self.addressed {
                SendToFocus::Message
            } else {
                SendToFocus::Recipients
            };
            self.changed();
        }
    }

    fn move_row(&mut self, down: bool) {
        if self.rows.is_empty() {
            return;
        }
        let current = self.hovered.unwrap_or(0);
        let next = if down {
            (current + 1).min(self.rows.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.hovered = Some(next);
        self.dirty = true;
    }

    /// Enter in the recipient field: the first row found, else the name
    /// typed becomes a new agent. Either way the keyboard goes on to the
    /// message: a recipient picked is a message about to be written.
    fn pick_or_name(&mut self) {
        if let Some(index) = self
            .hovered
            .filter(|&index| index < self.rows.len())
            .or((!self.rows.is_empty()).then_some(0))
        {
            self.take_row(index);
            return;
        }
        let name = self.search.trim();
        if name.is_empty() {
            self.focus = SendToFocus::Message;
            return;
        }
        let recipient = Recipient::new_agent(name.to_owned());
        self.take_recipient(recipient);
    }

    fn take_row(&mut self, index: usize) {
        let Some(recipient) = self.rows.get(index).cloned() else {
            return;
        };
        self.take_recipient(recipient);
    }

    /// A recipient the person chose: it joins the chips, in the place of the
    /// new agent when the person is making one, and the keyboard goes on to
    /// the message.
    fn take_recipient(&mut self, recipient: Recipient) {
        self.focus = SendToFocus::Message;
        self.search.clear();
        self.search_selection = Selection::default();
        self.rows.clear();
        self.hovered = None;
        self.answered = false;
        if !self.to.iter().any(|taken| {
            taken.kind == recipient.kind && taken.id == recipient.id && taken.name == recipient.name
        }) {
            if let Some(placing) = self
                .to
                .iter()
                .position(|taken| taken.kind == RecipientKind::NewAgent && taken.id.is_empty())
                && !self
                    .to
                    .iter()
                    .any(|taken| taken.kind != RecipientKind::NewAgent)
            {
                self.to.remove(placing);
            }
            if self.to.len() < 8 {
                self.to.push(recipient);
            }
        }
        self.changed();
        self.layout = self.build_layout();
    }

    /// Whether the box has something to send: whom it goes to, and either the
    /// words or what they are about. A subject on its own is the message — a
    /// task sent to someone says what it is without a sentence beside it.
    #[must_use]
    pub fn can_send(&self) -> bool {
        !self.to.is_empty() && (!self.text.trim().is_empty() || !self.subjects.is_empty())
    }

    /// Send what the box holds: one request the session acts on as the
    /// person.
    pub fn send(&mut self) {
        if !self.can_send() {
            return;
        }
        let text = self.text.trim();
        let text: String = text
            .chars()
            .take(crate::shared_selection::MAX_SEND_CHARS)
            .collect();
        crate::shared_selection::send_to(SendToRequest {
            to: self.to.clone(),
            about: self.subjects.clone(),
            text,
        });
        self.sent = true;
        self.dirty = true;
    }

    /// Typing into the field the keyboard is in.
    fn edit(&mut self, event: &WindowEvent, modifiers: winit::event::Modifiers) -> bool {
        let command = self.command_modifier_down(modifiers);
        let shift = modifiers.state().shift_key();
        let (text, selection, composition) = match self.focus {
            SendToFocus::Recipients => (
                &mut self.search,
                &mut self.search_selection,
                &mut self.search_composition,
            ),
            SendToFocus::Message => (
                &mut self.text,
                &mut self.text_selection,
                &mut self.text_composition,
            ),
        };
        let change = composition.handle_event(event, text, selection, shift, command);
        match change {
            QueryEventChange::Ignored => false,
            QueryEventChange::Display => {
                self.dirty = true;
                true
            }
            QueryEventChange::Edited => {
                if self.focus == SendToFocus::Recipients {
                    self.ask_search();
                } else {
                    self.changed();
                }
                self.dirty = true;
                true
            }
        }
    }

    fn command_modifier_down(&self, modifiers: winit::event::Modifiers) -> bool {
        self.platform.command_modifier_down(modifiers.state())
    }

    /// Draw the box: the floating panel its menu stands in, its chips and
    /// fields on it.
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.appear.follow(self.open, theme.motion());
        if !self.appear.is_drawn() {
            return;
        }
        self.theme = theme.clone();
        self.text_scale = f64::from(theme.text_scale);
        self.layout = self.build_layout();
        let elevation = &theme.tokens.elevation.floating;
        let reach = f64::from(elevation.blur) * 2.0 + f64::from(elevation.offset_y).abs() + 2.0;
        let bounds = self.rect.inflate(reach, reach);
        let pose = self.appear.pose(self.rect, self.anchor);
        pose.paint(scene, transform, bounds, |scene, transform| {
            self.paint_body(scene, transform, text_renderer, theme);
        });
        self.dirty = false;
    }

    /// Its panel and what stands on it, drawn in the pose [`Self::paint`]
    /// gives: it arrives on what it is about and leaves alone.
    fn paint_body(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let radius = panel::radius(theme);
        panel::paint_rounded(scene, transform, self.rect, radius, theme);
        let ground = crate::chrome::panel_ground(theme);
        let tokens = &theme.tokens;
        let style = theme.text(TextRole::Body);
        let dim = tokens
            .colors
            .on_surface_at(ground, crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST)
            .ink_dim;
        let ink = tokens.colors.text_primary;
        let inset = f64::from(tokens.spacing.s2);
        let row = self.row_height();

        // The quiet line that names the box: whose message this is.
        let caption = theme.text(TextRole::Caption);
        let header_label = self.name();
        let (_, header_height) = text_renderer.measure_style(&header_label, None, &caption);
        text_renderer.draw_style(
            scene,
            &header_label,
            dim,
            transform.pre_translate(Vec2::new(
                self.layout.header.x0,
                self.layout.header.y0
                    + (self.layout.header.height() - f64::from(header_height)) / 2.0,
            )),
            None,
            &caption,
        );

        // The recipients' chips: each leads with the mark of what it is — the
        // accent's sparkle for the agent a message makes, their own avatar in
        // their colour for a person or an agent already here — and carries the
        // × that takes it off the message.
        for (index, chip) in self.layout.chips.iter().enumerate() {
            let armed = self.focused && self.focus == SendToFocus::Recipients && index == 0;
            let pill = RoundedRect::from_rect(*chip, radius.min(chip.height() / 2.0));
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tokens.colors.surfaces.raised,
                None,
                &pill,
            );
            if armed {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    tokens.colors.intent.accent_ink,
                    None,
                    &pill,
                );
            }
            let Some(recipient) = self.to.get(index) else {
                continue;
            };
            let mark = self.mark_size();
            let mark_rect =
                Rect::from_center_size((chip.x0 + self.mark_lead(), chip.center().y), (mark, mark));
            if recipient.kind == RecipientKind::NewAgent {
                crate::icons::draw_icon(
                    scene,
                    transform,
                    mark_rect,
                    crate::icons::Icon::Sparkles,
                    tokens.colors.intent.accent_ink,
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            } else {
                crate::widget::avatar::AvatarFace::new(&recipient.name, &recipient.id).paint(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    mark_rect,
                );
            }
            let remove = self.layout.removes.get(index).copied().flatten();
            let text_x = chip.x0 + self.text_lead();
            let room = remove.map_or(chip.x1 - row * 0.2, |remove| remove.x0);
            let shown = text_renderer.elide_to_width_style(
                &chip_name(recipient),
                (room - text_x - row * 0.1).max(1.0) as f32,
                &style,
            );
            let (_, height) = text_renderer.measure_style(&shown, None, &style);
            text_renderer.draw_style(
                scene,
                &shown,
                ink,
                transform.pre_translate(Vec2::new(
                    text_x,
                    chip.y0 + (chip.height() - f64::from(height)) / 2.0,
                )),
                None,
                &style,
            );
            if let Some(remove) = remove {
                let side = f64::from(tokens.metrics.icon_sm) * 0.8;
                crate::icons::draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(remove.center(), (side, side)),
                    crate::icons::Icon::X,
                    dim,
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
        }
        // The search field: what is typed, or what typing searches. A box
        // opened to the one place it sends to has none.
        let placeholder = crate::i18n::I18nManager::lookup_plain("send-to-search-placeholder");
        if !self.addressed {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tokens.colors.surfaces.raised,
                None,
                &RoundedRect::from_rect(
                    self.layout.search,
                    radius.min(self.layout.search.height() / 2.0),
                ),
            );
            self.search_composition.draw(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                QueryView {
                    text: &self.search,
                    selection: &self.search_selection,
                    placeholder: &placeholder,
                    show_caret: self.open && self.focused && self.focus == SendToFocus::Recipients,
                },
                // Its words stand in from its edge as the chips' marks do, so
                // the field and the chips beside it read as one row.
                Rect::new(
                    self.layout.search.x0 + self.mark_inset(),
                    self.layout.search.y0,
                    self.layout.search.x1,
                    self.layout.search.y1,
                ),
                &style,
                QueryInk::of(&tokens.colors),
            );
        }

        // What the search found, the row the keyboard is on armed.
        for (index, row) in self.layout.rows.iter().enumerate() {
            if self.hovered == Some(index) {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.surfaces.raised,
                    None,
                    &RoundedRect::from_rect(*row, radius.min(row.height() / 2.0)),
                );
            }
            let (name, detail) = self.rows.get(index).map_or(("", ""), |recipient| {
                (
                    recipient.name.as_str(),
                    recipient.detail.as_deref().unwrap_or(""),
                )
            });
            let text_x = row.x0 + inset;
            let shown = text_renderer.elide_to_width_style(
                name,
                (row.width() - inset * 2.0).max(1.0) as f32,
                &style,
            );
            let (_, height) = text_renderer.measure_style(&shown, None, &style);
            let text_y = row.y0 + (row.height() - f64::from(height)) / 2.0;
            text_renderer.draw_style(
                scene,
                &shown,
                ink,
                transform.pre_translate(Vec2::new(text_x, text_y)),
                None,
                &style,
            );
            if !detail.is_empty() {
                let caption = theme.text(TextRole::Caption);
                let (width, _) = text_renderer.measure_style(&shown, None, &style);
                let shown_detail = text_renderer.elide_to_width_style(
                    detail,
                    (row.x1 - (text_x + f64::from(width) + inset * 2.0)).max(1.0) as f32,
                    &caption,
                );
                text_renderer.draw_style(
                    scene,
                    &shown_detail,
                    dim,
                    transform.pre_translate(Vec2::new(
                        text_x + f64::from(width) + inset,
                        row.y0 + (row.height() - f64::from(height)) / 2.0,
                    )),
                    None,
                    &caption,
                );
            }
        }
        if self.answered && self.rows.is_empty() && !self.search.trim().is_empty() {
            let none = crate::i18n::I18nManager::lookup_plain("send-to-no-results");
            text_renderer.draw_style(
                scene,
                &none,
                dim,
                transform.pre_translate(Vec2::new(
                    self.rect.x0 + self.padding(),
                    self.layout.search.y1 + inset,
                )),
                None,
                &style,
            );
        }

        // The subjects: what the message is about, each led by the glyph of the
        // app that holds it, and the button that adds what the person has
        // selected now.
        for (index, chip) in self.layout.subjects.iter().enumerate() {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tokens.colors.surfaces.raised,
                None,
                &RoundedRect::from_rect(*chip, radius.min(chip.height() / 2.0)),
            );
            if let Some(subject) = self.subjects.get(index) {
                let mark = self.mark_size();
                crate::icons::draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(
                        (chip.x0 + self.mark_lead(), chip.center().y),
                        (mark, mark),
                    ),
                    subject_glyph(subject),
                    dim,
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
                let shown = text_renderer.elide_to_width_style(
                    &subject_label(subject),
                    (chip.width() - self.text_lead() - self.row_height() * 0.2).max(1.0) as f32,
                    &style,
                );
                let (_, height) = text_renderer.measure_style(&shown, None, &style);
                text_renderer.draw_style(
                    scene,
                    &shown,
                    ink,
                    transform.pre_translate(Vec2::new(
                        chip.x0 + self.text_lead(),
                        chip.y0 + (chip.height() - f64::from(height)) / 2.0,
                    )),
                    None,
                    &style,
                );
            }
        }
        // The button that adds what is selected now, drawn as the chip it
        // makes: its mark and its words, so it is read as what it adds rather
        // than a lone sign, and never drawn while it has nothing to add.
        let plus = self.layout.plus;
        if plus.width() > 0.0 {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tokens.colors.surfaces.raised,
                None,
                &RoundedRect::from_rect(plus, radius.min(plus.height() / 2.0)),
            );
            crate::icons::draw_icon(
                scene,
                transform,
                Rect::from_center_size(
                    (plus.x0 + self.mark_lead(), plus.center().y),
                    (self.mark_size(), self.mark_size()),
                ),
                crate::icons::Icon::Plus,
                dim,
                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            let shown = text_renderer.elide_to_width_style(
                &self.add_label(),
                (plus.width() - self.text_lead() - self.row_height() * 0.2).max(1.0) as f32,
                &style,
            );
            let (_, height) = text_renderer.measure_style(&shown, None, &style);
            text_renderer.draw_style(
                scene,
                &shown,
                dim,
                transform.pre_translate(Vec2::new(
                    plus.x0 + self.text_lead(),
                    plus.y0 + (plus.height() - f64::from(height)) / 2.0,
                )),
                None,
                &style,
            );
        }

        // The message: air enough to write in, its words wrapped inside it and
        // room made for them as they wrap.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(self.layout.message, radius * 0.75),
        );
        let message_placeholder = crate::i18n::I18nManager::lookup_plain(if self.addressed {
            "send-to-report-placeholder"
        } else {
            "send-to-message-placeholder"
        });
        self.text_composition.draw_wrapped(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            QueryView {
                text: &self.text,
                selection: &self.text_selection,
                placeholder: &message_placeholder,
                show_caret: self.open && self.focused && self.focus == SendToFocus::Message,
            },
            Rect::new(
                self.layout.message.x0 + inset,
                self.layout.message.y0,
                self.layout.message.x1 - inset,
                self.layout.message.y1,
            ),
            &style,
            QueryInk::of(&tokens.colors),
        );

        // Send: the accent once there is something to send, a quiet control
        // until then.
        let ready = self.can_send();
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            if ready {
                tokens.colors.intent.accent_ink
            } else {
                tokens.colors.surfaces.raised
            },
            None,
            &RoundedRect::from_rect(
                self.layout.send,
                radius.min(self.layout.send.height() / 2.0),
            ),
        );
        let label = crate::i18n::I18nManager::lookup_plain("send-to-send");
        let (width, height) = text_renderer.measure_style(&label, None, &style);
        text_renderer.draw_style(
            scene,
            &label,
            if ready { ground } else { dim },
            transform.pre_translate(Vec2::new(
                self.layout.send.center().x - f64::from(width) / 2.0,
                self.layout.send.center().y - f64::from(height) / 2.0,
            )),
            None,
            &style,
        );
    }

    /// What assistive technology reads: the recipient field with its chips,
    /// what the search found, the subjects, the message and Send, in the
    /// order the keyboard visits them.
    #[must_use]
    pub fn accessibility(&self) -> Option<AccessNode> {
        if !self.open {
            return None;
        }
        let mut position = 0usize;
        let total = 4 + self.rows.len().min(MAX_ROWS) + self.subjects.len();
        let mut action = |node: AccessNode| {
            position = position.saturating_add(1);
            node.with_position(position, total)
        };
        let chips: Vec<String> = self.to.iter().map(chip_name).collect();
        let mut children = Vec::new();
        children.push(action(
            AccessNode::new(Role::TextInput)
                .with_name(crate::i18n::I18nManager::lookup_plain(
                    "send-to-recipients-name",
                ))
                .with_optional_value(Some(chips.join(", ")))
                .with_action_target("send-to:recipients".to_owned())
                .with_bounds(self.layout.search)
                .with_state(AccessState {
                    focusable: true,
                    focused: self.focused && self.focus == SendToFocus::Recipients,
                    ..Default::default()
                }),
        ));
        for (index, recipient) in self.rows.iter().take(MAX_ROWS).enumerate() {
            children.push(action(
                AccessNode::new(Role::MenuItem)
                    .with_name(recipient.name.clone())
                    .with_description(recipient.detail.clone().unwrap_or_default())
                    .with_action_target(format!("send-to:recipient:{index}"))
                    .with_bounds(self.layout.rows.get(index).copied().unwrap_or(Rect::ZERO))
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.focused
                            && self.focus == SendToFocus::Recipients
                            && self.hovered == Some(index),
                        ..Default::default()
                    }),
            ));
        }
        for (index, subject) in self.subjects.iter().enumerate() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(subject_label(subject))
                    .with_bounds(
                        self.layout
                            .subjects
                            .get(index)
                            .copied()
                            .unwrap_or(Rect::ZERO),
                    ),
            );
        }
        // Subjects are read where they stand; adding one is the keyboard's
        // too, and stands next to them while it has something to add.
        if self.layout.plus.width() > 0.0 {
            children.push(action(
                AccessNode::new(Role::Button)
                    .with_name(self.add_label())
                    .with_action_target("send-to:add-subject".to_owned())
                    .with_bounds(self.layout.plus)
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    }),
            ));
        }
        children.push(action(
            AccessNode::new(Role::TextInput)
                .with_name(crate::i18n::I18nManager::lookup_plain(
                    "send-to-message-name",
                ))
                .with_optional_value(Some(self.text.clone()))
                .with_action_target("send-to:message".to_owned())
                .with_bounds(self.layout.message)
                .with_state(AccessState {
                    focusable: true,
                    focused: self.focused && self.focus == SendToFocus::Message,
                    ..Default::default()
                }),
        ));
        children.push(action(
            AccessNode::new(Role::Button)
                .with_name(crate::i18n::I18nManager::lookup_plain("send-to-send"))
                .with_action_target("send-to:send".to_owned())
                .with_bounds(self.layout.send)
                .with_state(AccessState {
                    focusable: true,
                    disabled: !self.can_send(),
                    ..Default::default()
                }),
        ));
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(self.name())
                .with_bounds(self.rect)
                .with_children(children),
        )
    }

    /// Assistive technology pressed or wrote in one of the box's parts.
    pub fn perform_semantic_action(&mut self, target: &str, action: &crate::a11y::Action) -> bool {
        if !self.open {
            return false;
        }
        match target {
            "send-to:recipients" => {
                if let crate::a11y::Action::SetValue(text) = action {
                    text.clone_into(&mut self.search);
                    self.search_selection = Selection::collapsed(self.search.len());
                    self.ask_search();
                    self.layout = self.build_layout();
                    return true;
                }
                false
            }
            "send-to:message" => {
                if let crate::a11y::Action::SetValue(text) = action {
                    text.clone_into(&mut self.text);
                    self.text_selection = Selection::collapsed(self.text.len());
                    self.changed();
                    return true;
                }
                false
            }
            "send-to:add-subject" => {
                if matches!(action, crate::a11y::Action::Activate) {
                    self.add_subject();
                    return true;
                }
                false
            }
            "send-to:send" => {
                if matches!(action, crate::a11y::Action::Activate) {
                    self.send();
                    return true;
                }
                false
            }
            other => {
                let Some(index) = other
                    .strip_prefix("send-to:recipient:")
                    .and_then(|index| index.parse::<usize>().ok())
                else {
                    return false;
                };
                if !matches!(action, crate::a11y::Action::Activate) {
                    return false;
                }
                self.take_row(index);
                true
            }
        }
    }

    /// Whether the box needs another frame: the search answered, a chip was
    /// added, the caret moved.
    #[must_use]
    pub fn needs_redraw(&self) -> bool {
        self.open && self.dirty
    }

    /// The part under `point`, for the box's own press routing.
    #[must_use]
    pub fn contains(&self, point: Point) -> bool {
        self.open && self.rect.contains(point)
    }
}

/// Whether `line` is the excerpt's own count of what it left out (`- and 3
/// more`), which names no subject.
fn is_overflow(line: &str) -> bool {
    line.strip_prefix("- and ")
        .and_then(|rest| rest.strip_suffix(" more"))
        .is_some_and(|count| !count.is_empty() && count.chars().all(|c| c.is_ascii_digit()))
}

/// What a subject chip reads: the words selected — the first line of them, and
/// how many more there are — else what the resource is called, else the item it
/// is.
#[must_use]
fn subject_label(subject: &SharedSelection) -> String {
    if let Some(excerpt) = subject.anchor.excerpt.as_deref() {
        let lines: Vec<String> = excerpt
            .lines()
            .map(str::trim)
            .filter(|line| !is_overflow(line))
            .map(|line| {
                line.strip_prefix("- ")
                    .unwrap_or(line)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|line| !line.is_empty())
            .collect();
        if let Some(first) = lines.first() {
            // A list's items are what the selection says it holds; its excerpt
            // lists as many as it could fit.
            let more = subject
                .anchor
                .items
                .len()
                .max(lines.len())
                .saturating_sub(1);
            let mut label: String = first.chars().take(48).collect();
            if more > 0 {
                label.push_str(&format!(" +{more}"));
            }
            return label;
        }
    }
    if let Some(label) = subject
        .resource
        .as_ref()
        .map(|resource| resource.label.trim())
        .filter(|label| !label.is_empty())
    {
        return label.to_owned();
    }
    subject
        .anchor
        .place
        .as_ref()
        .map(|place| place.item.clone())
        .or_else(|| subject.anchor.items.first().map(|item| item.item.clone()))
        .unwrap_or_default()
}

/// The glyph a subject chip leads with: the app whose surface holds the
/// resource the subject names.
#[must_use]
fn subject_glyph(subject: &SharedSelection) -> crate::icons::Icon {
    subject
        .resource_kind
        .as_deref()
        .and_then(|kind| kind.split('.').next())
        .map_or(crate::icons::Icon::AppWindow, |app| {
            crate::icons::app_glyph(app, None)
        })
}

/// What a recipient's chip reads: its name, and for the new agent nobody named
/// yet, that it is a new agent — the session names it by what it is asked.
fn chip_name(recipient: &Recipient) -> String {
    if recipient.kind == RecipientKind::NewAgent && recipient.name.is_empty() {
        crate::i18n::I18nManager::lookup_plain("send-to-new-agent")
    } else {
        recipient.name.clone()
    }
}

#[cfg(test)]
#[path = "send_to_tests.rs"]
mod tests;
