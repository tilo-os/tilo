//! [`RichInput`]: a field to type rich text into, bound to a `RichText` value: a chat's
//! composer (`rich_input(bind: draft)`). Its completion drawers are its own, the system's
//! behaviour of every rich input, never declared by a view: `@` offers the people of the room,
//! `:` the emoji, and the triggers the system gives sources for ([`RichInput::set_sources`]).
//! What a drawer picks goes in as a reference, a run marked `mention` that the text's readers
//! see as their own inline view of what it names, never as prose that happens to start with
//! `@`.
//!
//! It is a [`RichTextLine`] in a field's chrome that wraps and grows, whose keys stay the
//! field's own (no block editor takes Enter or the arrows), and that reports each
//! edit as the rich text it now holds. A value set from elsewhere (cleared after sending) is
//! shown when it differs from what the field last reported.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_ui_graph::rich::{Attr, Block, Embed, InlineView, RichText};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;

use crate::component::{Bound, Component, Described, Event, Person, Value};
use crate::i18n::I18nManager;
use crate::permissions::UserId;
use crate::presence::{ParticipantKind, PresenceCtx};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::completion::{CompletionSource, Tier};
use crate::widget::marks::{Mark, MarkSpan};
use crate::widget::mention_picker::{self, MentionCandidate};
use crate::widget::rich_text_line::RichTextLine;
use crate::widget::{Decorator, EventCtx, Widget};

/// A field to type rich text into, bound to the rich text it edits: each reference a
/// completion picks is written as a reference, which every reader sees as their own inline
/// view of what it names.
#[derive(Component)]
#[component(name = "RichInput", role = "TextInput", label = "placeholder")]
pub struct RichInputProps {
    /// The rich text it edits.
    #[component(bind(merge = "rich"))]
    pub value: Bound<RichText>,
    /// What to type, shown while it is empty.
    pub placeholder: Option<String>,
    /// Whether it can be typed into now.
    pub enabled: bool,
    /// Who spoke last in what it writes into, newest first: whom its `@` drawer offers first,
    /// under "In this conversation". The system gives it to every rich input.
    pub speakers: Vec<Person>,
    /// Who can read what it writes into: whom its `@` drawer offers, under "Members". The
    /// system gives it to every rich input; given no one, the drawer offers the room's roster.
    pub people: Vec<Person>,
    /// Enter was pressed: what was written is done (a composer's message is sent). With it,
    /// Enter sends rather than breaking the line; Shift+Enter still breaks it.
    #[component(event)]
    pub submit: Event,
}

impl Default for RichInputProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            placeholder: None,
            enabled: true,
            speakers: Vec::new(),
            people: Vec::new(),
            submit: Event::None,
        }
    }
}

/// The references picks wrote, waiting to be marked: each one's byte range and what it names.
type Picked = Rc<RefCell<Vec<(std::ops::Range<usize>, String)>>>;

/// See the [module docs](self).
pub struct RichInput {
    line: RichTextLine,
    value: Bound<RichText>,
    /// What the field last reported or was given: an equal value from the host changes
    /// nothing.
    shown: RichText,
    /// How each reference written here reads, by the `mention` mark's value.
    views: Rc<RefCell<HashMap<String, InlineView>>>,
    picked: Picked,
    /// The placeholder and the name the field was built with: a change builds it again.
    name: String,
    /// The revision of the bound value last looked at.
    seen: u64,
    /// Whom its `@` drawer offers, as the system gave them.
    people: DrawerPeople,
    /// The sources the system gave for its triggers ([`Self::set_sources`]).
    sources: Vec<CompletionSource>,
    /// The `@` source built from [`Self::people`], unless a source given for `@` takes its
    /// place.
    people_source: Option<CompletionSource>,
}

/// The people of a rich input's `@` drawer: who spoke last, then who can read it, and what the
/// room said each of them is when the drawer was last given them.
#[derive(Default)]
struct DrawerPeople {
    speakers: Vec<Person>,
    members: Vec<Person>,
    /// The kind and colour of each, in order, as the source was built with; `None` until built.
    built: Option<Vec<(ParticipantKind, (u8, u8, u8))>>,
}

