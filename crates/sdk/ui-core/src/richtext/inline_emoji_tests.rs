use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use vello::peniko::Color;

use super::*;
use crate::richtext::TextAlign;
use crate::text_render::{TextRenderer, TextStyle};
use crate::workspace_emoji::{CustomEmoji, EmojiSource};

/// A window's workspace emoji, as a test sets them.
pub(crate) struct Fixed {
    pub(crate) emoji: RefCell<Rc<WorkspaceEmoji>>,
    pub(crate) revision: Cell<u64>,
}

impl Fixed {
    pub(crate) fn new(emoji: WorkspaceEmoji) -> Self {
        Self {
            emoji: RefCell::new(Rc::new(emoji)),
            revision: Cell::new(1),
        }
    }

    /// The workspace's emoji are now `emoji`.
    pub(crate) fn set(&self, emoji: WorkspaceEmoji) {
        *self.emoji.borrow_mut() = Rc::new(emoji);
        self.revision.set(self.revision.get() + 1);
    }
}

impl EmojiSource for Fixed {
    fn emoji(&self) -> Rc<WorkspaceEmoji> {
        Rc::clone(&self.emoji.borrow())
    }
    fn revision(&self) -> u64 {
        self.revision.get()
    }
    fn can_manage(&self) -> bool {
        false
    }
    fn add(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err("fixed".into())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        Err("fixed".into())
    }
    fn choose_and_add(&self) {}
}

fn picture(rgb: [u8; 3]) -> Arc<[u8]> {
    let side = 8;
    let rgba: Vec<u8> = (0..side * side)
        .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
        .collect();
    Arc::from(crate::png::encode_rgba8(side, side, &rgba).unwrap())
}

pub(crate) fn workspace() -> WorkspaceEmoji {
    WorkspaceEmoji::new(vec![
        CustomEmoji {
            name: "tilo".to_owned(),
            image: picture([220, 40, 90]),
        },
        CustomEmoji {
            name: "party_parrot".to_owned(),
            image: picture([40, 200, 90]),
        },
        CustomEmoji {
            name: "broken".to_owned(),
            image: Arc::from(&b"not a picture"[..]),
        },
    ])
}

fn written<'t>(text: &'t str, ranges: &[Range<usize>]) -> Vec<&'t str> {
    ranges.iter().map(|range| &text[range.clone()]).collect()
}

#[test]
fn only_names_the_workspace_has_a_picture_for_are_found() {
    let emoji = workspace();
    let text = "hi :tilo: and :party_parrot:, not :missing: or :broken: or :tilo";
    assert_eq!(
        written(text, &emoji_ranges(text, &emoji)),
        [":tilo:", ":party_parrot:"]
    );
    // A colon closing one name never opens the next; one that ended a
    // failed attempt may.
    let text = ":tilo::tilo: a:tilo:b ::tilo: :x:tilo: 10:30:45";
    assert_eq!(
        written(text, &emoji_ranges(text, &emoji)),
        [":tilo:", ":tilo:", ":tilo:", ":tilo:", ":tilo:"]
    );
    assert!(emoji_ranges(":ti lo: :TILO: :tilo-x:", &emoji).is_empty());
    assert!(emoji_ranges(":tilo:", &WorkspaceEmoji::default()).is_empty());
}

#[test]
fn spans_split_around_each_emoji_and_keep_their_bytes_and_style() {
    let emoji = workspace();
    let spans = vec![
        RichSpan::new("go :tilo: now").bold(true),
        RichSpan::new(" `:tilo:`"),
        RichSpan {
            family: Some(FontFamily::Mono),
            ..RichSpan::new(":tilo:")
        },
    ];
    let source: String = spans.iter().map(|span| span.text.as_str()).collect();
    let split = split_spans(spans, &emoji);
    let text: String = split.iter().map(|span| span.text.as_str()).collect();
    assert_eq!(text, source, "the text's bytes are kept");
    let pictures: Vec<(&str, bool)> = split
        .iter()
        .filter(|span| span.workspace_emoji)
        .map(|span| (span.text.as_str(), span.bold))
        .collect();
    assert_eq!(
        pictures,
        [(":tilo:", true), (":tilo:", false)],
        "code keeps its `:name:`"
    );
    assert!(
        split[0].bold && split[2].bold,
        "the run around it keeps its style"
    );
    let none = vec![RichSpan::new("no :missing: here")];
    assert_eq!(split_spans(none.clone(), &emoji), none);
}

fn shaped(spans: &[RichSpan], width: Option<f32>) -> RichTextLayout {
    let mut fs = crate::font_catalog::fixture_font_system();
    RichTextLayout::shape_styled(
        &mut fs,
        spans,
        &TextStyle::new(16.0).with_line_height(1.45),
        width,
        TextAlign::Left,
        Vec::new(),
        Vec::new(),
    )
}

