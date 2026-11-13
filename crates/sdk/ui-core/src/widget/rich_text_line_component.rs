//! [`RichTextLine`] as a described component: one editable line of text.

use crate::component::{Bound, Component, Described, Event, Value};
use crate::tokens::TextRole;

use super::{LineSignal, RichTextLine};
use crate::widget::DrawChild;
use crate::widget::TextInput;

/// One line of text edited in place (a block editor's cell, a composer's message), with its
/// inline marks, reporting the keys that end or break it so a host can split or join lines.
#[derive(Component)]
#[component(name = "RichTextLine", role = "TextInput", label = "placeholder")]
pub struct RichTextLineProps {
    /// The text it edits.
    #[component(bind(merge = "splice"))]
    pub value: Bound<String>,
    /// What to type, shown while it is empty.
    pub placeholder: Option<String>,
    /// How the text reads: `body` unless said.
    pub role: Option<TextRole>,
    /// The text is monospace (code).
    pub mono: bool,
    /// The line wraps to its width instead of scrolling.
    pub wrap: bool,
    /// Enter was pressed: the line ends at this byte.
    #[component(event(value = i64))]
    pub enter: Event,
    /// Shift+Enter was pressed: the line breaks at this byte, a soft break.
    #[component(event(value = i64))]
    pub soft_enter: Event,
    /// Backspace at the start was pressed: the line joins the one before it.
    #[component(event)]
    pub backspace_at_start: Event,
}

impl Default for RichTextLineProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            placeholder: None,
            role: None,
            mono: false,
            wrap: false,
            enter: Event::None,
            soft_enter: Event::None,
            backspace_at_start: Event::None,
        }
    }
}

/// A rich text line driven from a view: what it types edits the bound text, and the keys that
/// leave the line reach the view's events.
pub struct DeclaredLine {
    line: RichTextLine,
    enter: Event,
    soft_enter: Event,
    backspace: Event,
}

impl DeclaredLine {
    /// The line editing `props`'s bound text, its leaving keys reported to its events.
    #[must_use]
    pub fn new(props: RichTextLineProps) -> Self {
        let RichTextLineProps {
            value,
            placeholder,
            role,
            mono,
            wrap,
            enter,
            soft_enter,
            backspace_at_start,
        } = props;
        let line = build(&value, placeholder.as_deref(), role, mono, wrap);
        Self {
            line,
            enter,
            soft_enter,
            backspace: backspace_at_start,
        }
    }

    #[must_use]
    pub fn line(&mut self) -> &mut RichTextLine {
        &mut self.line
    }
}

fn build(
    value: &Bound<String>,
    placeholder: Option<&str>,
    role: Option<TextRole>,
    mono: bool,
    wrap: bool,
) -> RichTextLine {
    let edits = value.clone();
    let mut inner = TextInput::from_state(&value.state)
        .with_presence_path(value.path.clone())
        .with_plain_chrome()
        .with_completion(crate::widget::TextCompletion::EMOJI)
        .on_change(move |text| edits.edited(Value::Text(text.to_owned())));
    if let Some(placeholder) = placeholder {
        inner = inner.with_placeholder(placeholder);
    }
    let mut line = RichTextLine::from_input(inner);
    if let Some(role) = role {
        line = line.with_text_role(role);
    }
    if mono {
        line = line.with_mono();
    }
    if wrap {
        line.set_wrap(true);
    }
    line
}

impl crate::widget::Widget for DeclaredLine {
    fn needs_redraw(&self) -> bool {
        self.line.needs_redraw() || self.line.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.line.mark_drawn();
    }

    fn needs_animation_frame(&self) -> bool {
        self.line.needs_animation_frame()
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.line.sync(state, path);
        self.report();
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let changed = self.line.update(state, path);
        changed | self.report()
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> taffy::NodeId {
        self.line.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: crate::kurbo::Point,
    ) {
        self.line.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: crate::kurbo::Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.line
            .draw_child(scene, transform, text, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        self.line.handle_event(event, ctx);
    }

    fn get_rect(&self) -> crate::kurbo::Rect {
        self.line.get_rect()
    }

    fn name(&self) -> &'static str {
        "RichTextLine"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.line.accessibility()
    }
}

impl DeclaredLine {
    /// Report the line's last signal as the view's events; `true` when it fired one.
    fn report(&mut self) -> bool {
        let Some(signal) = self.line.take_signal() else {
            return false;
        };
        let at = |byte: usize| Some(Value::Int(i64::try_from(byte).unwrap_or(0)));
        match signal {
            LineSignal::Enter { split_at } => self.enter.fire(at(split_at)),
            LineSignal::SoftNewline { split_at } => self.soft_enter.fire(at(split_at)),
            LineSignal::BackspaceAtStart => self.backspace.fire(None),
            _ => return false,
        }
        true
    }
}

impl Described for DeclaredLine {
    type Props = RichTextLineProps;

    fn from_props(_key: &str, props: RichTextLineProps) -> Self {
        Self::new(props)
    }

    fn set_props(&mut self, props: RichTextLineProps) -> bool {
        let RichTextLineProps {
            value,
            role,
            mono,
            wrap,
            placeholder,
            enter,
            soft_enter,
            backspace_at_start,
        } = props;
        let caret = self.line.cursor_pos();
        self.enter = enter;
        self.soft_enter = soft_enter;
        self.backspace = backspace_at_start;
        self.line = build(&value, placeholder.as_deref(), role, mono, wrap);
        self.line.set_cursor_pos(caret.min(value.state.get().len()));
        true
    }
}