impl DrawerPeople {
    fn is_empty(&self) -> bool {
        self.speakers.is_empty() && self.members.is_empty()
    }

    fn given(&mut self, speakers: Vec<Person>, members: Vec<Person>) {
        if speakers != self.speakers || members != self.members {
            self.speakers = speakers;
            self.members = members;
            self.built = None;
        }
    }

    /// The `@` source, when it changed since it was last built: those who spoke last (up to
    /// five), then the members who did not, each as the room says they are.
    fn source(&mut self, presence: &PresenceCtx) -> Option<CompletionSource> {
        if self.speakers.is_empty() && self.members.is_empty() {
            return None;
        }
        let said = |person: &Person| {
            let user = UserId(person.identity.clone());
            (presence.participant_kind(&user), presence.color_for(&user))
        };
        let now: Vec<_> = self
            .speakers
            .iter()
            .chain(&self.members)
            .map(said)
            .collect();
        if self.built.as_ref() == Some(&now) {
            return None;
        }
        let candidate = |person: &Person| {
            let (kind, color) = said(person);
            mention_picker::candidate(&MentionCandidate {
                id: person.identity.clone(),
                name: person.name.clone(),
                kind,
                color,
                is_self: presence.is_own_identity(&person.identity),
            })
        };
        let first: Vec<&Person> = self.speakers.iter().take(SPEAKERS).collect();
        let speakers = first.iter().map(|person| candidate(person)).collect();
        let members = self
            .members
            .iter()
            .filter(|person| !first.iter().any(|said| said.identity == person.identity))
            .map(candidate)
            .collect();
        self.built = Some(now);
        Some(CompletionSource::new(
            '@',
            vec![
                Tier::new(speakers)
                    .with_header(I18nManager::lookup_plain("completion-in-conversation"))
                    .with_limit(SPEAKERS),
                Tier::new(members).with_header(I18nManager::lookup_plain("completion-members")),
            ],
        ))
    }
}

/// How many of those who spoke last the drawer offers first (chat.md, "Completion drawers").
const SPEAKERS: usize = 5;

impl RichInput {
    /// The field over `value`.
    #[must_use]
    pub fn new(key: &str, value: Bound<RichText>, placeholder: &str, name: &str) -> Self {
        let mut line = RichTextLine::new();
        line.set_wrap(true);
        line.set_composer(true);
        let inner = line.inner_mut();
        inner.set_plain_chrome(false);
        inner.set_placeholder(placeholder);
        inner.set_completion(crate::widget::TextCompletion::ALL);
        let mut input = Self {
            line,
            value,
            shown: RichText::default(),
            views: Rc::new(RefCell::new(HashMap::new())),
            picked: Rc::new(RefCell::new(Vec::new())),
            name: name.to_owned(),
            seen: 0,
            people: DrawerPeople::default(),
            sources: Vec::new(),
            people_source: None,
        };
        let _ = key;
        let picked = Rc::clone(&input.picked);
        input
            .line
            .inner_mut()
            .set_on_reference_inserted(move |_, range, reference| {
                picked.borrow_mut().push((range, reference.to_owned()));
            });
        let given = input.value.state.with(Clone::clone);
        input.show(&given);
        input
    }

    /// Complete from `sources` too, one per trigger, as the system gives them: each
    /// candidate's view is what its reference reads as here once picked.
    pub fn set_sources(&mut self, sources: Vec<CompletionSource>) {
        self.sources = sources;
        self.offer();
    }

    /// The sources it completes from: those given, then the people of its `@` drawer where
    /// no source given is for `@`.
    fn offered(&self) -> Vec<CompletionSource> {
        let mut sources = self.sources.clone();
        if let Some(people) = &self.people_source
            && !sources.iter().any(|source| source.trigger == '@')
        {
            sources.push(people.clone());
        }
        sources
    }

