//! References in the block editor's text, and the editor as the described component (`ui.BlockEditor`):
//! rich text shown read-only (tilolang's `rich_text(value)`), and a document's body edited through the
//! field a view binds it to (its [`body`](super::block_editor_body)).

use std::collections::HashMap;
use std::rc::Rc;

use tilo_ui_graph::rich::{InlineView, RichText, Run};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::BlockEditor;
use crate::component::{Bound, Component, Described, Event, Link, PropType, Type, Value};
use crate::theme::Theme;
use crate::widget::block_editor_body::BodyBinding;
use crate::widget::marks::{Mark, MentionColors, MentionPaint};

pub const OPEN_REFERENCE: &str = "open_reference";

impl PropType for RichText {
    fn ty() -> Type {
        Type::RichText
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Rich(rich) => Ok(rich.clone()),
            other => Err(format!("takes RichText, not {}", other.kind())),
        }
    }
}

/// A document: its paragraphs with their formatting, and each reference in it as the reader's own
/// inline view of what it refers to (a person with their name, a channel, a task with its number),
/// resolved under their permissions.
#[derive(Component, Default)]
#[component(name = "BlockEditor", role = "Document")]
pub struct BlockEditorProps {
    /// The document it edits, as its paragraphs (a `RichText` field).
    #[component(bind(merge = "rich"))]
    pub rich: Bound<RichText>,
    /// The document it edits, as Markdown (a `Text` field), the same text round-tripped.
    #[component(bind(merge = "splice"))]
    pub markdown: Bound<String>,
    /// Rich text it shows read-only, when it binds nothing.
    #[component(positional)]
    pub value: Option<RichText>,
    /// What to type, shown while the editor is empty.
    pub placeholder: Option<String>,
    /// A reference in the text was activated, carrying the reference.
    #[component(event(value = String))]
    pub open_reference: Event,
    /// The records a `[[` link offers, each as it reads for the person now: choosing one writes a reference
    /// to it in the text, which every reader reads as it reads now.
    pub links: Vec<Link>,
    /// The `[[` link picker opened or closed: what a view answering `links_query` keeps to offer records
    /// only while the picker shows them.
    #[component(event(value = bool))]
    pub links_open: Event,
    /// The text typed into the open `[[` link picker, each time it changes: a view whose records are a
    /// collection asks the store for what it holds of it, and hands back `links` — a page of them, read
    /// when the picker shows it rather than on every frame of the document.
    #[component(event(value = String))]
    pub links_query: Event,
    /// The commands the editor's slash menu offers besides the block kinds, by their labels: choosing one
    /// reports it as `command`.
    pub commands: Vec<String>,
    /// A command of `commands` was chosen, carrying its label: the view does what the label says, as it
    /// would for a button it placed.
    #[component(event(value = String))]
    pub command: Event,
    /// The document's headings, in the order they stand, each time they change.
    #[component(event(value = Vec<String>))]
    pub headings: Event,
}

/// The editor's references: how each reads, where each was painted, and what activating one does.
#[derive(Default)]
pub(super) struct References {
    /// How each reference reads for the person reading, by the `mention` mark's value.
    views: Rc<HashMap<String, InlineView>>,
    /// What each reference the text projects (an embed) shows of its own content, by the reference: one
    /// level deep, as its reader reads it.
    previews: Rc<HashMap<String, Vec<Run>>>,
    painted_for: Option<u64>,
    owner_paint: Option<MentionPaint>,
    shown: Option<RichText>,
    wake_at: Option<crate::time::Instant>,
    /// Where each reference was painted in the last draw.
    hits: Vec<(Rect, String)>,
    pub(super) on_open: Option<Box<dyn FnMut(&str)>>,
    headings: Event,
    /// The headings last reported to it: it is told again only when they change.
    reported: Option<Vec<String>>,
    links_open: Event,
    links_query: Event,
    /// What the picker last told the view, which it hears again only when that changes: whether it was
    /// open, and the text it held.
    picker: Option<(bool, String)>,
}

impl BlockEditor {
    /// Show `rich` read-only, each reference as it reads for the person reading.
    pub fn show_rich_text(&mut self, rich: &RichText) -> bool {
        let changed_references = self.show_references(rich);
        if self.references.shown.as_ref() != Some(rich) {
            self.references.shown = Some(rich.clone());
        }
        let changed = self.present_shown();
        changed || changed_references
    }

    /// How each reference in `rich` reads here, and what each the text projects shows: the chips'
    /// titles and the cards' previews.
    pub(crate) fn show_references(&mut self, rich: &RichText) -> bool {
        let views: HashMap<String, InlineView> = rich
            .embeds
            .iter()
            .map(|embed| (embed.reference.clone(), embed.view.clone()))
            .collect();
        let previews: HashMap<String, Vec<Run>> = rich
            .embeds
            .iter()
            .filter_map(|embed| Some((embed.reference.clone(), embed.preview.clone()?)))
            .collect();
        let changed_previews = *self.references.previews != previews;
        if changed_previews {
            self.references.previews = Rc::new(previews);
            self.dirty = true;
            self.structure_dirty = true;
            self.stack.width = None;
        }
        let changed_views = *self.references.views != views;
        if changed_views {
            self.references.views = Rc::new(views);
            self.references.painted_for = None;
        }
        changed_previews || changed_views
    }