#[test]
fn an_emoji_takes_one_square_on_its_row_without_growing_it() {
    let emoji = workspace();
    let text = "ok :party_parrot: go";
    let plain = shaped(&[RichSpan::new(text)], None);
    let layout = shaped(&split_spans(vec![RichSpan::new(text)], &emoji), None);
    let at = text.find(':').unwrap();
    let end = at + ":party_parrot:".len();

    let rects = layout.selection_rects(at, end);
    assert_eq!(rects.len(), 1);
    let (x0, y0, x1, y1) = rects[0];
    let advance = ADVANCE_EM * 16.0;
    assert!(
        (x1 - x0 - advance).abs() < 0.25,
        "the run is one emoji wide: {} vs {advance}",
        x1 - x0
    );
    let natural = plain.selection_rects(at, end)[0];
    assert!(
        natural.2 - natural.0 > advance * 2.0,
        "its letters were wider"
    );
    assert!(
        (layout.height() - plain.height()).abs() < 0.01,
        "the row is as tall as the text's: {} vs {}",
        layout.height(),
        plain.height()
    );
    // The text after it moves up by what the letters took beyond the square.
    let shift = (natural.2 - natural.0) - (x1 - x0);
    let after = layout.caret_geometry(text.len()).0;
    assert!((plain.caret_geometry(text.len()).0 - shift - after).abs() < 0.25);

    let squares = layout.inline_emoji_squares();
    assert_eq!(squares.len(), 1);
    let (name, square) = squares[0];
    assert_eq!(name, "party_parrot");
    let side = f64::from((SIDE_EM * 16.0).min(y1 - y0));
    assert!((square.width() - side).abs() < 1e-3 && (square.height() - side).abs() < 1e-3);
    assert!((square.center().x - f64::from(x0 + x1) / 2.0).abs() < 1e-3);
    assert!((square.center().y - f64::from(y0 + y1) / 2.0).abs() < 1e-3);
    assert!(f64::from(y0) <= square.y0 && square.y1 <= f64::from(y1));

    // The bytes still answer the pointer: the picture's middle is inside it.
    let hit = layout.hit(square.center().x as f32, square.center().y as f32);
    assert!((at..=end).contains(&hit), "{hit} in {at}..{end}");
}

#[test]
fn a_wrapped_emoji_stays_whole_on_one_row() {
    let emoji = workspace();
    let text = "word word word word :tilo: word word word :tilo: word word";
    let layout = shaped(&split_spans(vec![RichSpan::new(text)], &emoji), Some(120.0));
    assert!(layout.buffer.layout_runs().count() > 2, "the text wraps");
    for range in emoji_ranges(text, &emoji) {
        assert_eq!(
            layout.selection_rects(range.start, range.end).len(),
            1,
            "an emoji is never split over rows"
        );
    }
    assert_eq!(layout.inline_emoji_squares().len(), 2);
}

#[test]
fn a_missing_emoji_stays_text() {
    let text = "ok :missing: go";
    let spans = split_spans(vec![RichSpan::new(text)], &workspace());
    assert!(spans.iter().all(|span| !span.workspace_emoji));
    let layout = shaped(&spans, None);
    assert!(layout.inline_emoji_squares().is_empty());
}

#[test]
fn the_renderer_paints_the_picture_in_place_of_the_name() {
    let draw = |text: &str| {
        let mut renderer =
            TextRenderer::with_font_system(crate::font_catalog::fixture_font_system());
        let spans = split_spans_in_window(vec![RichSpan::new(text)]);
        let layout = RichTextLayout::shape_styled(
            &mut renderer.font_system,
            &spans,
            &TextStyle::new(16.0),
            None,
            TextAlign::Left,
            Vec::new(),
            Vec::new(),
        );
        let mut scene = Scene::new();
        renderer.draw_rich(&mut scene, &layout, Affine::IDENTITY, Color::WHITE);
        let encoding = scene.encoding();
        (
            encoding.resources.glyphs.len(),
            encoding.draw_tags.contains(&vello_encoding::DrawTag::IMAGE),
        )
    };
    assert_eq!(draw("ab :tilo:"), (9, false), "no window: the name is text");

    let scope = crate::wake::WakeScope::new();
    let _entry = scope.enter();
    crate::workspace_emoji::register(&scope.handle(), Rc::new(Fixed::new(workspace())));
    assert_eq!(
        draw("ab :tilo:"),
        (3, true),
        "the name's glyphs are left out for its picture"
    );
    assert_eq!(draw("ab :nope:"), (9, false));
}

#[test]
fn a_label_measures_and_paints_its_emoji_as_pictures() {
    let mut renderer = TextRenderer::with_font_system(crate::font_catalog::fixture_font_system());
    let style = TextStyle::new(14.0);
    let label = "Topic: ship it :party_parrot:";
    let paint = |renderer: &mut TextRenderer| {
        let mut scene = Scene::new();
        renderer.draw_style(
            &mut scene,
            label,
            Color::WHITE,
            Affine::IDENTITY,
            None,
            &style,
        );
        scene
            .encoding()
            .draw_tags
            .contains(&vello_encoding::DrawTag::IMAGE)
    };
    let (text_width, text_height) = renderer.measure_style(label, None, &style);
    assert!(!paint(&mut renderer), "no window: the label is text");

    let scope = crate::wake::WakeScope::new();
    let _entry = scope.enter();
    crate::workspace_emoji::register(&scope.handle(), Rc::new(Fixed::new(workspace())));
    let (width, height) = renderer.measure_style(label, None, &style);
    assert!(width < text_width - 14.0, "{width} vs {text_width}");
    assert!(
        (height - text_height).abs() < 0.01,
        "{height} vs {text_height}"
    );
    assert!(paint(&mut renderer));
    let in_window = renderer.measure_style("10:30 :nope:", None, &style);
    let detached = {
        let _detached = crate::wake::WakeScope::enter_detached();
        renderer.measure_style("10:30 :nope:", None, &style)
    };
    assert_eq!(
        in_window, detached,
        "text without the workspace's emoji is measured as before"
    );
}
