//! References in the block editor's text, and the editor as the described component (`ui.BlockEditor`):
//! rich text shown read-only (tilolang's `rich_text(value)`), and a document's body edited through the
//! field a view binds it to (its [`body`](super::block_editor_body)).

use std::collections::HashMap;
use std::rc::Rc;

use tilo_ui_graph::rich::{InlineView, RichText, Run};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::{BlockEditor, Pages};
use crate::block_editor::{ImageInserter, PictureIn};
use crate::block_editor_body::BodyBinding;
use crate::slash_menu::SlashHostCommand;
use ui::component::{Bound, Component, Described, Event, Link, Value};
use ui::theme::Theme;
use ui::widget::link_popover::PageEntry;
use ui::widget::marks::{MentionColors, MentionPaint};

pub const OPEN_REFERENCE: &str = "open_reference";

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
    /// The commands only an editor can carry out that its slash menu offers besides the block
    /// kinds: `image` puts a picture the person chooses in the document, `page` makes a page inside
    /// this one, titled with the block's words. A view offers only the ones it answers.
    pub slash: Vec<String>,
    /// The person chose a picture from the slash menu: the view chooses the file and puts it in the
    /// document, as its own Insert image does.
    #[component(event)]
    pub slash_image: Event,
    /// The person chose a page from the slash menu, in the block the words are: the view makes
    /// the page titled with those words, and names it again with `link` once it has.
    #[component(event(value = String))]
    pub slash_page: Event,
    /// The tracked changes the document holds, each time they change: each change's id, and its
    /// kind (0 for one that inserted words, 1 for one that deleted them). What a review panel
    /// lists so the person takes or rejects one change at a time.
    #[component(event(value = Vec<String>, at = Vec<i64>))]
    pub changes: Event,
    /// The document's headings, in the order they stand, each time they change: their words, and
    /// how deep each stands (1 for `#`). A heading reads as its words, never as the `#`s a writer
    /// typed, so a view draws one as it says.
    #[component(event(value = Vec<String>, at = Vec<i64>))]
    pub headings: Event,
    /// The person's selection changed: the block it starts in, by its id, then its start and end
    /// within that block, counted in characters. A caret is the same place twice. What a view
    /// anchors a comment to: the words the person has selected, where they are, not wherever the
    /// document ends.
    #[component(event(value = String, at = Vec<i64>))]
    pub selection: Event,
    /// A press on a comment's marker in the margin: the id of the comment it stands for, which
    /// the view opens in the thread it came from.
    #[component(event(value = String))]
    pub comment: Event,
    /// A picture pasted or dropped into the document, written in at the caret: its name, then the
    /// `tilo-blob://` address its bytes were kept at. What an app that names the pictures its
    /// document shows — an export writing each beside the note — records of one.
    #[component(event(value = Vec<String>))]
    pub image: Event,
    /// The formatting a view asks for, as a toolbar beside the document asks it: `bold`,
    /// `italic`, `underline`, `strike`, `super`, `sub`, `size:<points>`,
    /// `font:<sans|serif|mono>`, `color:#rrggbb`, `highlight:#rrggbb`, `align:<left|center|right|justify>`,
    /// `before:<points>`, `after:<points>`, `indent:<points>`, `hanging:<points>`,
    /// `spacing:<1|1.15|1.5|2>`, `style:<name>`, `changes:accept`, `changes:reject`, `bullets`, `numbers`,
    /// `heading:<level>` or `link:<address>`, then a count
    /// the view changes each time it asks (`bold:1`, `bold:2`), so the same command asked twice
    /// is two commands. Formatting a message is a command of the shared editor's, never a
    /// control of the app's own.
    #[component(state)]
    pub format: Option<String>,
    /// The paper the document is set on: `A4`, `Letter` or `Legal`. A view that asks for pages
    /// has the stack broken into sheets with the margins a printed page keeps, each sheet
    /// carrying `header` in its top margin and `footer` and its number in the bottom one.
    pub page: Option<String>,
    /// Words set in every page's top margin, such as the document's title.
    pub header: Option<String>,
    /// Words set in every page's bottom margin, before the page number.
    pub footer: Option<String>,
    /// The page's top margin in points, where the view sets it; an inch otherwise.
    pub margin_top: Option<f64>,
    /// The page's right margin in points, where the view sets it.
    pub margin_right: Option<f64>,
    /// The page's bottom margin in points, where the view sets it.
    pub margin_bottom: Option<f64>,
    /// The page's left margin in points, where the view sets it.
    pub margin_left: Option<f64>,
    /// A picture to write in at the caret, as a view asks after the person chose a file: the
    /// address of its content, then a count the view changes every time it asks
    /// (`{address}:1`), so the same picture chosen twice is two pictures. A view asks this from
    /// its own Insert image, so the picture lands where the person is writing rather than at the
    /// end of the document.
    #[component(state)]
    pub insert: Option<String>,
    /// A block to make a link to a record, as a view asks once it made the page a block named: a
    /// count the view changes every time it asks (`1`), and the record it becomes a link to
    /// (`link_to`), read as its title. The block is the one the editor last reported a chosen
    /// page for; it keeps its place, reads as the record wherever it is opened, and a rename
    /// shows there.
    #[component(state)]
    pub link: Option<String>,
    /// The record the block named by `link` becomes a link to: the page a view made from it.
    pub link_to: Option<Link>,
    /// The heading the document shows, asked by the view that places an outline beside it: the
    /// place of the heading among the document's headings (`headings` reports them in that
    /// order), then a count the view changes every time it asks (`3:1`), so asking for a heading
    /// already shown shows it again. The editor puts the caret at the heading's start and keeps it
    /// in view. A block named by its place in the document instead (`¶5:2`, the sixth block from
    /// the top, counted from 0) is shown the same way: what the window asks for when a search hit
    /// on a paragraph opens the document.
    #[component(state)]
    pub reveal: Option<String>,
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
    wake_at: Option<ui::time::Instant>,
    /// Where each reference was painted in the last draw.
    hits: Vec<(Rect, String)>,
    pub(super) on_open: Option<Box<dyn FnMut(&str)>>,
    headings: Event,
    /// The selection last reported to the view: it is told again only when it changes.
    selection: Event,
    reported_selection: Option<(String, Vec<i64>)>,
    /// The comment markers in the margin, where each was painted in the last draw.
    pub(super) comment_markers: Vec<(Rect, String)>,
    /// The view's answer to a press on a comment's marker.
    comment: Event,
    /// The headings last reported to it, their words and levels: it is told again only when they
    /// change.
    reported: Option<(Vec<String>, Vec<i64>)>,
    /// The tracked changes last reported to the view: it is told again only when they change.
    changes: Event,
    reported_changes: Option<(Vec<String>, Vec<i64>)>,
    links_open: Event,
    links_query: Event,
    /// The view's answers to the slash menu's picture and page commands: which it waits for, and
    /// which it answers at all.
    slash_image: Event,
    slash_page: Event,
    /// What the picker last told the view, which it hears again only when that changes: whether it was
    /// open, and the text it held.
    picker: Option<(bool, String)>,
}