    fn present_shown(&mut self) -> bool {
        let Some(rich) = self.references.shown.take() else {
            return false;
        };
        let reading = crate::widget::rich_doc::reading_here();
        let changed = self.present_blocks(&crate::widget::rich_doc::blocks_at(&rich, &reading));
        self.references.wake_at = crate::widget::rich_doc::changes_in(&rich, reading.now)
            .map(|soon| crate::time::now() + soon);
        self.references.shown = Some(rich);
        changed
    }

    pub(super) fn references_wake(&self) -> Option<crate::time::Instant> {
        self.references.wake_at
    }

    /// Run `open` with the reference activated, each time one is.
    pub fn on_open_reference(&mut self, open: impl FnMut(&str) + 'static) {
        self.references.on_open = Some(Box::new(open));
    }

    /// The references painted in the last draw, with where each was painted.
    #[must_use]
    pub fn reference_hits(&self) -> &[(Rect, String)] {
        &self.references.hits
    }

    #[must_use]
    pub fn reference_at(&self, point: Point) -> Option<&str> {
        self.references
            .hits
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, reference)| reference.as_str())
    }

    /// The links inside the cards painted in the last draw, with where each was painted: what a click
    /// on one opens, and where to point to open it.
    #[must_use]
    pub fn card_link_hits(&self) -> &[(Rect, String)] {
        &self.card_links
    }

    /// How the record `reference` reads now, as a card of it is titled: its title for the person
    /// reading, `written` for one this reader has no reading of, and how a record they cannot read
    /// reads.
    pub(super) fn reference_title(&self, reference: &str, written: &str) -> String {
        match self.references.views.get(reference) {
            Some(InlineView::Resource { title, .. }) if !title.is_empty() => title.clone(),
            Some(InlineView::Person { name, .. }) if !name.is_empty() => name.clone(),
            Some(InlineView::Unreadable) => crate::widget::rich_doc::unreadable_phrase(),
            Some(
                InlineView::Elsewhere | InlineView::Resource { .. } | InlineView::Person { .. },
            )
            | None => written.to_owned(),
        }
    }

    /// What the record `reference` shows of its own content where the text projects it, as its reader
    /// reads it: one level deep, a link inside it never opened out.
    pub(super) fn reference_preview(&self, reference: &str) -> Option<&[Run]> {
        self.references
            .previews
            .get(reference)
            .map(std::vec::Vec::as_slice)
    }

    pub(super) fn set_owner_mention_paint(&mut self, paint: Option<MentionPaint>) {
        self.references.owner_paint = paint;
        self.references.painted_for = None;
        self.dirty = true;
    }

    /// Paint the references in `theme`'s colours when the theme, how they read or the owner's paint
    /// changed since: a resource as a chip, one the reader cannot read as a quiet chip, a person in the
    /// owner's colours for them.
    pub(super) fn paint_references(&mut self, theme: &Theme) {
        if self
            .references
            .wake_at
            .is_some_and(|wake| wake <= crate::time::now())
        {
            self.present_shown();
        }
        let fingerprint = theme_fingerprint(theme);
        if self.references.painted_for == Some(fingerprint) {
            return;
        }
        self.references.painted_for = Some(fingerprint);
        let colors = &theme.tokens.colors;
        let ground = colors.surfaces.base;
        let accent = colors.intent.accent_ink;
        let contrast = crate::tokens::contrast::BODY_TEXT_MIN_CONTRAST;
        let band = crate::tokens::colors::mix8(accent, ground, 0.86);
        let resource = MentionColors {
            ink: colors.ink_on(band, accent, contrast),
            band: Some(band),
        };
        let unreadable = MentionColors {
            ink: colors.ink_on(colors.surfaces.raised, colors.text_dim, contrast),
            band: Some(colors.surfaces.raised),
        };
        let person = MentionColors {
            ink: colors.ink_on(ground, accent, contrast),
            band: None,
        };
        let views = Rc::clone(&self.references.views);
        let owner = self.references.owner_paint.clone();
        let of_person =
            move |reference: &str| owner.as_ref().map_or(person, |paint| paint(reference));
        self.apply_mention_paint(Some(Rc::new(move |reference: &str| {
            match views.get(reference) {
                Some(InlineView::Unreadable) => unreadable,
                Some(InlineView::Resource { .. } | InlineView::Elsewhere) => resource,
                None if tilo_reference::embed::names_resource(reference) => resource,
                Some(InlineView::Person { .. }) | None => of_person(reference),
            }
        })));
    }

    /// Where each reference of the painted blocks was painted.
    pub(super) fn refresh_reference_hits(&mut self, theme: &Theme, painted: &[bool]) {
        self.references.hits.clear();
        for (block, _) in self
            .blocks
            .iter()
            .zip(painted)
            .filter(|(_, painted)| **painted)
        {
            let line = &block.line;
            for span in line.marks() {
                let Mark::Mention(reference) = &span.mark else {
                    continue;
                };
                for rect in line.mark_rects(theme, span.range.clone()) {
                    self.references.hits.push((rect, reference.clone()));
                }
            }
        }
    }

    /// Activate the reference a click on read-only text lands on: `true` when one was.
    pub(super) fn activate_reference(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        let Some(reference) = self.reference_at(self.last_mouse_pos).map(str::to_owned) else {
            return false;
        };
        if let Some(open) = self.references.on_open.as_mut() {
            open(&reference);
        }
        true
    }
}

fn theme_fingerprint(theme: &Theme) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let colors = &theme.tokens.colors;
    for color in [
        colors.surfaces.base,
        colors.surfaces.raised,
        colors.intent.accent_ink,
        colors.text_dim,
    ] {
        color.to_rgba8().to_u32().hash(&mut hasher);
    }
    hasher.finish()
}

