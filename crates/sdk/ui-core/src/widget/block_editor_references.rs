//! References in the block editor's text, and the editor as the described component that shows
//! rich text (`ui.RichText`, tilolang's `rich_text(value)`).
//!
//! A reference is a run marked `mention` (`tilo_reference::embed`). The text a component is
//! given says how each reference reads for the person reading ([`InlineView`]); the editor
//! shows each as that inline view, painted as a chip, and reports activating one by its
//! reference, for its host to open what it refers to under the reader's permissions. One the
//! reader cannot read shows only that something is there.

use std::collections::HashMap;
use std::rc::Rc;

use tilo_ui_graph::rich::{InlineView, RichText};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::BlockEditor;
use crate::component::{Component, Described, Event, PropType, Type, Value};
use crate::theme::Theme;
use crate::widget::marks::{Mark, MentionColors, MentionPaint};

/// The event a component fires when a reference in its text is activated, carrying the
/// reference (the `mention` mark's value): what its host opens when the view gives the event
/// nothing of its own.
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

/// Rich text, shown: its paragraphs with their formatting, and each reference in it as the
/// reader's own inline view of what it refers to (a person with their name, a channel, a task
/// with its number), resolved under their permissions. Activating a reference opens it.
#[derive(Component, Default)]
#[component(name = "RichText", role = "Document")]
pub struct RichTextProps {
    /// The text it shows.
    #[component(required, positional)]
    pub value: RichText,
    /// A reference in the text was activated, carrying the reference. Without one, the
    /// reference is opened.
    #[component(event(value = String))]
    pub open_reference: Event,
}

/// The editor's references: how each reads, where each was painted, and what activating one
/// does.
#[derive(Default)]
pub(super) struct References {
    /// How each reference reads for the person reading, by the `mention` mark's value.
    views: Rc<HashMap<String, InlineView>>,
    /// The theme the chips were last painted for.
    painted_for: Option<u64>,
    /// How the owner paints people's mentions (their colours).
    owner_paint: Option<MentionPaint>,
    /// The rich text last given to show, read again as its times move on.
    shown: Option<RichText>,
    /// When the shown text's times next read differently.
    wake_at: Option<crate::time::Instant>,
    /// Where each reference was painted in the last draw.
    hits: Vec<(Rect, String)>,
    /// What activating a reference does.
    on_open: Option<Box<dyn FnMut(&str)>>,
}

impl BlockEditor {
    /// Show `rich` read-only, each reference as it reads for the person reading.
    pub fn show_rich_text(&mut self, rich: &RichText) -> bool {
        let views: HashMap<String, InlineView> = rich
            .embeds
            .iter()
            .map(|embed| (embed.reference.clone(), embed.view.clone()))
            .collect();
        let changed_views = *self.references.views != views;
        if changed_views {
            self.references.views = Rc::new(views);
            self.references.painted_for = None;
        }
        if self.references.shown.as_ref() != Some(rich) {
            self.references.shown = Some(rich.clone());
        }
        let changed = self.present_shown();
        changed || changed_views
    }

    /// Show the text last given again, as it reads now: its times move on.
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

    /// When the shown text's times next read differently.
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

    /// The reference painted at `point`, when one is.
    #[must_use]
    pub fn reference_at(&self, point: Point) -> Option<&str> {
        self.references
            .hits
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, reference)| reference.as_str())
    }

    /// Paint people's mentions as the owner says (`paint`, their colours), and references to
    /// anything else as chips.
    pub(super) fn set_owner_mention_paint(&mut self, paint: Option<MentionPaint>) {
        self.references.owner_paint = paint;
        self.references.painted_for = None;
        self.dirty = true;
    }

    /// Paint the references in `theme`'s colours when the theme, how they read or the owner's
    /// paint changed since: a resource as a chip, one the reader cannot read as a quiet chip,
    /// a person in the owner's colours for them.
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

/// What identifies a theme's colours, for repainting chips when it changes.
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
    type Props = RichTextProps;

    fn from_props(key: &str, props: RichTextProps) -> Self {
        let mut editor = Self::new()
            .with_wrap_lines(true)
            .with_compact_paragraph_spacing(true)
            .with_block_handles(false)
            .with_flush_text()
            .with_read_only(true);
        let _ = key;
        editor.set_props(props);
        editor
    }

    fn set_props(&mut self, props: RichTextProps) -> bool {
        let RichTextProps {
            value,
            open_reference,
        } = props;
        self.show_rich_text(&value);
        self.on_open_reference(move |reference| {
            open_reference.fire(Some(Value::Text(reference.to_owned())));
        });
        true
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

    /// `Ask @ada about Fix it, not secret.` with a person, a task and a resource the reader
    /// cannot read, read as `views` says.
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
                nodes.get(&node).map_or(Size::ZERO, |widget| {
                    // The pointers are the stable widget leaves registered by build_layout.
                    unsafe { (**widget).measure(known, available, renderer) }
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
        let props = RichTextProps {
            value: text(vec![
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
            ]),
            open_reference: Event::View(Intent::new(&queue, "post", OPEN_REFERENCE)),
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

    #[test]
    fn a_reference_with_no_reading_reads_as_its_run() {
        let mut editor = BlockEditor::from_props(
            "post",
            RichTextProps {
                value: text(vec![(TASK, InlineView::Elsewhere)]),
                open_reference: Event::None,
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
        let props = RichTextProps {
            value: text(Vec::new()),
            open_reference: Event::default(),
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
}