impl BlockEditor {
    /// Show `rich` read-only, each reference as it reads for the person reading.
    pub fn show_rich_text(&mut self, rich: &RichText) -> bool {
        self.note_edit_anywhere();
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
        let reading = crate::rich_doc::reading_here();
        let changed = self.present_blocks(&crate::rich_doc::blocks_at(&rich, &reading));
        self.references.wake_at =
            crate::rich_doc::changes_in(&rich, reading.now).map(|soon| ui::time::now() + soon);
        self.references.shown = Some(rich);
        changed
    }

    pub(super) fn references_wake(&self) -> Option<ui::time::Instant> {
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
            Some(InlineView::Unreadable) => crate::rich_doc::unreadable_phrase(),
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
            .is_some_and(|wake| wake <= ui::time::now())
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
        let contrast = ui::tokens::contrast::BODY_TEXT_MIN_CONTRAST;
        let band = ui::tokens::colors::mix8(accent, ground, 0.86);
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
                let Some(reference) = span.mark.reference() else {
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

/// The editor's picture inserter: keep the picture in the window's content store and answer
/// where its bytes are, so the editor writes it in at the caret. What the view hears of it — by
/// its name and its address — the editor fires itself, as one part of the same gesture as the
/// edit (`BlockEditor::set_image_event`).
fn picture_inserter() -> ImageInserter {
    Box::new(|picture: PictureIn<'_>| match picture {
        PictureIn::Bytes { name, bytes, .. } => ui::pictures::keep_picture(name, bytes),
        PictureIn::Path(path) => ui::pictures::dropped_picture(path),
    })
}

/// The pages a view asks for, when it names a paper: its header and footer, and the margins it
/// sets or an inch each.
fn pages_of(
    page: Option<&str>,
    header: Option<String>,
    footer: Option<String>,
    margins: [Option<f64>; 4],
) -> Option<Pages> {
    let pages = Pages::of(
        page?,
        header.unwrap_or_default(),
        footer.unwrap_or_default(),
    )?;
    let points =
        |value: Option<f64>| value.map_or(super::DEFAULT_PAGE_MARGIN, |points| points as f32);
    Some(pages.with_margins(
        points(margins[0]),
        points(margins[1]),
        points(margins[2]),
        points(margins[3]),
    ))
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
            slash,
            slash_image,
            slash_page,
            changes,
            headings,
            selection,
            comment,
            image,
            // A view opened with a formatting ask is just opened: the ask is answered on the
            // change that follows, never on the editor a note is first shown in.
            format: _,
            page,
            header,
            footer,
            margin_top,
            margin_right,
            margin_bottom,
            margin_left,
            insert: _,
            link: _,
            link_to: _,
            reveal,
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
        editor.offer(Offered {
            links,
            links_open,
            links_query,
            commands,
            command,
            changes,
            headings,
            selection,
            comment,
            open_reference,
        });
        editor.offer_slash(&slash, slash_image, slash_page);
        editor.set_image_store(Some(ui::pictures::Pictures::window().store().clone()));
        if editing {
            editor.set_image_inserter(Some(picture_inserter()));
            editor.set_image_event(image);
            let (body, placeholder) = BodyBinding::of(rich, markdown, placeholder);
            editor.set_empty_hint(&placeholder);
            editor.show_body(&body);
            editor.body = Some(body);
        } else {
            editor.show_rich_text(&value.unwrap_or_default());
        }
        editor.set_pages(pages_of(
            page.as_deref(),
            header.clone(),
            footer.clone(),
            [margin_top, margin_right, margin_bottom, margin_left],
        ));
        editor.publish_headings();
        editor.publish_changes();
        // A view opened with a heading asked for, of the note shown before, is just opened; a
        // block asked for by its place is shown.
        editor.reveal_opened(reveal);
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
            slash,
            slash_image,
            slash_page,
            changes,
            headings,
            selection,
            comment,
            image,
            format,
            page,
            header,
            footer,
            margin_top,
            margin_right,
            margin_bottom,
            margin_left,
            insert,
            link,
            link_to,
            reveal,
        } = props;
        let editing = BodyBinding::bound(&rich, &markdown);
        if editing != self.body.is_some() {
            return false;
        }
        let showing = self.body.is_none();
        self.offer(Offered {
            links,
            links_open,
            links_query,
            commands,
            command,
            changes,
            headings,
            selection,
            comment,
            open_reference,
        });
        self.offer_slash(&slash, slash_image, slash_page);
        self.set_find_replace_enabled(!showing);
        if !showing {
            // The view that bound the document hears of each picture that goes into it.
            self.set_image_inserter(Some(picture_inserter()));
            self.set_image_event(image);
        }
        let rebound = if showing {
            self.show_rich_text(&value.unwrap_or_default());
            true
        } else {
            self.rebind_body(rich, markdown, &placeholder.unwrap_or_default())
        };
        // A view that asks for a heading again changes its count, so the same heading is shown
        // again rather than read as the ask it already answered.
        self.reveal_asked(reveal);
        self.set_pages(pages_of(
            page.as_deref(),
            header.clone(),
            footer.clone(),
            [margin_top, margin_right, margin_bottom, margin_left],
        ));
        // A formatting ask is one command: the same ask twice is the command it already ran.
        let formatted = self.format_asked(format);
        let inserted = self.insert_asked(insert);
        let linked = self.link_asked(link, link_to);
        self.publish_headings();
        self.publish_changes();
        rebound || formatted || inserted || linked
    }
}