    /// Give the field the sources it completes from: those given, and the people of its `@`
    /// drawer where no source given is for `@`.
    fn offer(&mut self) {
        let sources = self.offered();
        for candidate in sources
            .iter()
            .flat_map(|source| &source.tiers)
            .flat_map(|tier| &tier.candidates)
        {
            if let Some(reference) = &candidate.reference {
                let view = match &candidate.person {
                    Some(person) => InlineView::Person {
                        identity: person.identity.clone(),
                        name: candidate.name.clone(),
                    },
                    None => InlineView::Resource {
                        title: candidate.name.clone(),
                        icon: None,
                        detail: None,
                    },
                };
                self.views.borrow_mut().insert(reference.clone(), view);
            }
        }
        self.line.inner_mut().set_completion_sources(sources);
    }

    /// Show a value the host set from elsewhere (the draft cleared after sending), when it
    /// differs from what the field holds. Returns whether it showed one.
    fn follow(&mut self) -> bool {
        let revision = self.value.state.revision();
        if revision == self.seen {
            return false;
        }
        self.seen = revision;
        let given = self.value.state.with(Clone::clone);
        if given == self.shown {
            return false;
        }
        self.show(&given);
        true
    }

    /// Show `rich` in the field: its paragraphs as lines, each reference marked.
    fn show(&mut self, rich: &RichText) {
        let (text, marks) = line_of(rich);
        for embed in &rich.embeds {
            self.views
                .borrow_mut()
                .insert(embed.reference.clone(), embed.view.clone());
        }
        self.line.set_text(&text);
        self.line.set_marks(marks);
        self.shown = rich.clone();
    }

    /// The rich text the field holds now.
    #[must_use]
    pub fn rich(&self) -> RichText {
        rich_of(self.line.value(), self.line.marks(), &self.views.borrow())
    }

    /// Mark what picks wrote, and report the value when it changed. A person picked from the
    /// room's roster reads as their name.
    fn settle(&mut self) {
        let picked: Vec<_> = self.picked.borrow_mut().drain(..).collect();
        for (range, reference) in picked {
            if !self.views.borrow().contains_key(&reference) {
                let name = self
                    .line
                    .value()
                    .get(range.clone())
                    .unwrap_or_default()
                    .trim_start_matches('@')
                    .to_owned();
                self.views.borrow_mut().insert(
                    reference.clone(),
                    InlineView::Person {
                        identity: reference.clone(),
                        name,
                    },
                );
            }
            self.line
                .add_mark(MarkSpan::new(range, Mark::Mention(reference)));
        }
        let rich = self.rich();
        if rich != self.shown {
            self.shown = rich.clone();
            self.value.state.set(rich.clone());
            self.seen = self.value.state.revision();
            self.value.edited(Value::Rich(rich));
        }
    }
}

/// `rich` as one field's text, its paragraphs a line each, and its references as marks over
/// their runs (marks count characters; the field counts bytes).
fn line_of(rich: &RichText) -> (String, Vec<MarkSpan>) {
    let mut text = String::new();
    let mut marks = Vec::new();
    for (index, block) in rich.blocks.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        let base = text.len();
        for mark in &block.marks {
            let (Attr::Text(reference), "mention") = (&mark.value, mark.name.as_str()) else {
                continue;
            };
            let start = base + crate::widget::rich_doc::byte_of_char(&block.text, mark.start);
            let end = base + crate::widget::rich_doc::byte_of_char(&block.text, mark.end);
            if start < end {
                marks.push(MarkSpan::new(start..end, Mark::Mention(reference.clone())));
            }
        }
        text.push_str(&block.text);
    }
    (text, marks)
}