impl Described for BlockEditor {
    type Props = BlockEditorProps;

    fn from_props(key: &str, props: BlockEditorProps) -> Self {
        let BlockEditorProps {
            rich,
            markdown,
            value,
            placeholder,
            open_reference,
            links,
            links_open,
            links_query,
            commands,
            command,
            headings,
        } = props;
        let editing = BodyBinding::bound(&rich, &markdown);
        let mut editor = if editing {
            let mut editor = Self::new()
                .with_id(key)
                .with_wrap_lines(true)
                .with_compact_paragraph_spacing(true);
            editor.set_find_replace_enabled(true);
            editor
        } else {
            Self::new()
                .with_wrap_lines(true)
                .with_compact_paragraph_spacing(true)
                .with_block_handles(false)
                .with_flush_text()
                .with_read_only(true)
        };
        editor.offer(
            links,
            links_open,
            links_query,
            &commands,
            command,
            headings,
            Some(open_reference),
        );
        editor.set_image_store(Some(crate::pictures::Pictures::window().store().clone()));
        if editing {
            let (body, placeholder) = BodyBinding::of(rich, markdown, placeholder);
            editor.set_empty_hint(&placeholder);
            editor.show_body(&body);
            editor.body = Some(body);
        } else {
            editor.show_rich_text(&value.unwrap_or_default());
        }
        editor.publish_headings();
        editor
    }

    fn set_props(&mut self, props: BlockEditorProps) -> bool {
        let BlockEditorProps {
            rich,
            markdown,
            value,
            placeholder,
            open_reference,
            links,
            links_open,
            links_query,
            commands,
            command,
            headings,
        } = props;
        let editing = BodyBinding::bound(&rich, &markdown);
        if editing != self.body.is_some() {
            return false;
        }
        let showing = self.body.is_none();
        self.offer(
            links,
            links_open,
            links_query,
            &commands,
            command,
            headings,
            Some(open_reference),
        );
        self.set_find_replace_enabled(!showing);
        let rebound = if showing {
            self.show_rich_text(&value.unwrap_or_default());
            true
        } else {
            self.rebind_body(rich, markdown, &placeholder.unwrap_or_default())
        };
        self.publish_headings();
        rebound
    }
}

impl BlockEditor {
    fn offer(
        &mut self,
        links: Vec<Link>,
        links_open: Event,
        links_query: Event,
        commands: &[String],
        command: Event,
        headings: Event,
        open_reference: Option<Event>,
    ) {
        let answering = !links_query.is_none();
        self.set_links(
            links
                .iter()
                .map(|link| (link.reference.clone(), link.title.clone())),
        );
        if answering {
            self.link_pages_offered = true;
            self.refresh_slash_commands();
        }
        self.link_popover.set_narrows_itself(!answering);
        self.references.links_open = links_open;
        self.references.links_query = links_query;
        // A picker just built is closed and holds nothing: a view already offers nothing there,
        // so it is told only of what changes from here on.
        self.references
            .picker
            .get_or_insert_with(|| (false, String::new()));
        self.set_view_commands(commands);
        self.on_command(move |label| {
            command.fire(Some(Value::Text(label.to_owned())));
        });
        if let Some(open_reference) = open_reference {
            self.on_open_reference(move |reference| {
                open_reference.fire(Some(Value::Text(reference.to_owned())));
            });
        }
        self.references.headings = headings;
    }

    pub(super) fn publish_link_query(&mut self) {
        let open = self.link_popover.is_visible();
        let text = self.link_popover.filter().to_owned();
        let before = self.references.picker.replace((open, text.clone()));
        if before
            .as_ref()
            .is_some_and(|(was_open, was_text)| *was_open == open && *was_text == text)
        {
            return;
        }
        if before
            .as_ref()
            .is_none_or(|(was_open, _)| *was_open != open)
        {
            self.references.links_open.fire(Some(Value::Bool(open)));
        }
        if open
            && before
                .as_ref()
                .is_none_or(|(_, was_text)| *was_text != text)
        {
            self.references.links_query.fire(Some(Value::Text(text)));
        }
    }