/// The props a view hands the editor that the editor answers: the pages it may link to, its
/// commands, and the events it tells of each change.
struct Offered {
    links: Vec<Link>,
    links_open: Event,
    links_query: Event,
    commands: Vec<String>,
    command: Event,
    changes: Event,
    headings: Event,
    selection: Event,
    comment: Event,
    open_reference: Event,
}

impl BlockEditor {
    fn offer(&mut self, offered: Offered) {
        let Offered {
            links,
            links_open,
            links_query,
            commands,
            command,
            changes,
            headings,
            selection,
            comment,
            open_reference,
        } = offered;
        self.references.changes = changes;
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
        self.set_view_commands(&commands);
        self.on_command(move |label| {
            command.fire(Some(Value::Text(label.to_owned())));
        });
        self.on_open_reference(move |reference| {
            open_reference.fire(Some(Value::Text(reference.to_owned())));
        });
        self.references.headings = headings;
        self.references.selection = selection;
        self.references.comment = comment;
    }

    /// The commands the editor itself carries out that the view offers it: a picture and a page,
    /// each asked of the view, which is the one that can choose a file or make a page.
    fn offer_slash(&mut self, slash: &[String], slash_image: Event, slash_page: Event) {
        let host: Vec<SlashHostCommand> = slash
            .iter()
            .filter_map(|word| match word.as_str() {
                "image" => Some(SlashHostCommand::Image),
                "page" => Some(SlashHostCommand::Page),
                _ => None,
            })
            .collect();
        self.set_slash_host(&host);
        self.references.slash_image = slash_image;
        self.references.slash_page = slash_page;
    }