/// The rich text of a field holding `text` with `marks`: one paragraph per line, each mention
/// a `mention` mark, and how each reads.
fn rich_of(text: &str, marks: &[MarkSpan], views: &HashMap<String, InlineView>) -> RichText {
    if text.is_empty() {
        return RichText::default();
    }
    let mut blocks = Vec::new();
    let mut embeds: Vec<Embed> = Vec::new();
    let mut at = 0usize;
    for line in text.split('\n') {
        let end = at + line.len();
        let chars = |byte: usize| {
            line.get(..byte.saturating_sub(at).min(line.len()))
                .map_or(0, |head| head.chars().count())
        };
        let mut block = Block {
            kind: "paragraph".to_owned(),
            text: line.to_owned(),
            ..Block::default()
        };
        for span in marks {
            let Mark::Mention(reference) = &span.mark else {
                continue;
            };
            if span.range.start < at || span.range.end > end || span.range.is_empty() {
                continue;
            }
            block.marks.push(tilo_ui_graph::rich::Mark {
                start: chars(span.range.start),
                end: chars(span.range.end),
                name: "mention".to_owned(),
                value: Attr::Text(reference.clone()),
            });
            if !embeds.iter().any(|embed| embed.reference == *reference)
                && let Some(view) = views.get(reference)
            {
                embeds.push(Embed {
                    reference: reference.clone(),
                    view: view.clone(),
                });
            }
        }
        blocks.push(block);
        at = end + 1;
    }
    RichText { blocks, embeds }
}

impl RichInput {
    /// Fire `submit` on a submitting Enter, or, with none, let Enter break the line.
    fn submit_with(&mut self, submit: Event) {
        let callback: Option<Box<dyn FnMut(&str)>> = if submit.is_none() {
            None
        } else {
            Some(Box::new(move |_: &str| submit.fire(None)))
        };
        self.line.inner_mut().set_on_submit(callback);
    }
}

impl Described for RichInput {
    type Props = RichInputProps;

    fn from_props(key: &str, props: RichInputProps) -> Self {
        let RichInputProps {
            value,
            placeholder,
            enabled,
            speakers,
            people,
            submit,
        } = props;
        let placeholder = placeholder.unwrap_or_default();
        let name = if value.name.is_empty() {
            placeholder.clone()
        } else {
            value.name.clone()
        };
        let mut input = Self::new(key, value, &placeholder, &name);
        input.people.given(speakers, people);
        input.seen = input.value.state.revision();
        input.line.inner_mut().set_disabled(!enabled);
        input.line.inner_mut().set_accessible_name(&name);
        input.submit_with(submit);
        input
    }

    /// Placeholder, enabled and a value set from elsewhere change in place, so the field keeps
    /// its caret and its open drawer.
    fn set_props(&mut self, props: RichInputProps) -> bool {
        let RichInputProps {
            value,
            placeholder,
            enabled,
            speakers,
            people,
            submit,
        } = props;
        let placeholder = placeholder.unwrap_or_default();
        let name = if value.name.is_empty() {
            placeholder.clone()
        } else {
            value.name.clone()
        };
        if name != self.name {
            return false;
        }
        self.people.given(speakers, people);
        self.value = value;
        self.seen = u64::MAX;
        self.follow();
        self.line.inner_mut().set_placeholder(&placeholder);
        self.line.inner_mut().set_disabled(!enabled);
        self.submit_with(submit);
        true
    }
}

impl Decorator for RichInput {
    type Inner = RichTextLine;

    fn inner(&self) -> &RichTextLine {
        &self.line
    }

    fn inner_mut(&mut self) -> &mut RichTextLine {
        &mut self.line
    }

    fn name(&self) -> &'static str {
        "RichInput"
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.follow();
        self.line.build_layout(taffy, theme, node_map)
    }

    fn is_layout_dirty(&self) -> bool {
        self.value.state.revision() != self.seen || self.line.is_layout_dirty()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.line.handle_event(event, ctx);
        self.settle();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The room says what each of the drawer's people is (an agent is marked as one) with
        // the frame.
        if self.people.is_empty() && self.people_source.is_some() {
            self.people_source = None;
            self.offer();
        } else if let Some(source) = self.people.source(presence) {
            self.people_source = Some(source);
            self.offer();
        }
        self.line.draw(scene, transform, text, theme, presence);
    }

    /// The field's completion drawer floats above everything, as every field's does.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.line
            .inner_mut()
            .draw_overlay(scene, transform, text, theme, presence);
    }

    fn needs_animation_frame(&self) -> bool {
        self.line.needs_animation_frame() || self.line.inner().needs_animation_frame()
    }
}

#[cfg(test)]
#[path = "rich_input_tests.rs"]
mod tests;