    pub(super) fn publish_headings(&mut self) {
        if self.references.headings.is_none() {
            return;
        }
        let headings: Vec<String> = self.headings().into_iter().map(|(_, text)| text).collect();
        if self.references.reported.as_ref() == Some(&headings) {
            return;
        }
        self.references.reported = Some(headings.clone());
        self.references.headings.fire(Some(Value::List(
            headings.into_iter().map(Value::Text).collect(),
        )));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as Map;

    use taffy::{Dimension, Size, Style, TaffyTree};
    use tilo_ui_graph::rich::{Attr, Block, Embed, InlineView, Mark as RichMark, RichText};
    use vello::Scene;
    use vello::kurbo::{Affine, Point};

    use super::*;
    use crate::component::{Fired, Intent};
    use crate::presence::PresenceCtx;
    use crate::text_render::TextRenderer;
    use crate::widget::Widget as _;

    const TASK: &str = r#"{"app":"so.tilo.lang.board","id":"tl:so.tilo.lang.board:d/f:t1","kind":"tl.so.tilo.lang.board.Task"}"#;
    const SECRET: &str =
        r#"{"app":"so.tilo.lang.board","id":"tl:so.tilo.lang.board:hidden","kind":""}"#;

    fn mention(start: usize, end: usize, value: &str) -> RichMark {
        RichMark {
            start,
            end,
            name: "mention".into(),
            value: Attr::Text(value.into()),
        }
    }

    /// `Ask @ada about Fix it, not secret.` with a person, a task and a resource the reader cannot
    /// read, read as `views` says.
    fn text(views: Vec<(&str, InlineView)>) -> RichText {
        RichText {
            blocks: vec![Block {
                kind: "paragraph".into(),
                text: "Ask @ada about Fix it, not secret.".into(),
                marks: vec![
                    mention(4, 8, "u-ada"),
                    mention(15, 21, TASK),
                    mention(27, 33, SECRET),
                ],
                attrs: std::collections::BTreeMap::new(),
            }],
            embeds: views
                .into_iter()
                .map(|(reference, view)| Embed {
                    reference: reference.into(),
                    view,
                    preview: None,
                })
                .collect(),
        }
    }

    fn laid_out(editor: &mut BlockEditor, renderer: &mut TextRenderer) {
        let theme = Theme::light();
        let mut tree = TaffyTree::new();
        let mut nodes = Map::new();
        let node = editor.build_layout(&mut tree, &theme, &mut nodes);
        let root = tree
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(720.0),
                        height: Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        tree.compute_layout_with_measure(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(720.0),
                height: taffy::AvailableSpace::MaxContent,
            },
            |known, available, node, _, _| {
                nodes.get(&node).map_or(Size::ZERO, |widget| unsafe {
                    (**widget).measure(known, available, renderer)
                })
            },
        )
        .unwrap();
        editor.apply_layout(&tree, node, Point::ORIGIN);
        editor.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            renderer,
            &theme,
            &PresenceCtx::empty(),
        );
    }

    #[test]
    fn each_reference_reads_as_the_readers_inline_view_and_a_click_opens_it() {
        let queue = crate::Intents::<Fired>::default();
        let props = BlockEditorProps {
            value: Some(text(vec![
                (
                    "u-ada",
                    InlineView::Person {
                        identity: "u-ada".into(),
                        name: "Ada Lovelace".into(),
                    },
                ),
                (
                    TASK,
                    InlineView::Resource {
                        title: "Fix the build".into(),
                        icon: None,
                        detail: Some("T-12".into()),
                    },
                ),
                (SECRET, InlineView::Unreadable),
            ])),
            open_reference: Event::View(Intent::new(&queue, "post", OPEN_REFERENCE)),
            ..BlockEditorProps::default()
        };
        let mut editor = BlockEditor::from_props("post", props);
        assert!(editor.is_read_only());
        let shown = editor.blocks()[0].text().to_owned();
        assert_eq!(
            shown, "Ask @Ada Lovelace about T-12 Fix the build, not Not shared with you.",
            "a person by their name now, a task by its number and title now, and what the \
             reader cannot read only as being there"
        );
        assert!(
            !shown.contains("secret"),
            "nothing of an unreadable resource shows"
        );

        let mut renderer = TextRenderer::new();
        laid_out(&mut editor, &mut renderer);
        let hits = editor.reference_hits().to_vec();
        let task = hits
            .iter()
            .find(|(_, reference)| reference == TASK)
            .map(|(rect, _)| *rect)
            .expect("the task is painted as a chip");
        assert!(hits.iter().any(|(_, reference)| reference == "u-ada"));
        assert!(hits.iter().any(|(_, reference)| reference == SECRET));
        assert_eq!(
            editor.cursor_shape(task.center()),
            Some(winit::window::CursorIcon::Pointer)
        );

        crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::cursor_moved(task.center().x, task.center().y),
        );
        assert!(crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::mouse_release()
        ));
        assert_eq!(
            queue.take(),
            vec![Fired {
                key: "post".into(),
                event: OPEN_REFERENCE.into(),
                value: Some(Value::Text(TASK.into())),
            }],
            "a click opens the reference, by the reference"
        );
        crate::testing::dispatch(&mut editor, &crate::synthetic_input::cursor_moved(1.0, 1.0));
        crate::testing::dispatch(&mut editor, &crate::synthetic_input::mouse_release());
        assert!(
            queue.take().is_empty(),
            "a click beside a reference opens nothing"
        );
    }

    fn type_keys(editor: &mut BlockEditor, text: &str) {
        for ch in text.chars() {
            let typed = ch.to_string();
            let event = if ch == ' ' {
                crate::synthetic_input::key_press(
                    winit::keyboard::Key::Named(winit::keyboard::NamedKey::Space),
                    Some(" "),
                )
            } else {
                crate::synthetic_input::char_key(&typed)
            };
            crate::testing::dispatch(editor, &event);
        }
    }

    #[test]
    fn the_records_a_view_links_are_offered_and_chosen_as_references_to_them() {
        use crate::widget::CursorPlacement;
        use crate::widget::marks::Mark;

        let fired = crate::Intents::<Fired>::default();
        let body = crate::LocalState::new(RichText::default());
        let plan = |title: &str| Link {
            reference: "tl:so.tilo.notes:tl.so.tilo.notes.d/f:t1".into(),
            title: title.into(),
        };
        let mut editor = BlockEditor::from_props(
            "b",
            BlockEditorProps {
                rich: bound(&fired, body.clone()),
                links: vec![plan("Plan")],
                ..BlockEditorProps::default()
            },
        );
        editor.focus_block(0, CursorPlacement::End);
        type_keys(&mut editor, "[[pl");
        assert!(editor.link_popover().is_visible());
        assert_eq!(
            editor
                .link_popover()
                .highlighted()
                .map(|page| page.title.as_str()),
            Some("Plan"),
            "the record the view named, by the title it reads as now"
        );
        crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::key_press(
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
                None,
            ),
        );
        assert_eq!(editor.blocks()[0].text(), "Plan");
        eprintln!("MARKS {:?}", editor.blocks()[0].line.marks());
        assert!(
            matches!(
                editor.blocks()[0].line.marks().first().map(|span| &span.mark),
                Some(Mark::Mention(reference)) if reference == "tl:so.tilo.notes:tl.so.tilo.notes.d/f:t1"
            ),
            "what is written names the record, so it reads as it reads now wherever it goes"
        );

        editor.take_body_edits();
        let edit = fired
            .take()
            .into_iter()
            .rev()
            .find_map(|fired| match fired.value {
                Some(Value::Rich(rich)) => Some(rich),
                _ => None,
            });
        let edited = edit.expect("the view is told what the document is now");
        let mention = edited.blocks[0]
            .marks
            .iter()
            .find(|mark| mark.name == "mention")
            .expect("a mention mark");
        assert_eq!(
            edited.blocks[0].text, "Plan",
            "the run reads as the record's title"
        );
        assert!(matches!(
            &mention.value,
            tilo_ui_graph::rich::Attr::Text(reference)
                if reference == "tl:so.tilo.notes:tl.so.tilo.notes.d/f:t1"
        ));

        // A record renamed later is offered by its new title: the view names the record, and the
        // title is how it reads now, never text kept from before.
        editor.set_props(BlockEditorProps {
            rich: bound(&fired, body),
            links: vec![plan("Plan for Q4")],
            ..BlockEditorProps::default()
        });
        assert_eq!(
            editor
                .link_popover()
                .highlighted()
                .map(|page| page.title.as_str()),
            Some("Plan for Q4")
        );
    }

    #[test]
    fn the_picker_tells_its_view_when_it_opens_and_what_is_typed_in_it() {
        use crate::widget::CursorPlacement;

        let fired = crate::Intents::<Fired>::default();
        let asked = crate::Intents::<Fired>::default();
        let body = crate::LocalState::new(RichText::default());
        let link = |title: &str| Link {
            reference: "tl:so.tilo.notes:tl.so.tilo.notes.d/f:t1".into(),
            title: title.into(),
        };
        let props = |links: Vec<Link>| BlockEditorProps {
            rich: bound(&fired, body.clone()),
            links_open: Event::View(Intent::new(&asked, "b", "links_open")),
            links_query: Event::View(Intent::new(&asked, "b", "links_query")),
            links,
            ..BlockEditorProps::default()
        };
        let mut editor = BlockEditor::from_props("b", props(Vec::new()));
        editor.focus_block(0, CursorPlacement::End);
        type_keys(&mut editor, "[[gr");

        // It is told the picker opened, and each text typed into it in turn.
        let told = asked.take();
        assert!(
            matches!(
                told.first(),
                Some(Fired { event, value: Some(Value::Bool(true)), .. }) if event == "links_open"
            ),
            "the picker opening reaches the view: {told:?}"
        );
        let texts: Vec<&str> = told
            .iter()
            .filter(|fired| fired.event == "links_query")
            .filter_map(|fired| match &fired.value {
                Some(Value::Text(text)) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["g", "gr"], "what was typed: {told:?}");

        editor.set_props(props(vec![link("Groceries")]));
        assert_eq!(
            editor
                .link_popover()
                .highlighted()
                .map(|page| page.title.as_str()),
            Some("Groceries"),
            "the record the view found by what was typed"
        );

        // Closing tells it the picker is shut, so a view reading a collection stops.
        crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::key_press(
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                None,
            ),
        );
        assert!(!editor.link_popover().is_visible());
        assert_eq!(
            asked.take(),
            vec![Fired {
                key: "b".into(),
                event: "links_open".into(),
                value: Some(Value::Bool(false)),
            }],
            "the picker closing reaches the view"
        );
    }

    fn embedding(views: Vec<(&str, InlineView)>) -> RichText {
        RichText {
            blocks: vec![
                Block {
                    kind: "paragraph".into(),
                    text: "Groceries".into(),
                    marks: vec![RichMark {
                        start: 0,
                        end: 9,
                        name: crate::widget::rich_doc::EMBED_MARK.into(),
                        value: Attr::Text(GROCERIES.into()),
                    }],
                    attrs: std::collections::BTreeMap::new(),
                },
                Block {
                    kind: "paragraph".into(),
                    text: "and later".into(),
                    marks: Vec::new(),
                    attrs: std::collections::BTreeMap::new(),
                },
            ],
            embeds: views
                .into_iter()
                .map(|(reference, view)| Embed {
                    reference: reference.into(),
                    view,
                    preview: (reference == GROCERIES).then(|| {
                        vec![
                            tilo_ui_graph::rich::Run {
                                text: "Oat milk".into(),
                                reference: None,
                            },
                            tilo_ui_graph::rich::Run {
                                text: " Plan".into(),
                                reference: Some(PLAN.into()),
                            },
                            tilo_ui_graph::rich::Run {
                                text: " Groceries".into(),
                                reference: Some(GROCERIES.into()),
                            },
                        ]
                    }),
                })
                .collect(),
        }
    }

    const GROCERIES: &str = "tl:so.tilo.lang.notes:tl.so.tilo.lang.notes.d/f:g1";
    const PLAN: &str = "tl:so.tilo.lang.notes:tl.so.tilo.lang.notes.d/f:p1";

    #[test]
    fn an_embed_of_a_record_shows_its_start_and_opens_what_it_names() {
        let queue = crate::Intents::<Fired>::default();
        let mut editor = BlockEditor::from_props(
            "n",
            BlockEditorProps {
                value: Some(embedding(vec![
                    (
                        GROCERIES,
                        InlineView::Resource {
                            title: "Shopping".into(),
                            icon: None,
                            detail: None,
                        },
                    ),
                    (
                        PLAN,
                        InlineView::Resource {
                            title: "Plan".into(),
                            icon: None,
                            detail: None,
                        },
                    ),
                ])),
                open_reference: Event::View(Intent::new(&queue, "n", OPEN_REFERENCE)),
                ..BlockEditorProps::default()
            },
        );
        assert_eq!(
            editor.blocks()[0].text(),
            "Shopping",
            "the card reads as the record's title now, not the one its run kept"
        );
        assert_eq!(editor.blocks().len(), 2, "an embed opens nothing out");

        let mut renderer = TextRenderer::new();
        laid_out(&mut editor, &mut renderer);
        let card = {
            let document = crate::widget::Widget::accessibility(&editor).expect("semantics");
            document
                .children
                .iter()
                .flat_map(|row| &row.children)
                .find(|node| {
                    node.action_target.as_deref() == Some(&*format!("notes.reference:{GROCERIES}"))
                })
                .cloned()
                .expect("the card is a control a reader opens")
        };
        assert!(
            card.name
                .as_deref()
                .is_some_and(|name| name.contains("Shopping")),
            "named by what it projects: {:?}",
            card.name
        );
        // What it projects is painted under it: a link for the note inside it, and one for the
        // note that projects this one back — a loop ends in a link, never in another card.
        let links: Vec<String> = editor
            .card_link_hits()
            .iter()
            .map(|(_, reference)| reference.clone())
            .collect();
        assert_eq!(
            links,
            vec![PLAN.to_owned(), GROCERIES.to_owned()],
            "{links:?}"
        );

        let plan = editor
            .card_link_hits()
            .iter()
            .find(|(_, reference)| reference == PLAN)
            .map(|(rect, _)| rect.center())
            .expect("the note it names, painted");
        crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::cursor_moved(plan.x, plan.y),
        );
        assert!(crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::mouse_release()
        ));
        assert_eq!(
            queue.take(),
            vec![Fired {
                key: "n".into(),
                event: OPEN_REFERENCE.into(),
                value: Some(Value::Text(PLAN.into())),
            }],
        );

        let card = card.bounds.expect("where the card is").to_rect();
        let at = Point::new(card.x0 + 4.0, card.y0 + 3.0);
        assert!(
            !editor
                .card_link_hits()
                .iter()
                .any(|(rect, _)| rect.contains(at)),
            "a spot on the card that is not one of its links"
        );
        crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::cursor_moved(at.x, at.y),
        );
        assert!(crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::mouse_release()
        ));
        assert_eq!(
            queue.take(),
            vec![Fired {
                key: "n".into(),
                event: OPEN_REFERENCE.into(),
                value: Some(Value::Text(GROCERIES.into())),
            }],
            "the card opens the record it projects"
        );
    }

    #[test]
    fn a_document_being_edited_shows_an_embed_of_a_record_as_it_reads_and_opens_it() {
        let fired = crate::Intents::<Fired>::default();
        let body = crate::LocalState::new(embedding(vec![
            (
                GROCERIES,
                InlineView::Resource {
                    title: "Shopping".into(),
                    icon: None,
                    detail: None,
                },
            ),
            (
                PLAN,
                InlineView::Resource {
                    title: "Plan".into(),
                    icon: None,
                    detail: None,
                },
            ),
        ]));
        let mut editor = BlockEditor::from_props(
            "b",
            BlockEditorProps {
                rich: bound(&fired, body.clone()),
                open_reference: Event::View(Intent::new(&fired, "b", OPEN_REFERENCE)),
                ..BlockEditorProps::default()
            },
        );
        let mut renderer = TextRenderer::new();
        laid_out(&mut editor, &mut renderer);
        let card = {
            let document = crate::widget::Widget::accessibility(&editor).expect("semantics");
            document
                .children
                .iter()
                .flat_map(|row| &row.children)
                .find(|node| {
                    node.action_target.as_deref() == Some(&*format!("notes.reference:{GROCERIES}"))
                })
                .cloned()
                .expect("the card is a control a reader opens")
        };
        assert!(
            card.name
                .as_deref()
                .is_some_and(|name| name.contains("Shopping")),
            "a bound document's card reads the record's title now: {:?}",
            card.name
        );
        assert_eq!(
            editor
                .card_link_hits()
                .iter()
                .map(|(_, reference)| reference.as_str())
                .collect::<Vec<_>>(),
            vec![PLAN, GROCERIES],
            "and shows the start of what it says, its references as links"
        );

        let card = card.bounds.expect("where the card is").to_rect();
        let at = Point::new(card.x0 + 4.0, card.y0 + 3.0);
        crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::cursor_moved(at.x, at.y),
        );
        assert!(crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::mouse_release()
        ));
        assert_eq!(
            fired.take(),
            vec![Fired {
                key: "b".into(),
                event: OPEN_REFERENCE.into(),
                value: Some(Value::Text(GROCERIES.into())),
            }],
        );

        let mut moved = embedding(vec![
            (
                GROCERIES,
                InlineView::Resource {
                    title: "Groceries".into(),
                    icon: None,
                    detail: None,
                },
            ),
            (
                PLAN,
                InlineView::Resource {
                    title: "Plan".into(),
                    icon: None,
                    detail: None,
                },
            ),
        ]);
        if let Some(embed) = moved.embeds.first_mut() {
            embed.preview = Some(vec![tilo_ui_graph::rich::Run {
                text: "Bread and cheese".into(),
                reference: None,
            }]);
        }
        body.set(moved);
        assert!(editor.follow_body(), "the field changed elsewhere");
        laid_out(&mut editor, &mut renderer);
        assert_eq!(
            editor
                .card_link_hits()
                .iter()
                .map(|(_, reference)| reference.as_str())
                .collect::<Vec<_>>(),
            Vec::<&str>::new(),
            "the card shows what it says now, a line with no link in it"
        );
    }

    #[test]
    fn typing_an_embed_or_choosing_it_writes_a_projection_of_the_record() {
        use crate::widget::CursorPlacement;
        use crate::widget::marks::Mark;
        use winit::keyboard::{Key, NamedKey};

        let fired = crate::Intents::<Fired>::default();
        let reference = "tl:so.tilo.notes:tl.so.tilo.notes.d/f:t1";
        let editor_of = || {
            BlockEditor::from_props(
                "b",
                BlockEditorProps {
                    rich: bound(&fired, crate::LocalState::new(RichText::default())),
                    links: vec![Link {
                        reference: reference.into(),
                        title: "Plan".into(),
                    }],
                    ..BlockEditorProps::default()
                },
            )
        };
        let mut editor = editor_of();
        editor.focus_block(0, CursorPlacement::End);
        type_keys(&mut editor, "![[pl");
        assert!(editor.link_popover().is_visible());
        crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::key_press(Key::Named(NamedKey::Enter), None),
        );
        assert_eq!(editor.blocks()[0].text(), "Plan");
        assert!(
            matches!(
                editor.blocks()[0].line.marks().first().map(|span| &span.mark),
                Some(Mark::Embed(named)) if named == reference
            ),
            "the paragraph projects the record, and says which: {:?}",
            editor.blocks()[0].line.marks()
        );

        let mut editor = editor_of();
        editor.focus_block(0, CursorPlacement::End);
        type_keys(&mut editor, " /embed");
        crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::key_press(Key::Named(NamedKey::Enter), None),
        );
        assert!(
            editor.link_popover().is_visible(),
            "the picker offers the records"
        );
        crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::key_press(Key::Named(NamedKey::Enter), None),
        );
        assert_eq!(editor.blocks()[0].text(), "Plan");
        assert!(
            editor.blocks()[0]
                .line
                .marks()
                .iter()
                .any(|span| matches!(&span.mark, Mark::Embed(named) if named == reference)),
            "{:?}",
            editor.blocks()[0].line.marks()
        );
    }

    #[test]
    fn the_views_commands_run_by_their_labels_and_it_hears_the_documents_headings() {
        use tilo_ui_graph::rich::{Attr, Block};

        let fired = crate::Intents::<Fired>::default();
        let rich = RichText {
            blocks: vec![
                Block {
                    kind: "heading".into(),
                    text: "Plan".into(),
                    attrs: [("level".to_owned(), Attr::Int(2))].into(),
                    ..Block::default()
                },
                Block {
                    kind: "paragraph".into(),
                    text: "ship it".into(),
                    ..Block::default()
                },
            ],
            embeds: Vec::new(),
        };
        let body = crate::LocalState::new(rich);
        let props = |value: &crate::LocalState<RichText>| BlockEditorProps {
            rich: bound(&fired, value.clone()),
            commands: vec!["Link to note…".into()],
            command: Event::View(Intent::new(&fired, "b", "command")),
            headings: Event::View(Intent::new(&fired, "b", "headings")),
            ..BlockEditorProps::default()
        };
        let mut editor = BlockEditor::from_props("b", props(&body));
        assert_eq!(
            editor.headings(),
            vec![(2, "Plan".to_owned())],
            "the document's headings, each with its level"
        );
        assert_eq!(
            fired.take(),
            vec![Fired {
                key: "b".into(),
                event: "headings".into(),
                value: Some(Value::List(vec![Value::Text("Plan".into())])),
            }],
            "the view is told them, to draw an outline of the document"
        );

        editor.focus_block(0, crate::widget::CursorPlacement::End);
        type_keys(&mut editor, " /link");
        let _ = fired.take();
        crate::testing::dispatch(
            &mut editor,
            &crate::synthetic_input::key_press(
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
                None,
            ),
        );
        assert!(
            fired.take().iter().any(|fired| fired.event == "command"
                && fired.value == Some(Value::Text("Link to note…".into()))),
            "the command the person chose, by its label"
        );
    }

    #[test]
    fn a_reference_with_no_reading_reads_as_its_run() {
        let mut editor = BlockEditor::from_props(
            "post",
            BlockEditorProps {
                value: Some(text(vec![(TASK, InlineView::Elsewhere)])),
                open_reference: Event::None,
                ..BlockEditorProps::default()
            },
        );
        assert_eq!(
            editor.blocks()[0].text(),
            "Ask @ada about Fix it, not secret."
        );
        assert!(!editor.show_rich_text(&text(vec![(TASK, InlineView::Elsewhere)])));
        assert!(editor.show_rich_text(&text(vec![(TASK, InlineView::Unreadable)])));
        assert_eq!(
            editor.blocks()[0].text(),
            "Ask @ada about Not shared with you, not secret."
        );
    }

    #[test]
    fn any_editor_paints_a_resource_as_a_chip_and_people_as_its_owner_does() {
        let mut editor = BlockEditor::new();
        let owner = MentionColors {
            ink: vello::peniko::Color::from_rgb8(200, 30, 30),
            band: None,
        };
        editor.set_mention_paint(Some(Rc::new(move |_: &str| owner)));
        editor.present_blocks(&crate::widget::rich_doc::blocks_of(&text(Vec::new())));
        let mut renderer = TextRenderer::new();
        laid_out(&mut editor, &mut renderer);
        let paint = editor.mention_paint().cloned().expect("painted");
        assert_eq!(
            paint("u-ada"),
            owner,
            "a person in the owner's colours for them"
        );
        let chip = paint(TASK);
        assert!(chip.band.is_some(), "a resource, in any form, is a chip");
        assert_eq!(
            paint("tilo://open#/me/so.tilo.chat/-/chat.channel:general"),
            chip
        );
        assert_eq!(paint("tl:so.tilo.lang.board:d/f:t1"), chip);
        let hits = editor.reference_hits();
        assert_eq!(hits.len(), 3, "every reference can be pointed at: {hits:?}");
    }

    #[test]
    fn shown_times_wake_the_editor_to_read_again() {
        let mut editor = BlockEditor::new();
        let mut rich = text(Vec::new());
        assert!(editor.show_rich_text(&rich));
        assert_eq!(
            editor.references_wake(),
            None,
            "no time, nothing to wake for"
        );
        let soon = crate::widget::rich_doc::InlineTime {
            at: crate::clock::local_now().with_timezone(&chrono::Utc) + chrono::Duration::hours(1),
            show: crate::widget::rich_doc::TimeShow::Countdown,
        };
        rich.blocks[0].marks.push(RichMark {
            start: 0,
            end: 3,
            name: crate::widget::rich_doc::TIME_MARK.into(),
            value: Attr::Text(soon.mark_value()),
        });
        assert!(editor.show_rich_text(&rich));
        assert!(
            editor.references_wake().is_some(),
            "a countdown asks to be read again as it ticks"
        );
        assert!(
            !editor.blocks()[0].text().starts_with("Ask"),
            "the time reads as it does now: {}",
            editor.blocks()[0].text()
        );
    }

    #[test]
    fn a_shown_body_starts_at_its_edge_as_what_stands_beside_it_does() {
        let props = BlockEditorProps {
            value: Some(text(Vec::new())),
            open_reference: Event::default(),
            ..BlockEditorProps::default()
        };
        let mut editor = BlockEditor::from_props("post", props);
        let mut renderer = TextRenderer::new();
        laid_out(&mut editor, &mut renderer);
        assert_eq!(
            editor.text_inset(&Theme::light()),
            0.0,
            "a message's body lines up with its author's name, quote and chips"
        );
        let editable = BlockEditor::new();
        assert!(
            editable.text_inset(&Theme::light()) > 0.0,
            "a document keeps its inset"
        );
    }

    fn bound<T: Default + 'static>(
        fired: &crate::Intents<Fired>,
        state: crate::LocalState<T>,
    ) -> Bound<T> {
        Bound {
            state,
            edits: Some(Intent::new(fired, "b", "value")),
            name: String::new(),
            path: Vec::new(),
        }
    }

    #[test]
    fn a_value_from_elsewhere_shows_and_the_shown_markdown_follows_it() {
        let fired = crate::Intents::<Fired>::default();
        let value = crate::LocalState::new("First line".to_owned());
        let props = BlockEditorProps {
            markdown: bound(&fired, value.clone()),
            ..BlockEditorProps::default()
        };
        let mut editor = BlockEditor::from_props("b", props);
        assert_eq!(editor.to_markdown().trim(), "First line");
        value.set("Changed by a collaborator".to_owned());
        assert!(editor.follow_body());
        assert_eq!(editor.to_markdown().trim(), "Changed by a collaborator");
    }

    #[test]
    fn a_rich_document_shows_as_its_blocks() {
        let fired = crate::Intents::<Fired>::default();
        let rich = RichText {
            blocks: vec![
                Block {
                    kind: "heading".into(),
                    text: "Plan".into(),
                    attrs: [("level".to_owned(), Attr::Int(2))].into(),
                    ..Block::default()
                },
                Block {
                    kind: "list_item".into(),
                    text: "ship it".into(),
                    ..Block::default()
                },
            ],
            embeds: Vec::new(),
        };
        let value = crate::LocalState::new(rich.clone());
        let props = BlockEditorProps {
            rich: bound(&fired, value.clone()),
            ..BlockEditorProps::default()
        };
        let mut editor = BlockEditor::from_props("b", props);
        let kinds: Vec<_> = editor
            .blocks()
            .iter()
            .map(|block| block.kind.clone())
            .collect();
        assert_eq!(
            kinds,
            vec![
                crate::widget::BlockKind::Heading(2),
                crate::widget::BlockKind::Bullet { indent: 0 }
            ]
        );

        let mut changed = rich;
        changed.blocks[1].text = "shipped".into();
        value.set(changed);
        assert!(editor.follow_body());
        assert_eq!(editor.blocks()[1].content().text, "shipped");
    }
}