    /// Report the command only the view can carry out that a person chose in the slash menu: a
    /// picture, or a page inside this one. The view's own commands go as `command`, from the
    /// editor's own callback.
    pub(super) fn publish_slash_request(&mut self) {
        if self.references.slash_image.is_none() && self.references.slash_page.is_none() {
            return;
        }
        let Some(request) = self.take_slash_request() else {
            return;
        };
        match request.command {
            SlashHostCommand::Image => self.references.slash_image.fire(None),
            // The page a block makes is titled with the words that stood in it: the view names
            // the record it made again with `link`, and the editor writes the block it reported
            // as the reference to it.
            SlashHostCommand::Page => self
                .references
                .slash_page
                .fire(Some(Value::Text(request.text))),
            SlashHostCommand::Named(_) => {}
        }
    }

    /// Answer a view's ask to make the block a page was chosen in a link to the record the view
    /// made: the ask is a count the view changes every time it asks (`1`), and `to` is the
    /// record the block reads as now. Each ask is answered once.
    pub fn link_asked(&mut self, asked: Option<String>, to: Option<Link>) -> bool {
        if asked == self.link_ask {
            return false;
        }
        // The page is made by the view in the same press that named the block: an ask that
        // arrives before the record does is answered when it does, never dropped.
        let Some(to) = to else {
            return false;
        };
        let Some(asked) = asked else {
            return false;
        };
        let Some(block) = self.page_block.clone() else {
            return false;
        };
        let linked = self.link_block_to_page(&block, &PageEntry::link(&to.reference, &to.title));
        if linked {
            self.link_ask = Some(asked);
        }
        linked
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

    /// Tell the view where the person's selection is, when it moves: the block it starts in,
    /// then its start and end in that block, counted in characters. A caret reports the same
    /// place twice, so a view's comment lands where the person is writing.
    pub(super) fn publish_selection(&mut self) {
        if self.references.selection.is_none() {
            return;
        }
        let Some(reported) = self.selection_chars() else {
            return;
        };
        if self.references.reported_selection.as_ref() == Some(&reported) {
            return;
        }
        self.references.selection.fire(Some(Value::List(vec![
            Value::Text(reported.0.clone()),
            Value::List(reported.1.iter().map(|at| Value::Int(*at)).collect()),
        ])));
        self.references.reported_selection = Some(reported);
    }

    /// The block the person's selection starts in, and where it starts and ends in that block:
    /// the caret's own place twice while nothing is selected.
    fn selection_chars(&self) -> Option<(String, Vec<i64>)> {
        let (index, start, end) = if let Some(selection) = self.document_selection {
            let (start, end) = selection.ordered();
            let text = self.blocks.get(start.block)?.text();
            let end = if end.block == start.block {
                end.byte.min(text.len())
            } else {
                text.len()
            };
            (start.block, start.byte.min(end), end)
        } else {
            let index = self.focused?;
            let byte = self.blocks.get(index)?.line.cursor_pos();
            (index, byte, byte)
        };
        let block = self.blocks.get(index)?;
        let text = block.text();
        let chars = |byte: usize| {
            i64::try_from(crate::rich_doc::char_of_byte(text, byte)).unwrap_or(i64::MAX)
        };
        Some((block.id.clone(), vec![chars(start), chars(end)]))
    }

    /// The comment whose marker stands at `point` in the margin, when one does.
    #[must_use]
    pub fn comment_at(&self, point: Point) -> Option<&str> {
        self.references
            .comment_markers
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, id)| id.as_str())
    }

    /// Report the comment whose marker the pointer let go of, as a toolbar reports a press on
    /// its own button: what a view opens the thread with.
    pub(super) fn activate_comment(&self, event: &WindowEvent) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        let Some(id) = self.comment_at(self.last_mouse_pos).map(str::to_owned) else {
            return false;
        };
        self.references.comment.fire(Some(Value::Text(id)));
        true
    }

    pub(super) fn publish_headings(&mut self) {
        if self.references.headings.is_none() {
            return;
        }
        let headings = self.headings();
        let texts: Vec<String> = headings.iter().map(|(_, text)| text.clone()).collect();
        let levels: Vec<i64> = headings
            .iter()
            .map(|(level, _)| i64::from(*level))
            .collect();
        let reported = (texts, levels);
        if self.references.reported.as_ref() == Some(&reported) {
            return;
        }
        self.references.headings.fire(Some(Value::List(vec![
            Value::List(reported.0.iter().cloned().map(Value::Text).collect()),
            Value::List(reported.1.iter().map(|level| Value::Int(*level)).collect()),
        ])));
        self.references.reported = Some(reported);
    }

    /// Tell the view which tracked changes the document holds, when they change: each change's
    /// id and its kind, an insertion before a deletion, so a review panel lists them and takes
    /// or rejects one at a time.
    pub(super) fn publish_changes(&mut self) {
        if self.references.changes.is_none() {
            return;
        }
        let mut ids: Vec<String> = Vec::new();
        let mut kinds: Vec<i64> = Vec::new();
        for block in &self.blocks {
            for span in block.line.marks() {
                let Some(id) = span.mark.change() else {
                    continue;
                };
                if ids.iter().any(|seen| seen == id) {
                    continue;
                }
                ids.push(id.to_owned());
                kinds.push(i64::from(matches!(
                    span.mark,
                    ui::widget::marks::Mark::Deletion(_)
                )));
            }
        }
        let reported = (ids, kinds);
        if self.references.reported_changes.as_ref() == Some(&reported) {
            return;
        }
        self.references.changes.fire(Some(Value::List(vec![
            Value::List(reported.0.iter().cloned().map(Value::Text).collect()),
            Value::List(reported.1.iter().map(|kind| Value::Int(*kind)).collect()),
        ])));
        self.references.reported_changes = Some(reported);
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
    use ui::component::{Fired, Intent};
    use ui::presence::PresenceCtx;
    use ui::text_render::TextRenderer;
    use ui::widget::Widget as _;

    const TASK: &str = r#"{"app":"so.tilo.lang.board","id":"tl:so.tilo.lang.board:d/f:t1","kind":"tl.so.tilo.lang.board.Task"}"#;
    const SECRET: &str =
        r#"{"app":"so.tilo.lang.board","id":"tl:so.tilo.lang.board:hidden","kind":""}"#;
    /// How a mention of Ada is stored ([`tilo_reference::Embed::mark_value`]).
    const ADA: &str = r#"{"person":"u-ada"}"#;

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
                    mention(4, 8, ADA),
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
        editor.draw(&mut tilo_ui::DrawCtx::new(
            &mut Scene::new(),
            Affine::IDENTITY,
            renderer,
            &theme,
            PresenceCtx::empty().draw_env(),
        ));
    }

    fn type_keys(editor: &mut BlockEditor, text: &str) {
        for ch in text.chars() {
            let typed = ch.to_string();
            let event = if ch == ' ' {
                ui::synthetic_input::key_press(
                    winit::keyboard::Key::Named(winit::keyboard::NamedKey::Space),
                    Some(" "),
                )
            } else {
                ui::synthetic_input::char_key(&typed)
            };
            ui::testing::dispatch(editor, &event);
        }
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
                        name: crate::rich_doc::EMBED_MARK.into(),
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

    const GROCERIES: &str = "tl:so.tilo.notes:tl.so.tilo.notes.d/f:g1";
    const PLAN: &str = "tl:so.tilo.notes:tl.so.tilo.notes.d/f:p1";

    tilo_test_env::scenario! {
        /// Embeds of records.
        /// Each phase was a test of its own and builds its own fixture.
        fn an_embed_shows_a_record_and_writes_its_projection;

        phase an_embed_of_a_record_shows_its_start_and_opens_what_it_names;
        {
            let queue = ui::Intents::<Fired>::default();
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
                let document = ui::widget::Widget::accessibility(&editor).expect("semantics");
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
            ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::cursor_moved(plan.x, plan.y),
            );
            assert!(ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::mouse_release()
            ));
            assert_eq!(
                queue.take(),
                vec![Fired {
                    key: "n".into(),
                    event: OPEN_REFERENCE.into(),
                    value: Some(Value::Text(PLAN.into())),
                gesture: None,
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
            ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::cursor_moved(at.x, at.y),
            );
            assert!(ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::mouse_release()
            ));
            assert_eq!(
                queue.take(),
                vec![Fired {
                    key: "n".into(),
                    event: OPEN_REFERENCE.into(),
                    value: Some(Value::Text(GROCERIES.into())),
                gesture: None,
                }],
                "the card opens the record it projects"
            );
        }

        phase a_document_being_edited_shows_an_embed_of_a_record_as_it_reads_and_opens_it;
        {
            let fired = ui::Intents::<Fired>::default();
            let body = ui::LocalState::new(embedding(vec![
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
                let document = ui::widget::Widget::accessibility(&editor).expect("semantics");
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
            ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::cursor_moved(at.x, at.y),
            );
            assert!(ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::mouse_release()
            ));
            assert_eq!(
                fired.take(),
                vec![Fired {
                    key: "b".into(),
                    event: OPEN_REFERENCE.into(),
                    value: Some(Value::Text(GROCERIES.into())),
                gesture: None,
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

        phase typing_an_embed_or_choosing_it_writes_a_projection_of_the_record;
        {
            use crate::CursorPlacement;
            use ui::widget::marks::Mark;
            use winit::keyboard::{Key, NamedKey};

            let fired = ui::Intents::<Fired>::default();
            let reference = "tl:so.tilo.notes:tl.so.tilo.notes.d/f:t1";
            let editor_of = || {
                BlockEditor::from_props(
                    "b",
                    BlockEditorProps {
                        rich: bound(&fired, ui::LocalState::new(RichText::default())),
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
            ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::key_press(Key::Named(NamedKey::Enter), None),
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
            ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::key_press(Key::Named(NamedKey::Enter), None),
            );
            assert!(
                editor.link_popover().is_visible(),
                "the picker offers the records"
            );
            ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::key_press(Key::Named(NamedKey::Enter), None),
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
    }

    tilo_test_env::scenario! {
        /// References in a document: how they read, how they are picked and what the view hears.
        /// Each phase was a test of its own and builds its own fixture.
        fn references_read_as_inline_views_and_are_picked;

        phase each_reference_reads_as_the_readers_inline_view_and_a_click_opens_it;
        {
            let queue = ui::Intents::<Fired>::default();
            let props = BlockEditorProps {
                value: Some(text(vec![
                    (
                        ADA,
                        InlineView::Person {
                            identity: tilo_actions::MemberId::person("u-ada"),
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
            assert!(hits.iter().any(|(_, reference)| reference == ADA));
            assert!(hits.iter().any(|(_, reference)| reference == SECRET));
            assert_eq!(
                editor.cursor_shape(task.center()),
                Some(winit::window::CursorIcon::Pointer)
            );

            ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::cursor_moved(task.center().x, task.center().y),
            );
            assert!(ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::mouse_release()
            ));
            assert_eq!(
                queue.take(),
                vec![Fired {
                    key: "post".into(),
                    event: OPEN_REFERENCE.into(),
                    value: Some(Value::Text(TASK.into())),
                gesture: None,
                }],
                "a click opens the reference, by the reference"
            );
            ui::testing::dispatch(&mut editor, &ui::synthetic_input::cursor_moved(1.0, 1.0));
            ui::testing::dispatch(&mut editor, &ui::synthetic_input::mouse_release());
            assert!(
                queue.take().is_empty(),
                "a click beside a reference opens nothing"
            );
        }

        phase the_records_a_view_links_are_offered_and_chosen_as_references_to_them;
        {
            use crate::CursorPlacement;
            use ui::widget::marks::Mark;

            let fired = ui::Intents::<Fired>::default();
            let body = ui::LocalState::new(RichText::default());
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
            ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::key_press(
                    winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
                    None,
                ),
            );
            assert_eq!(editor.blocks()[0].text(), "Plan");
            assert!(
                matches!(
                    editor.blocks()[0].line.marks().first().map(|span| &span.mark),
                    Some(Mark::Reference(reference)) if reference == "tl:so.tilo.notes:tl.so.tilo.notes.d/f:t1"
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

        phase the_picker_tells_its_view_when_it_opens_and_what_is_typed_in_it;
        {
            use crate::CursorPlacement;

            let fired = ui::Intents::<Fired>::default();
            let asked = ui::Intents::<Fired>::default();
            let body = ui::LocalState::new(RichText::default());
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
            ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::key_press(
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
                gesture: None,
                }],
                "the picker closing reaches the view"
            );
        }

        phase the_views_commands_run_by_their_labels_and_it_hears_the_documents_headings;
        {
            use tilo_ui_graph::rich::{Attr, Block};

            let fired = ui::Intents::<Fired>::default();
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
            let body = ui::LocalState::new(rich);
            let props = |value: &ui::LocalState<RichText>| BlockEditorProps {
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
                    value: Some(Value::List(vec![
                        Value::List(vec![Value::Text("Plan".into())]),
                        Value::List(vec![Value::Int(2)]),
                    ])),
                gesture: None,
                }],
                "the view is told the headings' words and the level each stands at, to draw an \
             outline of the document"
            );

            editor.focus_block(0, crate::CursorPlacement::End);
            type_keys(&mut editor, " /link");
            let _ = fired.take();
            ui::testing::dispatch(
                &mut editor,
                &ui::synthetic_input::key_press(
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

        phase a_heading_that_only_stands_deeper_is_told_again;
        {
            use tilo_ui_graph::rich::{Attr, Block};

            let fired = ui::Intents::<Fired>::default();
            let heading = |level: i64| RichText {
                blocks: vec![Block {
                    kind: "heading".into(),
                    text: "Plan".into(),
                    attrs: [("level".to_owned(), Attr::Int(level))].into(),
                    ..Block::default()
                }],
                embeds: Vec::new(),
            };
            let props = |rich: RichText| BlockEditorProps {
                value: Some(rich),
                headings: Event::View(Intent::new(&fired, "b", "headings")),
                ..BlockEditorProps::default()
            };
            let mut editor = BlockEditor::from_props("b", props(heading(2)));
            let _ = fired.take();
            editor.set_props(props(heading(3)));
            assert_eq!(
                fired.take(),
                vec![Fired {
                    key: "b".into(),
                    event: "headings".into(),
                    value: Some(Value::List(vec![
                        Value::List(vec![Value::Text("Plan".into())]),
                        Value::List(vec![Value::Int(3)]),
                    ])),
                gesture: None,
                }],
                "a heading set deeper reads deeper, though its words stay the same"
            );
        }

        phase a_reference_with_no_reading_reads_as_its_run;
        {
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

        phase any_editor_paints_a_resource_as_a_chip_and_people_as_its_owner_does;
        {
            let mut editor = BlockEditor::new();
            let owner = MentionColors {
                ink: vello::peniko::Color::from_rgb8(200, 30, 30),
                band: None,
            };
            editor.set_mention_paint(Some(Rc::new(move |_: &str| owner)));
            editor.present_blocks(&crate::rich_doc::blocks_of(&text(Vec::new())));
            let mut renderer = TextRenderer::new();
            laid_out(&mut editor, &mut renderer);
            let paint = editor.mention_paint().cloned().expect("painted");
            assert_eq!(
                paint(ADA),
                owner,
                "a person in the owner's colours for them"
            );
            assert!(paint(TASK).band.is_some(), "a resource is a chip");
            let hits = editor.reference_hits();
            assert_eq!(hits.len(), 3, "every reference can be pointed at: {hits:?}");
        }
    }

    fn bound<T: Default + 'static>(
        fired: &ui::Intents<Fired>,
        state: ui::LocalState<T>,
    ) -> Bound<T> {
        Bound {
            state,
            edits: Some(Intent::new(fired, "b", "value")),
            name: String::new(),
            path: Vec::new(),
            identity: None,
        }
    }

    tilo_test_env::scenario! {
        /// Shown times, bodies, values and documents.
        /// Each phase was a test of its own and builds its own fixture.
        fn shown_values_follow_what_they_read;

        phase shown_times_wake_the_editor_to_read_again;
        {
            let mut editor = BlockEditor::new();
            let mut rich = text(Vec::new());
            assert!(editor.show_rich_text(&rich));
            assert_eq!(
                editor.references_wake(),
                None,
                "no time, nothing to wake for"
            );
            let soon = crate::rich_doc::InlineTime {
                at: ui::clock::local_now().timestamp() + jiff::SignedDuration::from_hours(1),
                show: crate::rich_doc::TimeShow::Countdown,
            };
            rich.blocks[0].marks.push(RichMark {
                start: 0,
                end: 3,
                name: crate::rich_doc::TIME_MARK.into(),
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

        phase a_shown_body_starts_at_its_edge_as_what_stands_beside_it_does;
        {
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

        phase a_value_from_elsewhere_shows_and_the_shown_markdown_follows_it;
        {
            let fired = ui::Intents::<Fired>::default();
            let value = ui::LocalState::new("First line".to_owned());
            let props = BlockEditorProps {
                markdown: bound(&fired, value.clone()),
                ..BlockEditorProps::default()
            };
            let mut editor = BlockEditor::from_props("b", props);
            assert_eq!(editor.to_markdown().trim(), "First line");
            value.set("Changed by a member".to_owned());
            assert!(editor.follow_body());
            assert_eq!(editor.to_markdown().trim(), "Changed by a member");
        }

        phase a_rich_document_shows_as_its_blocks;
        {
            let fired = ui::Intents::<Fired>::default();
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
            let value = ui::LocalState::new(rich.clone());
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
                    crate::BlockKind::Heading(2),
                    crate::BlockKind::Bullet { indent: 0 }
                ]
            );

            let mut changed = rich;
            changed.blocks[1].text = "shipped".into();
            value.set(changed);
            assert!(editor.follow_body());
            assert_eq!(editor.blocks()[1].content().text, "shipped");
        }
    }

    /// Markdown shown read-only keeps its lists: a reply's bulleted items are bullets, their
    /// text set in the list's column after the marker, not paragraphs flush with the line before.
    #[test]
    fn a_shown_markdown_list_is_a_bulleted_list() {
        let reading =
            BlockEditor::reading_markdown("Overdue:\n- Book a haircut\n- Call the landlord");
        let blocks: Vec<crate::BlockContent> =
            reading.blocks().iter().map(crate::Block::content).collect();
        let rich = crate::rich_doc::rich_of_blocks(&blocks, &RichText::default());
        let mut editor = BlockEditor::new();
        assert!(editor.show_rich_text(&rich));
        let kinds: Vec<crate::BlockKind> = editor.blocks().iter().map(|b| b.kind.clone()).collect();
        assert_eq!(
            kinds,
            [
                crate::BlockKind::Paragraph,
                crate::BlockKind::Bullet { indent: 0 },
                crate::BlockKind::Bullet { indent: 0 }
            ]
        );
        let mut renderer = TextRenderer::new();
        laid_out(&mut editor, &mut renderer);
        let x = |index: usize| {
            editor.blocks()[index]
                .line
                .caret_rect(&Theme::light(), 0)
                .x0
        };
        assert!(
            x(1) > x(0) + 8.0,
            "an item's text stands after its bullet: {} against {}",
            x(1),
            x(0)
        );
    }
}
