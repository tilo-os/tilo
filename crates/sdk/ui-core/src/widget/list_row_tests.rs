use super::*;
use crate::component::{Bound, Described, Fired, Intent};
use crate::headless::{FrameHarness, InspectionNode};
use crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST;
use crate::widget::{Column, Element};
use std::cell::Cell;
use std::rc::Rc;

fn face_height(face: &ListRowFace<'_>, scale: f32) -> f64 {
    let theme = Theme::dark().with_text_scale(scale);
    let mut text = TextRenderer::new();
    face.height(&mut text, &theme)
}

fn geometry(face: &ListRowFace<'_>, rect: Rect) -> RowGeometry {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    face.geometry(rect, &RowMetrics::new(&theme), &mut text, &theme)
}

fn find<'a>(node: &'a InspectionNode, widget: &str) -> Option<&'a InspectionNode> {
    if node.widget == widget {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, widget))
}

fn rect_of(node: &InspectionNode) -> Rect {
    let [x, y, w, h] = node.rect;
    Rect::new(x, y, x + w, y + h)
}

#[test]
fn a_title_cut_short_reads_in_full_in_the_tooltip() {
    let theme = crate::theme::Theme::dark();
    let message = "Expected `&[String]`, found `&Vec<String>` in the argument to parse_limit";
    let long = ListRow::new("long", message).meta("Ln 14, Col 29");
    let h = FrameHarness::for_root(crate::State::new(), long, (240, 60), theme.clone());
    let row = h.root::<ListRow>();
    let band = row.text_band();
    let (rect, text) = Widget::tooltip_target_at(row, band.center()).expect("elided title");
    assert_eq!(text, message);
    assert_eq!(rect, band);

    let short = ListRow::new("short", "Unused import").meta("Ln 2, Col 5");
    let h = FrameHarness::for_root(crate::State::new(), short, (480, 60), theme);
    let row = h.root::<ListRow>();
    assert_eq!(
        Widget::tooltip_target_at(row, row.text_band().center()),
        None,
        "a title that fits needs no tooltip"
    );
}

#[test]
fn a_row_tooltip_reads_anywhere_the_row_has_nothing_closer_to_say() {
    let theme = crate::theme::Theme::dark();
    let row = ListRow::new("used", "Deploy").tooltip("Used 3 times");
    let h = FrameHarness::for_root(crate::State::new(), row, (480, 60), theme);
    let row = h.root::<ListRow>();
    let (_, text) = Widget::tooltip_target_at(row, row.text_band().center()).expect("row tooltip");
    assert_eq!(text, "Used 3 times");
}

#[test]
fn a_badge_column_fits_every_badge_of_the_list() {
    let theme = crate::theme::Theme::dark();
    let badges: Rc<[String]> = vec!["SH".to_owned(), "RUST".to_owned()].into();
    let row = ListRow::new("short", "Deploy")
        .badge("SH")
        .badge_column(Rc::clone(&badges));
    let h = FrameHarness::for_root(crate::State::new(), row, (480, 60), theme.clone());
    let wide = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
        badge_column_width(text, &theme, &["SH", "RUST"])
    });
    let own = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
        badge_column_width(text, &theme, &["SH"])
    });
    assert!(wide > own);
    assert_eq!(h.root::<ListRow>().leading_column, Some(wide));
}

fn hovered() -> Interaction {
    Interaction {
        hovered: true,
        ..Interaction::default()
    }
}

#[test]
fn row_height_comes_from_measured_line_boxes_and_grows_with_text() {
    let single = ListRowFace::new("Inbox");
    let two = ListRowFace::new("Ada Lovelace").secondary("ada@example.org");
    let three = ListRowFace::new("Ada Lovelace")
        .secondary("The analytical engine weaves algebraic patterns")
        .two_line_secondary();
    let message = ListRowFace::new("Ada Lovelace")
        .subtitle("Notes on the engine")
        .secondary("The analytical engine weaves algebraic patterns")
        .two_line_secondary();
    let theme = Theme::dark();
    // Rows are 36 / 52 / 64 (design language, "Space and density").
    let single_row = f64::from(theme.tokens.metrics.row_single);
    assert!(
        (face_height(&single, 1.0) - single_row).abs() < 0.5,
        "a one-line row is the single-row height, {single_row}"
    );
    assert!(face_height(&two, 1.0) > face_height(&single, 1.0));
    assert!(face_height(&three, 1.0) > face_height(&two, 1.0));
    assert!(
        face_height(&message, 1.0) > face_height(&three, 1.0),
        "a subtitle is its own line"
    );
    assert!(
        face_height(&ListRowFace::new("A long name").two_line_title(), 1.0)
            > face_height(&single, 1.0) - 0.5
    );
    assert!(face_height(&single, 2.0) > face_height(&single, 1.0));
    for face in [&two, &three] {
        assert!(
            face_height(face, 2.0) > face_height(face, 1.0) * 1.5,
            "rows grow with the text scale"
        );
    }
}

/// A note is a line of the row's own under its text: the builder reaches the face, the line
/// is measured, and the reader is told it with the row.
#[test]
fn a_rows_note_is_a_line_of_its_own_under_its_text() {
    let row = ListRow::new("note-row", "Claude")
        .secondary("Kept by you")
        .note("$0.42 of $5 today");
    assert_eq!(
        row.with_face(|face| face.note.map(str::to_owned)),
        Some("$0.42 of $5 today".to_owned()),
        "the builder's note is the face's"
    );
    let plain = ListRowFace::new("Claude").secondary("Kept by you");
    let noted = plain.clone().note("$0.42 of $5 today");
    assert!(
        face_height(&noted, 1.0) > face_height(&plain, 1.0),
        "the note is a line under the row's text, not on it"
    );
}

/// A dense row is a quiet reference under content: one line at the caption's size with the
/// small glyph, as tall as that line and its inset rather than the list's row height, and a
/// finger's press on one still grows it to its target.
#[test]
fn a_dense_row_is_its_caption_line_not_a_list_row() {
    use crate::interaction::InteractionEnvironment;
    let theme = Theme::dark();
    let single = f64::from(theme.tokens.metrics.row_single);
    let quiet = ListRowFace::new("The move flat on Saturday").dense();
    assert!(
        face_height(&quiet, 1.0) < single,
        "a dense row stands under the list's row height"
    );
    assert!(
        face_height(&ListRowFace::new("Water the plants"), 1.0)
            - face_height(&ListRowFace::new("Water the plants").dense(), 1.0)
            > f64::from(theme.tokens.spacing.rhythm.tight),
        "the list's row height leaves it, not only its title's size"
    );
    assert!(
        face_height(&quiet, 2.0) > face_height(&quiet, 1.0) * 1.5,
        "a dense row grows with the text scale"
    );

    let row = || {
        ListRow::new("made", "The move flat on Saturday")
            .dense()
            .on_activate(|| {})
    };
    for (name, input, want) in [
        (
            "a pointer",
            InteractionEnvironment::POINTER,
            face_height(&quiet, 1.0),
        ),
        (
            "a finger",
            InteractionEnvironment::TOUCH,
            f64::from(InteractionEnvironment::TOUCH.minimum_collection_row()),
        ),
    ] {
        let mut h = FrameHarness::for_root(
            crate::State::new(),
            Column::new().push(Box::new(row())),
            (360, 200),
            theme.clone(),
        );
        h.set_interaction_environment(input);
        h.relayout_at((360, 200)).expect("layout");
        let inspection = h.inspection();
        let height = rect_of(find(&inspection.root, "ListRow").expect("the row")).height();
        assert!(
            (height - want).abs() < 0.5,
            "a dense row is {want} tall as {name} shows it, not {height}"
        );
    }
}

#[test]
fn a_quiet_word_before_the_title_shares_its_line_and_takes_its_room() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let rect = Rect::new(0.0, 0.0, 200.0, 36.0);
    let plain = ListRowFace::new("Book the van");
    assert!(!plain.title_elided(rect, &mut text, &theme), "it fits");
    assert!(
        (face_height(&plain, 1.0) - face_height(&plain.clone().prefix("From"), 1.0)).abs() < 0.5,
        "the word shares the title's line"
    );
    let long = ListRowFace::new("Book the van")
        .prefix("From the message Bo sent about the move flat on Saturday morning");
    assert!(
        long.title_elided(rect, &mut text, &theme),
        "the title keeps only the room the word leaves it"
    );
}

#[test]
fn a_row_without_its_avatar_leaves_no_room_for_one() {
    // Facts are a flowing run: a row with no avatar takes no room for one, so the facts it
    // has run on to the row's shared trailing edge instead of stopping at a hole.
    let outer = Rect::new(0.0, 0.0, 400.0, 56.0);
    let with = ListRowFace::new("Design")
        .secondary("See you there")
        .meta("Thursday")
        .avatar(AvatarFace::new("Ada", "ada"));
    let without = ListRowFace::new("Empty")
        .secondary("No messages yet")
        .meta("Thursday");
    let with = geometry(&with, outer);
    let without = geometry(&without, outer);
    let avatar = with.avatar.expect("the row shows the face");
    assert!(without.avatar.is_none());
    assert!(
        with.meta_right < avatar.x0,
        "the day stands before the face: {with:?}"
    );
    assert!(
        (without.meta_right - avatar.x1).abs() < 0.01,
        "with no avatar the day ends on the facts' shared edge: {without:?} beside {with:?}"
    );
}

#[test]
fn a_cards_facts_run_on_without_the_room_of_those_it_has_not() {
    let theme = Theme::dark();
    let m = RowMetrics::card(&theme);
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let outer = Rect::new(0.0, 0.0, 320.0, 57.0);
    // A day and a flag, nobody assigned: the flag follows the day one spacing token on,
    // with no empty slot where the avatar would stand.
    let face = ListRowFace::new("Measure the new bedroom")
        .card()
        .meta("Saturday")
        .mark(Icon::Flag);
    let geo = face.geometry(outer, &m, &mut text, &theme);
    assert!(geo.avatar.is_none(), "{geo:?}");
    let marks = geo.marks_right.expect("the flag");
    assert!(
        (marks - m.mark - (geo.meta_right + m.tag_gap)).abs() < 0.01,
        "the flag follows the day one spacing token on: {geo:?}"
    );
}

#[test]
fn a_card_without_facts_has_no_facts_line() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let m = RowMetrics::card(&theme);
    let bare = ListRowFace::new("Measure the new bedroom").card();
    let with_facts = ListRowFace::new("Measure the new bedroom")
        .card()
        .meta("Saturday");
    let facts = RowMetrics::line(&mut text, &m.meta);
    let tight = f64::from(theme.tokens.spacing.rhythm.tight);
    let difference = with_facts.height(&mut text, &theme) - bare.height(&mut text, &theme);
    assert!(
        (difference - facts - tight).abs() < 0.5,
        "a card grows by its facts line and nothing when it has none: it grew {difference} \
         (facts {facts}, gap {tight}, bare {}, with {})",
        bare.height(&mut text, &theme),
        with_facts.height(&mut text, &theme)
    );
}

#[test]
fn a_card_wraps_its_title_up_to_three_lines_and_cuts_only_past_what_fits() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    // The narrowest room a lane gives a card.
    let width = f64::from(crate::widget::CardWidth::Regular.min_width(&theme));
    let lines = |text: &mut TextRenderer, title: &str| {
        ListRowFace::new(title)
            .card()
            .meta("Thursday")
            .card_title_lines(width, text, &theme)
    };
    let height = |text: &mut TextRenderer, title: &str, lines: u8| {
        ListRowFace::new(title)
            .card()
            .title_lines(lines)
            .meta("Thursday")
            .height(text, &theme)
    };
    // A title that fits keeps one line: a card never cuts what it can show.
    assert_eq!(lines(&mut text, "Book the van"), 1);
    // A longer one takes the lines it needs, and its card grows to hold them.
    let long = "Order boxes, tape and bubble wrap for the kitchen, the bedroom and the hallway";
    let wrapped = lines(&mut text, long);
    assert!(
        (2..=CARD_TITLE_LINES).contains(&wrapped),
        "a long title takes {wrapped} lines"
    );
    let one = height(&mut text, "Book the van", 1);
    let several = height(&mut text, long, wrapped);
    assert!(
        (44.0..=56.0).contains(&one),
        "a card of one title line and its facts is {one} tall"
    );
    assert!(
        several > one,
        "a card as tall as its title's lines: {several} for {wrapped}, {one} for one"
    );
    // Past three lines it is cut, and its card is no taller than three lines make it.
    let endless = "Words ".repeat(80);
    assert_eq!(lines(&mut text, &endless), CARD_TITLE_LINES);
    assert!(
        (height(&mut text, &endless, CARD_TITLE_LINES) - height(&mut text, long, 3)).abs() < 0.01,
        "a title past three lines stands in the room three take"
    );
}

#[test]
fn a_cards_facts_stand_under_its_title_from_where_its_text_begins() {
    let theme = Theme::dark();
    let m = RowMetrics::card(&theme);
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let outer = Rect::new(0.0, 0.0, 264.0, 57.0);
    let face = ListRowFace::new("Order boxes and tape")
        .card()
        .leading(RowLeading::Checkbox {
            checked: false,
            icon: None,
        })
        .meta("Thursday")
        .mark(Icon::Flag)
        .mark_tone(Tone::Warning);
    let geo = face.geometry(outer, &m, &mut text, &theme);
    let line = RowMetrics::line(&mut text, &face.title_style(&m));
    // The facts stand on one line under the title, read from where the title's text begins —
    // the day, then the flag — not spread across the card's width.
    assert!(
        (geo.meta_left - geo.text_x).abs() < 0.01,
        "the facts start at the title's text: {geo:?}"
    );
    let marks = geo.marks_right.expect("the flag");
    assert!(geo.meta_right <= marks, "the day stands before the flag");
    assert!(
        geo.facts_cy > outer.y0 + m.pad_y + line,
        "the facts stand under the title's line: {geo:?}"
    );
    assert!(
        geo.facts_cy <= outer.y1 - m.pad_y,
        "the facts stand inside the card"
    );
    // The checkbox hangs on the title's first line, not centred on the card.
    let first_cy = face.card_content_top(outer, &m, &mut text, &theme) + line / 2.0;
    assert!(
        (geo.leading.center().y - first_cy).abs() < 0.01,
        "the checkbox stands on the title's first line: {:?}",
        geo.leading
    );
    assert!(
        (first_cy - outer.center().y).abs() > 0.5,
        "the card holds more room than its content, and the line is not the card's middle"
    );
}

/// A card that a finger's target made taller than its one line centres that line in it: the
/// text never hangs under the top padding with dead space below.
#[test]
fn a_card_grown_to_a_fingers_target_centres_its_single_line() {
    let theme = Theme::dark();
    let m = RowMetrics::card(&theme);
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let face = ListRowFace::new("Screenshots for the docs").card();
    let line = RowMetrics::line(&mut text, &face.title_style(&m));
    let natural = face.height(&mut text, &theme);
    let touch =
        f64::from(crate::interaction::InteractionEnvironment::TOUCH.minimum_collection_row());
    assert!(touch > natural, "a finger's target grows a one-line card");
    let grown = Rect::new(0.0, 0.0, 264.0, touch);
    let top = face.card_content_top(grown, &m, &mut text, &theme);
    assert!(
        (top + line / 2.0 - grown.center().y).abs() < 0.5,
        "the title's line is centred in the grown card: {top} in {touch}"
    );
    // The natural card starts its text under its own padding: centring adds nothing where
    // the card is only as tall as the text it holds.
    let snug = Rect::new(0.0, 0.0, 264.0, natural);
    let top = face.card_content_top(snug, &m, &mut text, &theme);
    assert!(
        (top - (snug.y0 + m.pad_y)).abs() < 0.5,
        "a card that fits its text keeps its padding: {top}"
    );
    // A leading checkbox hangs on the centred line, as it hangs on the title's line.
    let boxed = ListRowFace::new("Screenshots for the docs")
        .card()
        .leading(RowLeading::Checkbox {
            checked: false,
            icon: None,
        });
    let geo = boxed.geometry(grown, &m, &mut text, &theme);
    assert!(
        (geo.leading.center().y - grown.center().y).abs() < 0.5,
        "the checkbox centres with the line: {:?}",
        geo.leading
    );
}

#[test]
fn a_rows_facts_stand_in_one_column_in_a_fixed_order() {
    let theme = Theme::dark();
    let m = RowMetrics::new(&theme);
    let mut text = TextRenderer::new();
    let outer = Rect::new(0.0, 0.0, 560.0, 44.0);
    let face = ListRowFace::new("Call the landlord about the boiler")
        .meta("Today 09:00")
        .avatar(AvatarFace::new("Ada", "ada"))
        .value("2/5")
        .count(3)
        .mark(Icon::Flag)
        .mark_tone(Tone::Warning);
    let geo = face.geometry(outer, &m, &mut text, &theme);
    let avatar = geo.avatar.expect("who is on it, in the facts");
    // When it is due, who is on it, how far it has got, how important it is: left to right,
    // and the title ends before the column starts.
    assert!(
        geo.meta_right <= avatar.x0,
        "the due fact stands left of the face: {geo:?}"
    );
    assert!(geo.text_right < geo.meta_right, "{geo:?}");
    let count = geo.count_right.expect("the count's pill");
    assert!(avatar.x1 <= count, "{geo:?}");
    let marks = geo.marks_right.expect("the priority mark");
    assert!(count <= marks, "{geo:?}");
    assert!(
        (geo.value_right - (outer.x1 - m.bleed)).abs() < 0.01,
        "the last fact ends on the row's content edge"
    );
    assert!(marks <= geo.value_right, "a value keeps the edge: {geo:?}");
    // The facts column ends on one edge down a list, whatever each row's facts say.
    let other = ListRowFace::new("Water the plants")
        .meta("Jun 30")
        .avatar(AvatarFace::new("Grace", "grace"))
        .value("12/5")
        .mark(Icon::Star);
    let other = other.geometry(outer, &m, &mut text, &theme);
    assert!((other.value_right - geo.value_right).abs() < 0.01);
    // A row whose facts end earlier keeps that same edge.
    let bare = ListRowFace::new("Water the plants").meta("Jun 30");
    let bare = bare.geometry(outer, &m, &mut text, &theme);
    assert!((bare.meta_right - geo.value_right).abs() < 0.01, "{bare:?}");
}

#[test]
fn text_sits_on_the_content_edge_and_the_lift_bleeds_past_it() {
    let theme = Theme::dark();
    let bleed = crate::chrome::row_bleed(&theme);
    let outer = Rect::new(0.0, 0.0, 400.0, 40.0);
    let plain = geometry(&ListRowFace::new("Inbox"), outer);
    assert!(
        (plain.text_x - bleed).abs() < 0.01,
        "a face's text starts one bleed inside its outer rect"
    );
    // A retained row in a padded column: its title starts on the column's
    // content edge, which is the row's own layout box.
    let rows: Element = Box::new(
        Column::new()
            .with_horizontal_padding(24.0, 24.0)
            .push(Box::new(ListRow::new("a", "Ada").on_view(|| {}))),
    );
    let harness = FrameHarness::new(crate::State::new(), rows, (400, 200));
    let row = rect_of(find(&harness.inspection().root, "ListRow").unwrap());
    assert!((row.x0 - 24.0).abs() < 0.5);
    let mut list_row = ListRow::new("a", "Ada");
    list_row.metrics = Some(RowMetrics::new(&theme));
    list_row.rect = row;
    let mut text = TextRenderer::new();
    let x = list_row.with_face(|face| {
        face.text_x(
            list_row.outer(),
            &RowMetrics::new(&theme),
            &mut text,
            &theme,
        )
    });
    assert!(
        (x - row.x0).abs() < 0.01,
        "title on the content edge: {x} vs {}",
        row.x0
    );
    assert!(list_row.outer().x0 < row.x0, "the lift bleeds outward");
}

#[test]
fn hover_actions_replace_the_meta_but_not_the_value() {
    let theme = Theme::dark();
    let rect = Rect::new(0.0, 0.0, 400.0, 48.0);
    let actions = [
        RowAction {
            icon: Icon::Reply,
            label: "Reply",
        },
        RowAction {
            icon: Icon::Trash,
            label: "Delete message",
        },
    ];
    let idle = ListRowFace::new("Ada").meta("09:41").actions(&actions);
    assert!(idle.action_rects(rect, &theme).is_empty(), "hidden at rest");
    let hover = idle.clone().interaction(hovered());
    let rects = hover.action_rects(rect, &theme);
    assert_eq!(rects.len(), 2);
    assert!(rects[0].x1 < rects[1].x0, "in order, not touching");
    for r in &rects {
        assert!(rect.contains(r.origin()) && r.x1 <= rect.x1);
        assert!(
            (r.center().y - rect.center().y).abs() < 0.01,
            "centred on the row"
        );
    }
    // A value stays: the actions stand before it.
    let valued = ListRowFace::new("Tokyo")
        .title_value("09:41")
        .actions(&actions)
        .interaction(hovered());
    let geo = geometry(&valued, rect);
    let rects = valued.action_rects(rect, &theme);
    let value_left = geo.value_right
        - f64::from(
            TextRenderer::new()
                .measure_style("09:41", None, &RowMetrics::new(&theme).value_title)
                .0,
        );
    assert!(rects.last().unwrap().x1 <= value_left + 0.01);
    // A touch pointer has no hover: a selected row shows its actions.
    let touch = ListRowFace::new("Ada")
        .actions(&actions)
        .reveal_actions(true);
    assert_eq!(touch.action_rects(rect, &theme).len(), 2);
    // A disabled row shows none.
    assert!(
        hover
            .clone()
            .enabled(false)
            .action_rects(rect, &theme)
            .is_empty()
    );
}

/// Assistive technology reaches a row's actions without hovering it: each
/// button says where it stands, in the slot the row keeps for it.
#[test]
fn a_rows_actions_say_where_they_stand_without_a_hover() {
    let theme = Theme::dark();
    let rect = Rect::new(0.0, 0.0, 400.0, 48.0);
    let mut row = ListRow::new("note-1", "Ada")
        .action(Icon::Plus, "New note under Ada", || {})
        .action(Icon::MoreHorizontal, "More actions for Ada", || {});
    row.metrics = Some(RowMetrics::new(&theme));
    row.theme = Some(theme.clone());
    row.rect = rect;
    let node = Widget::accessibility(&row).unwrap();
    let buttons: Vec<_> = node
        .children
        .iter()
        .filter(|child| {
            child
                .action_target
                .as_deref()
                .is_some_and(|target| target.contains("/action/"))
        })
        .collect();
    assert_eq!(buttons.len(), 2, "one button per action");
    assert!(
        row.action_rects().is_empty(),
        "the row is at rest: it shows no action"
    );
    // The band the row's own controls are hit-tested and published in.
    let band = row.text_band();
    let slots = row.with_face(|face| {
        face.clone()
            .interaction(hovered())
            .action_rects(band, &theme)
    });
    let bounds: Vec<Rect> = buttons
        .iter()
        .map(|button| match button.bounds {
            Some(bounds) => bounds.to_rect(),
            None => panic!("an action a reader can press says where it stands"),
        })
        .collect();
    assert_eq!(bounds.len(), slots.len());
    for (bounds, slot) in bounds.iter().zip(&slots) {
        assert!(
            (bounds.x0 - slot.x0).abs() < 0.01 && (bounds.x1 - slot.x1).abs() < 0.01,
            "the hovered row shows the action where its button stands: {bounds:?} vs {slot:?}"
        );
    }
}

/// A row says where its actions stand through the capability a list asks it for, so the list
/// reports the place without knowing the kind of row its host built.
#[test]
fn a_row_says_where_its_actions_stand() {
    let theme = Theme::dark();
    let rect = Rect::new(0.0, 0.0, 400.0, 48.0);
    let mut row = ListRow::new("note-1", "Ada")
        .selected(true)
        .action(Icon::Trash, "Delete", || {});
    // A touch pointer has no hover: the selected row shows its action.
    row.set_interaction_environment(crate::interaction::InteractionEnvironment::TOUCH);
    row.metrics = Some(RowMetrics::new(&theme));
    row.theme = Some(theme.clone());
    row.rect = rect;
    let rects = crate::widget::capability::capability::<dyn crate::widget::RowActionRects>(&row)
        .expect("the row answers where its actions stand")
        .row_action_rects();
    assert_eq!(rects.len(), 1, "one action, where the row shows it");
    assert!(rect.contains(rects[0].center()), "{rect:?} vs {rects:?}");
    assert_eq!(rects, row.action_rects(), "the same place it paints");
}

#[test]
fn the_text_ends_before_the_rows_facts() {
    let rect = Rect::new(0.0, 0.0, 400.0, 64.0);
    let bare =
        ListRowFace::new("Ada Lovelace").secondary("A preview that runs the full width of the row");
    let geo = geometry(&bare, rect);
    let bare_right = geo.text_right;
    assert!(
        (geo.title_right - geo.text_right).abs() < 0.01
            && (geo.second_right - geo.text_right).abs() < 0.01,
        "a row with no facts keeps the row's width"
    );
    let facts = bare.clone().meta("Yesterday").count(4);
    let geo = geometry(&facts, rect);
    assert!(geo.title_right < bare_right, "the facts narrow the title");
    assert!(
        (geo.second_right - geo.title_right).abs() < 0.01,
        "the preview ends where the title does, before the column"
    );
    assert!(
        geo.title_right <= geo.meta_left + 0.01,
        "no line runs into the facts' room"
    );
}

#[test]
fn trailing_columns_stack_from_the_edge_and_reserve_a_control() {
    let theme = Theme::dark();
    let rect = Rect::new(0.0, 0.0, 480.0, 40.0);
    let face = ListRowFace::new("Wi-Fi")
        .disclosure()
        .checked(true)
        .status(RowStatus::Active)
        .trailing_width(52.0);
    let geo = geometry(&face, rect);
    let edge = rect.x1 - crate::chrome::row_bleed(&theme);
    let chevron = geo.disclosure.unwrap();
    let check = geo.check.unwrap();
    let slot = geo.trailing.unwrap();
    assert!((chevron.x1 - edge).abs() < 0.01, "the chevron is outermost");
    assert!(check.x1 < chevron.x0);
    assert!(geo.dot_x.unwrap() < check.x0);
    assert!(slot.x1 < geo.dot_x.unwrap());
    assert!((slot.width() - 52.0).abs() < 0.01);
    assert!(geo.text_right < slot.x0);
    let mut text = TextRenderer::new();
    assert_eq!(face.trailing_rect(rect, &mut text, &theme), Some(slot));
}

#[test]
fn the_status_dot_sits_on_the_title_line_of_a_two_line_row() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let face = ListRowFace::new("Ada")
        .secondary("Online")
        .status(RowStatus::Active);
    let h = face.height(&mut text, &theme);
    let rect = Rect::new(0.0, 0.0, 300.0, h);
    let mut scene = Scene::new();
    face.paint(
        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
        rect,
    );
    let m = RowMetrics::new(&theme);
    let block = face.text_block(&mut text, &m);
    let title_cy = rect.center().y - block / 2.0 + RowMetrics::line(&mut text, &m.title) / 2.0;
    assert!(title_cy < rect.center().y - 1.0, "fixture: two lines");
    let dot = face.dot_centre(rect, &mut text, &theme).unwrap();
    assert!(
        (dot.y - title_cy).abs() < 0.5,
        "the dot is on the title's line"
    );

    // Beside a chevron the dot centres with it: one trailing line.
    let face = face.disclosure();
    let dot = face.dot_centre(rect, &mut text, &theme).unwrap();
    assert!(
        (dot.y - rect.center().y).abs() < 0.5,
        "the dot centres with the chevron"
    );
}

#[test]
fn separators_hide_around_the_selection() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let count = |face: ListRowFace<'_>, text: &mut TextRenderer| {
        let mut scene = Scene::new();
        face.paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, text, &theme),
            Rect::new(0.0, 0.0, 300.0, 40.0),
        );
        scene.encoding().draw_tags.len()
    };
    let plain = count(ListRowFace::new("a"), &mut text);
    let separated = count(ListRowFace::new("a").separator(false), &mut text);
    let above_selection = count(ListRowFace::new("a").separator(true), &mut text);
    let selected_plain = count(ListRowFace::new("a").selected(true), &mut text);
    let selected = count(
        ListRowFace::new("a").selected(true).separator(false),
        &mut text,
    );
    assert_eq!(separated, plain + 1);
    assert_eq!(above_selection, plain);
    assert_eq!(selected, selected_plain);
}

#[test]
fn a_selected_row_is_the_lift_on_every_preset() {
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let ground = theme.tokens.colors.surfaces.content;
        let fill = row_fill(&theme, true, false).unwrap();
        let moved = crate::surface::edge_delta(fill, ground);
        assert!(
            moved >= crate::tokens::colors::HAIRLINE_MIN_DELTA,
            "{}: selection moved the ground only {moved:.4}",
            theme.name
        );
        assert!(
            row_fill(&theme, false, false).is_none(),
            "idle is transparent"
        );
    }
}

#[test]
fn toned_meta_spans_stay_legible_on_the_ground_and_the_lift() {
    for theme in [Theme::dark(), Theme::light()] {
        let ground = theme.tokens.colors.surfaces.content;
        let lift = crate::tokens::colors::composite(row_fill(&theme, true, false).unwrap(), ground);
        for tone in [Tone::Success, Tone::Danger, Tone::Warning] {
            let ink = theme.tokens.colors.ink_on_any(
                &[ground, lift],
                tone.colour(&theme),
                SMALL_TEXT_MIN_CONTRAST,
            );
            for under in [ground, lift] {
                assert!(crate::a11y::contrast_ratio(ink, under) >= SMALL_TEXT_MIN_CONTRAST);
            }
        }
    }
    // The spans paint without a plain meta.
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut scene = Scene::new();
    ListRowFace::new("AAPL")
        .meta_spans(&[("+2.4%", Tone::Success), ("182.10", Tone::Neutral)])
        .selected(true)
        .paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 320.0, 40.0),
        );
    assert!(scene.encoding().resources.glyph_runs.len() >= 3);
}

#[test]
fn a_priority_tones_the_mark_and_the_trailing_fact() {
    let props = ListRowProps {
        key: "plants".to_owned(),
        title: "Water the plants".to_owned(),
        mark: Some(crate::icons::Icon::Flag),
        tone: Tone::Danger,
        meta: Some("Jul 2".to_owned()),
        meta_tone: Tone::Warning,
        ..ListRowProps::default()
    };
    let row = <ListRow as Described>::from_props("plants", props);
    assert_eq!(row.mark, Some(crate::icons::Icon::Flag));
    assert_eq!(row.mark_tone, Tone::Danger, "the mark and the box's ring");
    assert_eq!(
        row.meta_spans,
        vec![("Jul 2".to_owned(), Tone::Warning)],
        "the trailing fact reads in the tone it was given"
    );
    // A row no tone is given paints its mark as its quiet ink, and its fact plainly.
    let props = ListRowProps {
        key: "plain".to_owned(),
        title: "Water the plants".to_owned(),
        mark: Some(crate::icons::Icon::Flag),
        meta: Some("Jul 2".to_owned()),
        ..ListRowProps::default()
    };
    let row = <ListRow as Described>::from_props("plain", props);
    assert_eq!(row.mark_tone, Tone::Neutral);
    assert!(row.meta_spans.is_empty());
}

#[test]
fn every_leading_kind_paints_in_its_column_and_titles_line_up() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let column = badge_column_width(&mut text, &theme, &["GET", "POST", "DELETE"]);
    let m = RowMetrics::new(&theme);
    let rect = Rect::new(0.0, 0.0, 400.0, 40.0);
    let x = |badge: &str, text: &mut TextRenderer| {
        ListRowFace::new("/users")
            .leading(RowLeading::Badge(badge))
            .leading_column(column)
            .text_x(rect, &m, text, &theme)
    };
    let get = x("GET", &mut text);
    assert!((get - x("DELETE", &mut text)).abs() < 0.01);
    assert!(get > column);

    let image = crate::raster::rgba8(vec![128_u8; 4 * 4 * 4], 4, 4);
    let painted = Cell::new(false);
    let painter = |_: &mut PaintCtx<'_>, _: Rect| painted.set(true);
    let kinds = [
        RowLeading::Icon(Icon::Folder),
        RowLeading::Avatar(AvatarFace::new("Ada", "ada").square()),
        RowLeading::Thumbnail(&image),
        RowLeading::Media(MediaArt::Placeholder, MediaAspect::Wide),
        RowLeading::Checkbox {
            checked: true,
            icon: None,
        },
        RowLeading::Swatch(theme.tokens.colors.semantic.info),
        RowLeading::Signal(crate::widget::SignalLevel::Good),
        RowLeading::Paint(&painter),
    ];
    for kind in kinds {
        let face = ListRowFace::new("Quarterly report").leading(kind);
        let mut scene = Scene::new();
        face.paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            rect,
        );
        let lead = face.leading_rect(rect, &mut text, &theme);
        assert!(lead.width() > 0.0, "{kind:?} has a column");
        assert!(
            rect.x0 - 0.01 <= lead.x0 && lead.x1 <= rect.x1 + 0.01,
            "{kind:?} is pressed inside its row"
        );
    }
    assert!(painted.get(), "a leading painter runs");
    // A wide thumbnail takes a wider column than a square one.
    let wide = geometry(
        &ListRowFace::new("Clip")
            .leading(RowLeading::Media(MediaArt::Placeholder, MediaAspect::Wide)),
        rect,
    );
    let square = geometry(
        &ListRowFace::new("Clip").leading(RowLeading::Media(
            MediaArt::Placeholder,
            MediaAspect::Square,
        )),
        rect,
    );
    assert!(wide.leading.width() > square.leading.width());
}

/// The box a row leads with is a control of its own: a finger's press on it reaches the target a
/// finger needs, inside the row, while the box it draws keeps the size of its column and the row
/// keeps its height.
#[test]
fn a_rows_box_presses_as_far_as_the_pointer_needs() {
    use crate::interaction::InteractionEnvironment;
    let theme = Theme::dark();
    let row = || {
        ListRow::new("milk", "Milk")
            .checkbox(false, |_| {})
            .on_activate(|| {})
    };
    let mut widths = Vec::new();
    for (name, input) in [
        ("a pointer", InteractionEnvironment::POINTER),
        ("a finger", InteractionEnvironment::TOUCH),
    ] {
        let mut h = FrameHarness::for_root(
            crate::State::new(),
            Column::new().push(Box::new(row())),
            (360, 300),
            theme.clone(),
        );
        h.set_interaction_environment(input);
        h.relayout_at((360, 300)).expect("layout");
        let inspection = h.inspection();
        let node = find(&inspection.root, "ListRow").expect("the row");
        let row_rect = rect_of(node);
        let boxed = node
            .semantics
            .as_ref()
            .and_then(|semantics| {
                semantics
                    .children
                    .iter()
                    .find(|child| child.role == crate::a11y::Role::Checkbox)
            })
            .expect("the row's box is a control of its own")
            .bounds
            .expect("a box a reader presses says where it stands")
            .to_rect();
        let target = f64::from(input.minimum_target());
        assert!(
            boxed.width() >= target - 0.5 && boxed.height() >= target - 0.5,
            "as {name} presses it, the box takes a press {target}×{target} wide: {boxed:?}"
        );
        // The row's lift, which bleeds into the list's gutter as the row does.
        let lift = row_rect.inflate(crate::chrome::row_bleed(&theme), 0.0);
        assert!(
            lift.x0 - 0.5 <= boxed.x0
                && boxed.x1 <= lift.x1 + 0.5
                && lift.y0 - 0.5 <= boxed.y0
                && boxed.y1 <= lift.y1 + 0.5,
            "the box's press stays inside its row as {name} presses it: {boxed:?} in {lift:?}"
        );
        assert!(
            boxed.width() > m_icon(&theme),
            "the press reaches past the box it draws, {boxed:?}"
        );
        widths.push(boxed.width());
    }
    assert!(
        widths[1] > widths[0],
        "a finger's press on the box is wider than a mouse's: {widths:?}"
    );
}

/// The edge a row draws its leading box at, from the theme's metrics.
fn m_icon(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.icon_control)
}

#[test]
fn accessibility_names_the_badge_and_reads_parts_and_actions() {
    let actions = [RowAction {
        icon: Icon::Copy,
        label: "Copy URL",
    }];
    let face = ListRowFace::new("/users")
        .leading(RowLeading::Badge("GET"))
        .meta("200")
        .tags(&[("Beta", ChipStyle::Neutral)])
        .count(3)
        .checked(true)
        .actions(&actions);
    let node = face.accessibility(Rect::new(0.0, 0.0, 300.0, 40.0), "row");
    assert_eq!(node.name.as_deref(), Some("GET /users"));
    let value = node.value.unwrap();
    assert!(value.contains("Beta") && value.contains("200") && value.contains('3'));
    assert_eq!(node.state.checked, Some(true));
    assert_eq!(node.children.len(), 1);
    assert_eq!(node.children[0].role, Role::Button);
    assert_eq!(node.children[0].name.as_deref(), Some("Copy URL"));

    let row = ListRow::new("r", "/users")
        .badge("GET")
        .leading_tooltip("Retrieves the resource");
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.name.as_deref(), Some("GET /users"));
    assert_eq!(node.description.as_deref(), Some("Retrieves the resource"));

    // An icon that names a kind says it the same way, and keeps the title
    // as the row's whole name.
    let row = ListRow::new("r", "Build server")
        .icon(Icon::SquareTerminal)
        .leading_tooltip("SSH");
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.name.as_deref(), Some("Build server"));
    assert_eq!(node.description.as_deref(), Some("SSH"));
}

#[test]
fn an_item_nobody_has_named_shows_a_muted_placeholder_and_is_spoken_by_it() {
    let untitled = crate::i18n::I18nManager::lookup_plain("list-row-untitled");
    assert_ne!(untitled, "list-row-untitled", "the placeholder is a phrase");
    assert!(
        !untitled.is_empty(),
        "a row of an item nobody has named yet shows words"
    );
    // The row paints, so it is visible and pressable, and is announced by the placeholder.
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut scene = Scene::new();
    let row = ListRow::new("r", "");
    ListRowFace::new("").paint(
        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
        Rect::new(0.0, 0.0, 320.0, 40.0),
    );
    assert!(
        !scene.encoding().resources.glyph_runs.is_empty(),
        "an empty title paints its placeholder"
    );
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.name.as_deref(), Some(untitled.as_str()));

    // A title with nothing but space is no name either; one that says something is itself.
    let node = Widget::accessibility(&ListRow::new("r", "  ")).unwrap();
    assert_eq!(node.name.as_deref(), Some(untitled.as_str()));
    let node = Widget::accessibility(&ListRow::new("r", "Groceries")).unwrap();
    assert_eq!(node.name.as_deref(), Some("Groceries"));
}

#[test]
fn retained_row_activates_by_pointer_and_accessibility() {
    let hits = Rc::new(Cell::new(0));
    let counter = Rc::clone(&hits);
    let rows: Element = Box::new(
        Column::new().with_no_padding().push(Box::new(
            ListRow::new("contact-ada", "Ada Lovelace")
                .secondary("ada@example.org")
                .avatar("Ada Lovelace", "ada@example.org")
                .meta("Today")
                .on_view(move || counter.set(counter.get() + 1)),
        )),
    );
    let mut harness = FrameHarness::new(crate::State::new(), rows, (400, 200));
    let [x, y, w, h] = find(&harness.inspection().root, "ListRow")
        .expect("row laid out")
        .rect;
    assert!(h >= 40.0);
    harness.click_at(x + w / 2.0, y + h / 2.0);
    assert_eq!(hits.get(), 1);
    let mut row = ListRow::new("r", "Row").on_view(|| {});
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.role, Role::ListItem);
    assert_eq!(node.name.as_deref(), Some("Row"));
    let mut doc = automerge::AutoCommit::new();
    assert!(Widget::perform_action(
        &mut row,
        &crate::a11y::Action::Activate,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
}

#[test]
fn hover_actions_run_from_assistive_technology_and_the_keyboard() {
    let replied = Rc::new(Cell::new(0));
    let deleted = Rc::new(Cell::new(0));
    let (r, d) = (Rc::clone(&replied), Rc::clone(&deleted));
    let mut row = ListRow::new("m", "Ada")
        .on_view(|| {})
        .action(Icon::Reply, "Reply", move || r.set(r.get() + 1))
        .action(Icon::Trash, "Delete message", move || d.set(d.get() + 1));
    let mut doc = automerge::AutoCommit::new();
    assert!(Widget::perform_action(
        &mut row,
        &crate::a11y::Action::Activate,
        &[1],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!((replied.get(), deleted.get()), (0, 1));
    // Arrow keys arm the actions of a focused row; Enter runs the armed one.
    row.set_focus(true);
    row.armed_action = Some(0);
    assert!(Widget::perform_action(
        &mut row,
        &crate::a11y::Action::Activate,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(replied.get(), 1);
    row.set_focus(false);
    assert_eq!(row.armed_action, None, "leaving the row disarms");
    // A hover action that is only a callback is not a published action, so
    // the inventory reports nothing for it; the row's own activation and
    // binding the row's action are what make it visible to a native audit.
    let bindings = crate::widget::controls::ActionControls::control_action_bindings(&row);
    assert!(
        bindings
            .iter()
            .all(|binding| !matches!(binding.control_id.as_str(), "Reply" | "Delete message")),
        "the row's hover actions are callbacks, not published actions: {bindings:?}"
    );
}

#[test]
fn on_view_publishes_the_row_as_an_action_instead_of_a_callback() {
    let opened = Rc::new(Cell::new(0));
    let counter = Rc::clone(&opened);
    let mut row = ListRow::new("contact-ada", "Ada Lovelace").on_view(move || {
        counter.set(counter.get() + 1);
    });
    let bindings = crate::widget::controls::ActionControls::control_action_bindings(&row);
    assert_eq!(bindings.len(), 1);
    let doc = automerge::AutoCommit::new();
    let resources = crate::resources::PublishesResources::resources(&row, &doc);
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].label, "Ada Lovelace");
    assert!(
        crate::widget::resource_operation_handlers(&row)
            .contains(crate::widget::activation::NATIVE_INVOKE)
    );
    let mut doc = automerge::AutoCommit::new();
    let invocation = tilo_actions::InvocationId::new();
    let payload = serde_json::to_vec("contact-ada").unwrap();
    let started = crate::widget::ResourceOperation::local(
        invocation,
        tilo_actions::OperationKind::View,
        crate::widget::activation::NATIVE_INVOKE,
        &payload,
    )
    .start(
        &mut row,
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    )
    .unwrap();
    assert!(started.is_some());
    assert_eq!(opened.get(), 1);
}

/// A row's own chevron button lines up with the disclosure chevrons of
/// the rows around it.
#[test]
fn a_trailing_disclosure_button_stands_on_the_chevron_column() {
    let rows = Column::new()
        .with_no_padding()
        .push(Box::new(
            ListRow::new("all", "Show all").disclosure().on_view(|| {}),
        ))
        .push(Box::new(
            ListRow::new("home", "Home")
                .on_view(|| {})
                .trailing_disclosure(Box::new(crate::widget::Button::icon_only(
                    "home.open",
                    crate::icons::Icon::ChevronRight,
                    "Open Home",
                ))),
        ));
    let harness = FrameHarness::new(crate::State::new(), Box::new(rows), (600, 300));
    let root = harness.inspection().root;
    let row = rect_of(find(&root, "ListRow").unwrap());
    let button = rect_of(find(&root, "Button").unwrap());
    let theme = Theme::dark();
    let m = RowMetrics::new(&theme);
    let chevron = row.x1 - m.icon / 2.0;
    assert!(
        (button.center().x - chevron).abs() < 0.5,
        "centred on the chevron column: {button:?} vs {chevron}"
    );
    assert!(
        button.x1 <= row.x1 + m.bleed + 0.5,
        "inside the row's bleed: {button:?} in {row:?}"
    );
}

#[test]
fn a_trailing_control_stands_beside_the_status_dot_not_over_it() {
    let theme = Theme::light();
    let row = ListRow::new("failed", "Couldn't load this")
        .secondary("Lounge did not answer")
        .error()
        .trailing(Box::new(
            crate::widget::Button::new("retry", "Try again")
                .control_size(crate::widget::ControlSize::Small),
        ));
    let h = FrameHarness::for_root(crate::State::new(), row, (720, 120), theme.clone());
    let row = h.root::<ListRow>();
    let button = row.trailing.as_ref().unwrap().element.get_rect();
    let band = row.text_band();
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let dot = row
        .with_face(|face| face.dot_centre(band, &mut text, &theme))
        .unwrap();
    let m = RowMetrics::new(&theme);
    assert!(
        button.x1 + m.column_gap <= dot.x - m.dot / 2.0 + 0.5,
        "the control ends a column gap before the dot: {button:?}, dot at {dot:?}"
    );
    assert!(
        dot.x + m.dot / 2.0 <= row.get_rect().x1 + 0.5,
        "the dot keeps the row's edge: {dot:?} in {:?}",
        row.get_rect()
    );
}

#[test]
fn a_trailing_control_stands_before_the_disclosure_chevron_not_over_it() {
    let theme = Theme::light();
    let row = ListRow::new("vpn", "Office")
        .secondary("Connected")
        .trailing(Box::new(crate::widget::Toggle::new("vpn-toggle", "vpn")))
        .disclosure();
    let h = FrameHarness::for_root(crate::State::new(), row, (720, 120), theme.clone());
    let row = h.root::<ListRow>();
    let toggle = row.trailing.as_ref().unwrap().element.get_rect();
    let band = row.text_band();
    let m = RowMetrics::new(&theme);
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let chevron = row
        .with_face(|face| face.geometry(band, &m, &mut text, &theme))
        .disclosure
        .unwrap();
    assert!(
        toggle.x1 + m.column_gap <= chevron.x0 + 0.5,
        "the control ends a column gap before the chevron: {toggle:?}, chevron {chevron:?}"
    );
}

#[test]
fn a_trailing_control_reserves_its_width_and_wraps_under_narrow_text() {
    let layout = |width: u32| {
        let row = ListRow::new("wifi", "Wi-Fi")
            .secondary("Home network")
            .trailing(Box::new(crate::widget::Toggle::new("wifi-toggle", "wifi")));
        let harness = FrameHarness::new(
            crate::State::new(),
            Box::new(Column::new().with_no_padding().push(Box::new(row))),
            (width, 300),
        );
        let root = harness.inspection().root;
        let row = find(&root, "ListRow").unwrap();
        let toggle = rect_of(find(row, "Toggle").unwrap());
        (rect_of(row), toggle)
    };
    let (row, toggle) = layout(600);
    assert!(toggle.x1 <= row.x1 + 0.5, "the control ends on the edge");
    assert!(
        toggle.y0 >= row.y0 && toggle.y1 <= row.y1,
        "beside the text: {toggle:?} in {row:?}"
    );
    let (narrow, wrapped) = layout(150);
    assert!(narrow.height() > row.height(), "the row grows to hold it");
    assert!(
        wrapped.y0 > narrow.y0 + row.height() / 2.0,
        "under the text"
    );
    assert!(
        (wrapped.width() - toggle.width()).abs() < 0.5 && (wrapped.x0 - narrow.x0).abs() < 0.5,
        "a switch keeps its own size under the text, on the text's edge: {wrapped:?} in {narrow:?}"
    );
}

#[test]
fn a_leading_checkbox_toggles_apart_from_activation() {
    let toggled = Rc::new(Cell::new(None));
    let activated = Rc::new(Cell::new(0));
    let (t, a) = (Rc::clone(&toggled), Rc::clone(&activated));
    let mut row = ListRow::new("m", "Ada")
        .checkbox_view(false, move |on| t.set(Some(on)))
        .on_view(move || a.set(a.get() + 1));
    let mut doc = automerge::AutoCommit::new();
    assert!(Widget::perform_action(
        &mut row,
        &crate::a11y::Action::ToggleCheck,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(toggled.get(), None, "the box's report waits to be answered");
    assert_eq!(crate::testing::answer_controls(&mut row), 1);
    assert_eq!(toggled.get(), Some(true));
    assert_eq!(activated.get(), 0);
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.state.checked, Some(true));
}

#[test]
fn a_disabled_row_is_inert_and_skipped_by_focus() {
    let mut row = ListRow::new("r", "Row").on_view(|| {}).with_enabled(false);
    assert!(!row.is_focusable());
    assert!(!Widget::is_enabled(&row));
    let mut doc = automerge::AutoCommit::new();
    assert!(!Widget::perform_action(
        &mut row,
        &crate::a11y::Action::Activate,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
}

#[test]
fn a_touch_pointer_shows_a_selected_rows_actions() {
    let mut row = ListRow::new("m", "Ada")
        .selected(true)
        .action(Icon::Trash, "Delete", || {});
    assert!(!row.reveal_actions());
    row.set_interaction_environment(crate::interaction::InteractionEnvironment::TOUCH);
    assert!(row.reveal_actions());
}

#[test]
fn a_view_row_is_a_certified_action_a_click_and_an_agent_both_reach() {
    let calls = Rc::new(Cell::new(0));
    let invoked = Rc::clone(&calls);
    let row = ListRow::new("contact-ada", "Ada Lovelace")
        .secondary("ada@example.org")
        .on_view(move || invoked.set(invoked.get() + 1));
    let mut driver = FrameHarness::new(
        crate::State::new(),
        Box::new(Column::new().with_no_padding().push(Box::new(row))),
        (400, 200),
    );
    assert!(driver.native_app_audit().is_certified());
    assert!(driver.resources().iter().any(|resource| {
        resource.id.as_str() == "tilo.control:contact-ada"
            && resource.actions.iter().any(|action| action.id == "invoke")
    }));
    driver
        .perform_named("contact-ada", &crate::a11y::Action::Activate)
        .expect("the row enters through its action");
    assert_eq!(calls.get(), 1);
    let [x, y, w, h] = find(&driver.inspection().root, "ListRow").unwrap().rect;
    driver.click_at(x + w / 2.0, y + h / 2.0);
    assert_eq!(calls.get(), 2, "a click runs the same action");
}

#[test]
fn the_words_a_search_found_are_marked_at_word_starts_whatever_their_case() {
    let line = "The glass rattled; Glassworks and fibreglass.";
    let spans = found_spans(line, &["glass"]);
    let marked: Vec<&str> = spans
        .iter()
        .filter_map(|span| line.get(span.clone()))
        .collect();
    // "fibreglass" has it inside a word, which is not a match.
    assert_eq!(marked, ["glass", "Glass"]);
    assert!(found_spans(line, &[]).is_empty());
    assert!(found_spans(line, &[""]).is_empty());
    // The longer of two words found at one place is marked whole.
    let spans = found_spans("Tideglass letters", &["tide", "tideglass"]);
    assert_eq!(spans, [std::ops::Range { start: 0, end: 9 }]);
    // Letters whose lower case is longer still line up with the text.
    assert_eq!(
        found_spans("İstanbul", &["i̇stan"]),
        [std::ops::Range { start: 0, end: 6 }]
    );
}

#[test]
fn a_control_wrapped_under_its_text_fills_the_row_to_its_trailing_edge() {
    let rows = || -> Element {
        let segmented = ListRow::new("density", "Density")
            .secondary("How much room controls and rows leave around them")
            .trailing(Box::new(crate::widget::ToggleGroup::new(
                vec![
                    crate::widget::Segment::new("Compact"),
                    crate::widget::Segment::new("Regular"),
                    crate::widget::Segment::new("Roomy"),
                ],
                1,
            )));
        let chooser =
            ListRow::new("scale", "Appearance").trailing(Box::new(crate::widget::Dropdown::new(
                vec![
                    "Follow the day and night schedule".into(),
                    "Always light".into(),
                ],
                "scale",
            )));
        Box::new(
            Column::new()
                .with_horizontal_padding(24.0, 24.0)
                .push(Box::new(segmented))
                .push(Box::new(chooser)),
        )
    };
    let controls = |harness: &FrameHarness| {
        let root = harness.inspection().root;
        let column = rect_of(&root);
        let mut rows = Vec::new();
        collect(&root, "ListRow", &mut rows);
        let segmented = rect_of(find(rows[0], "ToggleGroup").unwrap());
        let chooser = rect_of(find(rows[1], "Dropdown").unwrap());
        (
            column,
            rect_of(rows[0]),
            rect_of(rows[1]),
            segmented,
            chooser,
        )
    };
    let mut harness = FrameHarness::new(crate::State::new(), rows(), (900, 400));
    let (_, row, _, segmented, chooser) = controls(&harness);
    assert!(
        segmented.y1 <= row.y1 && (segmented.x1 - row.x1).abs() < 0.5,
        "beside its text the control ends on the row's edge: {segmented:?} in {row:?}"
    );
    assert!(
        segmented.width() < row.width() / 2.0,
        "beside its text the control keeps its own width: {segmented:?}"
    );
    let wide_chooser = chooser.width();
    for width in [360, 420] {
        harness
            .resize((width, 600))
            .expect("the narrow layout settles");
        let (_, segmented_row, chooser_row, segmented, chooser) = controls(&harness);
        for (kind, control, row) in [
            ("segmented control", segmented, segmented_row),
            ("chooser", chooser, chooser_row),
        ] {
            assert!(
                control.y0 > row.y0 + row.height() / 3.0,
                "at {width}: the {kind} wraps under its text: {control:?} in {row:?}"
            );
            assert!(
                (control.x1 - row.x1).abs() < 0.5,
                "at {width}: the wrapped {kind} ends on the row's edge: {control:?} in {row:?}"
            );
            assert!(
                (control.x0 - row.x0).abs() < 0.5,
                "at {width}: the wrapped {kind} starts on the text's edge: {control:?} in {row:?}"
            );
        }
    }
    harness.resize((900, 400)).expect("the wide layout settles");
    let (_, row, _, segmented, chooser) = controls(&harness);
    assert!(
        segmented.y1 <= row.y1 && segmented.width() < row.width() / 2.0,
        "widened again, the control goes back beside its text at its own width: {segmented:?}"
    );
    assert!((chooser.width() - wide_chooser).abs() < 0.5);
}

fn collect<'a>(node: &'a InspectionNode, widget: &str, out: &mut Vec<&'a InspectionNode>) {
    if node.widget == widget {
        out.push(node);
    }
    for child in &node.children {
        collect(child, widget, out);
    }
}

#[test]
fn a_rows_skeleton_paints_where_the_row_does() {
    let theme = crate::theme::Theme::dark();
    let row = ListRow::new("task", "Water the plants").secondary("Today");
    let h = FrameHarness::for_root(crate::State::new(), row, (480, 80), theme.clone());
    let row = h.root::<ListRow>();
    let skeleton = crate::widget::SkeletonFace::new(crate::widget::SkeletonShape::Row(row.shape()));
    assert_eq!(
        skeleton.painted_rect(row.get_rect(), &theme),
        row.outer(),
        "the skeleton's bars start where the row's text does"
    );
}

/// On a touchscreen a row a person presses (one that opens, one with a checkbox) is at least
/// the minimum target tall, as a button's face grows to it, so a finger never lands on the
/// row beside it; a row that only reads keeps its height, and a mouse keeps the stock height.
#[test]
fn a_pressed_row_is_a_fingers_target_tall_on_touch() {
    use crate::interaction::InteractionEnvironment;
    let theme = Theme::light();
    let single = f64::from(theme.tokens.metrics.row_single);
    let touch = f64::from(InteractionEnvironment::TOUCH.minimum_collection_row());
    let rows: [(&str, fn() -> ListRow, bool); 3] = [
        (
            "opening",
            || ListRow::new("opens", "Q3 plan").on_activate(|| {}),
            true,
        ),
        (
            "checkbox",
            || ListRow::new("checkbox", "Buy oat milk").checkbox(false, |_| {}),
            true,
        ),
        (
            "reading",
            || ListRow::new("reads", "Battery").value("82%"),
            false,
        ),
    ];
    for (name, row, pressed) in rows {
        for (input, want) in [
            (InteractionEnvironment::POINTER, single),
            (
                InteractionEnvironment::TOUCH,
                if pressed { touch } else { single },
            ),
        ] {
            let mut h = FrameHarness::for_root(
                crate::State::new(),
                Column::new().push(Box::new(row())),
                (360, 200),
                theme.clone(),
            );
            h.set_interaction_environment(input);
            h.relayout_at((360, 200)).expect("layout");
            let inspection = h.inspection();
            let height = rect_of(find(&inspection.root, "ListRow").expect("the row")).height();
            assert!(
                (height - want).abs() < 0.5,
                "the {name} row is {want} tall for {input:?}, not {height}"
            );
        }
    }
}

#[test]
fn delete_runs_the_destructive_action_the_row_swipes_to() {
    let removed = Rc::new(Cell::new(0));
    let removed_too = Rc::clone(&removed);
    let mut row = ListRow::new("milk", "Buy milk")
        .on_activate(|| {})
        .swipe_action(Icon::Trash, "Delete", true, move || {
            removed_too.set(removed_too.get() + 1)
        });
    row.set_focus(true);
    let mut harness = FrameHarness::for_root(crate::State::new(), row, (420, 80), Theme::dark());
    harness.key("Delete");
    assert_eq!(
        removed.get(),
        1,
        "Delete takes the row away as its destructive swipe does"
    );

    // A swipe action the app did not mark destructive is not Delete's: the
    // key keeps its own meaning.
    let ran = Rc::new(Cell::new(0));
    let ran_too = Rc::clone(&ran);
    let mut kept = ListRow::new("milk", "Buy milk")
        .on_activate(|| {})
        .swipe_action(Icon::Star, "Pin", false, move || {
            ran_too.set(ran_too.get() + 1)
        });
    kept.set_focus(true);
    let mut harness = FrameHarness::for_root(crate::State::new(), kept, (420, 80), Theme::dark());
    harness.key("Delete");
    assert_eq!(ran.get(), 0, "a plain row action is not destructive");
}

/// The swipe a view bound to an action runs that action once, through the row's control door.
#[test]
fn a_swipe_bound_to_an_action_runs_it_once_as_the_person_swipes_to_it() {
    let mut spec = tilo_actions::ResourceSpec::new("res-milk", "test.errand", "Errand");
    spec = spec.with_action(
        tilo_actions::ActionSpec::operation(
            "delete",
            "Delete",
            tilo_actions::OperationKind::Effect,
            "test.errand.effect",
            &(),
        )
        .expect("a test action"),
    );
    let binding = tilo_actions::ActionBinding::unit_for(&spec, "delete").expect("published");
    let address = binding.address().clone();
    let mut row =
        ListRow::new("milk", "Buy milk").swipe_binding(Icon::Trash, "Delete", true, binding);
    row.swipe.as_mut().expect("a swipe").run();
    let taken = crate::widget::controls::take_control_invocation(&mut row);
    assert_eq!(
        taken.as_ref().map(|taken| taken.address.clone()),
        Some(address),
        "one swipe runs the action once"
    );
    assert!(
        crate::widget::controls::take_control_invocation(&mut row).is_none(),
        "the swipe is taken once"
    );
}

#[test]
fn a_menu_shortcut_runs_where_the_row_has_the_keyboard() {
    let checked = Rc::new(Cell::new(0));
    let checked_too = Rc::clone(&checked);
    let mut row = ListRow::new("milk", "Buy milk")
        .on_activate(|| {})
        .shortcuts(vec![(
            crate::platform::Shortcut::from("Ctrl+Enter"),
            Rc::new(move || checked_too.set(checked_too.get() + 1)) as Rc<dyn Fn()>,
        )]);
    row.set_focus(true);
    let mut harness = FrameHarness::for_root(crate::State::new(), row, (420, 80), Theme::dark());
    harness.key("Enter");
    assert_eq!(checked.get(), 0, "the key on its own is not the chord");
    harness.set_modifiers(&["command".to_owned()]);
    harness.key("Enter");
    harness.set_modifiers(&[]);
    assert_eq!(
        checked.get(),
        1,
        "the declared chord runs the command its menu offers"
    );
    harness.set_modifiers(&["command".to_owned(), "shift".to_owned()]);
    harness.key("Enter");
    harness.set_modifiers(&[]);
    assert_eq!(
        checked.get(),
        1,
        "a modifier the shortcut does not name is another press"
    );
}

/// Flip the leading checkbox the way the view does when the person presses it.
fn press_leading_checkbox(row: &mut ListRow) {
    if let Some(Leading::Checkbox { checked, .. }) = &mut row.leading {
        *checked = !*checked;
        row.dirty = true;
    } else {
        panic!("the row has no leading checkbox");
    }
}

#[test]
fn a_rows_checkbox_fills_over_frames_where_a_plain_face_would_pop() {
    use crate::motion::note_source;
    use crate::time::testing::with_monotonic_time;
    use crate::time::{Duration, Instant};
    use crate::tokens::MotionSource;

    let start = Instant::now();
    let row = ListRow::new("milk", "Buy oat milk")
        .secondary("Groceries")
        .checkbox(false, |_| {});
    let mut harness = with_monotonic_time(start, || {
        FrameHarness::for_root(crate::State::new(), row, (400, 60), Theme::dark())
    });
    with_monotonic_time(start, || harness.poll_frame()).expect("the first frame draws");
    assert_eq!(
        harness.root::<ListRow>().check_pose.expect("drawn").level,
        0.0,
        "it starts unchecked, at rest"
    );

    // The person presses the box: the view answers with the new value.
    let pressed = start + Duration::from_millis(100);
    with_monotonic_time(pressed, || {
        note_source(MotionSource::Pointer);
        press_leading_checkbox(&mut harness.root_mut::<ListRow>());
    });

    let mut levels = Vec::new();
    for frame in 1..=24_u64 {
        let at = pressed + Duration::from_millis(frame * 16);
        with_monotonic_time(at, || harness.poll_frame()).expect("a frame of the fill");
        levels.push(harness.root::<ListRow>().check_pose.expect("drawn").level);
    }
    // The press's own frame starts the motion; the ones after it show it on its way.
    let moving = levels.get(1).copied().expect("a second frame");
    assert!(
        moving > 0.0 && moving < 1.0,
        "the fill is on its way, not popped: {levels:?}"
    );
    assert!(
        levels
            .iter()
            .filter(|level| **level > 0.0 && **level < 1.0)
            .count()
            >= 3,
        "it takes several frames to settle: {levels:?}"
    );
    assert!(
        levels.windows(2).all(|pair| pair[1] >= pair[0] - 1e-6),
        "it approaches the marked state: {levels:?}"
    );
    let settled = harness.root::<ListRow>().check_pose.expect("drawn").level;
    assert!((settled - 1.0).abs() < 1e-3, "it settles marked: {settled}");
    // Once the mark has landed it asks for no more frames.
    let landed = pressed + Duration::from_millis(900);
    with_monotonic_time(landed, || {
        assert!(
            !harness
                .root::<ListRow>()
                .check
                .is_moving(crate::time::now()),
            "a settled box asks for no more frames"
        );
    });
}

#[test]
fn a_press_on_the_leading_box_marks_the_row() {
    let toggled = Rc::new(Cell::new(None));
    let seen = Rc::clone(&toggled);
    let row = ListRow::new("milk", "Buy oat milk").checkbox(false, move |on| seen.set(Some(on)));
    let mut harness = FrameHarness::new(crate::State::new(), Box::new(row), (400, 60));
    let [x, y, _, h] = find(&harness.inspection().root, "ListRow")
        .expect("the row is laid out")
        .rect;
    harness.click_at(x + 12.0, y + h / 2.0);
    assert_eq!(
        toggled.get(),
        Some(true),
        "a press on the box a person sees toggles it"
    );
}

/// A row of the described kind whose leading checkbox is bound to `state`, as a view binds one:
/// what the person does with the box is reported to `intents` as the row's `check` event.
fn bound_row(state: &crate::LocalState<bool>, intents: &crate::Intents<Fired>) -> ListRow {
    let props = ListRowProps {
        key: "milk".to_owned(),
        title: "Buy oat milk".to_owned(),
        checkbox: Some(Bound {
            state: state.clone(),
            edits: Some(Intent::new(intents, "milk", "check")),
            name: "Done".to_owned(),
            path: Vec::new(),
        }),
        ..ListRowProps::default()
    };
    <ListRow as Described>::from_props("milk", props)
}

#[test]
fn a_bound_rows_checkbox_follows_a_check_made_elsewhere() {
    let state = crate::LocalState::new(false);
    let intents = crate::Intents::<Fired>::default();
    let row = bound_row(&state, &intents);
    let mut harness = FrameHarness::for_root(crate::State::new(), row, (400, 60), Theme::dark());
    harness.poll_frame().expect("the first frame draws");
    assert_eq!(
        harness.root::<ListRow>().check_pose.expect("drawn").level,
        0.0,
        "it starts unchecked"
    );

    // The task is checked off somewhere else: another device, an agent, an undo.
    state.set(true);
    harness
        .recompose()
        .expect("the frame that takes the new value");
    harness.poll_frame().expect("the frame that shows it");
    assert_eq!(
        harness.root::<ListRow>().check_pose.expect("drawn").level,
        1.0,
        "a check made elsewhere shows in the row, landed rather than replayed"
    );
    let node = Widget::accessibility(harness.root::<ListRow>()).expect("a list item");
    assert_eq!(
        node.state.checked,
        Some(true),
        "assistive technology reads the check made elsewhere"
    );

    // And a check taken away elsewhere goes the same way.
    state.set(false);
    harness
        .recompose()
        .expect("the frame that takes the new value");
    harness.poll_frame().expect("the frame that shows it");
    assert_eq!(
        harness.root::<ListRow>().check_pose.expect("drawn").level,
        0.0,
        "a check taken away elsewhere clears the box"
    );
    assert!(
        intents.take().is_empty(),
        "a value set elsewhere is not the person's edit to report back"
    );
}

/// A row checked off reads as done with: its title struck through and in the weaker ink, as a
/// row of an item that left reads. The box says it, so the row says it too, and a row whose
/// item came back says it no more.
#[test]
fn a_checked_rows_title_is_struck_through_and_muted() {
    let face_strikes = |row: &ListRow| row.with_face(|face| face.struck);
    let mut row = ListRow::new("milk", "Buy oat milk").checkbox(false, |_| {});
    assert!(!face_strikes(&row), "an unchecked row's title is plain");
    press_leading_checkbox(&mut row);
    assert!(
        face_strikes(&row),
        "a checked row's title is struck through and drawn in the weaker ink"
    );
    press_leading_checkbox(&mut row);
    assert!(!face_strikes(&row), "a reopened row's title is plain again");
    assert!(
        face_strikes(&ListRow::new("stale", "Old errand").struck()),
        "a row of a removed item is struck the same way"
    );
}

#[test]
fn a_toned_meta_is_spoken_once() {
    // A late day draws as a span in the danger tone: one fact, one span, said once.
    let node = ListRowFace::new("Ring the letting agent")
        .meta("Overdue since Jun 29")
        .meta_spans(&[("Overdue since Jun 29", Tone::Danger)])
        .accessibility(Rect::new(0.0, 0.0, 320.0, 40.0), "late");
    assert_eq!(node.value.as_deref(), Some("Overdue since Jun 29"));
}

#[test]
fn a_person_pressing_a_bound_rows_box_still_sees_the_check_fill_in() {
    use crate::time::testing::with_monotonic_time;
    use crate::time::{Duration, Instant};

    let start = Instant::now();
    let state = crate::LocalState::new(false);
    let intents = crate::Intents::<Fired>::default();
    let row = bound_row(&state, &intents);
    let mut harness = with_monotonic_time(start, || {
        FrameHarness::for_root(crate::State::new(), row, (400, 60), Theme::dark())
    });
    with_monotonic_time(start, || harness.poll_frame()).expect("the first frame draws");
    assert_eq!(
        harness.root::<ListRow>().check_pose.expect("drawn").level,
        0.0,
        "it starts unchecked"
    );
    let [x, y, _, h] = find(&harness.inspection().root, "ListRow")
        .expect("the row is laid out")
        .rect;
    let pressed = start + Duration::from_millis(100);
    with_monotonic_time(pressed, || harness.click_at(x + 12.0, y + h / 2.0));
    assert!(state.get(), "the binding took the person's check");
    let fired = intents.take();
    assert_eq!(fired.len(), 1, "the check is reported once");
    assert_eq!(
        (fired[0].key.as_str(), fired[0].event.as_str()),
        ("milk", "check"),
        "as the row's check event"
    );
    assert_eq!(fired[0].value, Some(crate::component::Value::Bool(true)));

    let mut levels = Vec::new();
    for frame in 1..=24_u64 {
        let at = pressed + Duration::from_millis(frame * 16);
        with_monotonic_time(at, || harness.poll_frame()).expect("a frame of the fill");
        levels.push(harness.root::<ListRow>().check_pose.expect("drawn").level);
    }
    assert!(
        levels
            .iter()
            .filter(|level| **level > 0.0 && **level < 1.0)
            .count()
            >= 3,
        "the bound box fills over several frames rather than popping: {levels:?}"
    );
    assert!(
        levels.windows(2).all(|pair| pair[1] >= pair[0] - 1e-6),
        "it approaches the marked state: {levels:?}"
    );
    let settled = harness.root::<ListRow>().check_pose.expect("drawn").level;
    assert!((settled - 1.0).abs() < 1e-3, "it settles marked: {settled}");
}

#[test]
fn a_row_that_says_still_marks_at_once_where_brisk_still_moves() {
    use crate::motion::note_source;
    use crate::time::testing::with_monotonic_time;
    use crate::time::{Duration, Instant};
    use crate::tokens::MotionSource;

    let start = Instant::now();
    let row = ListRow::new("milk", "Buy oat milk")
        .checkbox(false, |_| {})
        .with_feel(crate::motion::Feel::Still);
    let mut harness = with_monotonic_time(start, || {
        FrameHarness::for_root(crate::State::new(), row, (400, 60), Theme::dark())
    });
    with_monotonic_time(start, || harness.poll_frame()).expect("the first frame draws");
    let pressed = start + Duration::from_millis(100);
    with_monotonic_time(pressed, || {
        note_source(MotionSource::Pointer);
        press_leading_checkbox(&mut harness.root_mut::<ListRow>());
    });
    // The press's frame starts the mark; with nothing to move, the one after it shows it landed.
    for ms in [16_u64, 32] {
        with_monotonic_time(pressed + Duration::from_millis(ms), || harness.poll_frame())
            .expect("a frame");
    }
    assert_eq!(
        harness.root::<ListRow>().check_pose.expect("drawn").level,
        1.0,
        "a still row's mark lands at once"
    );
}

#[test]
fn a_nested_row_indents_its_content_a_gutter_a_step_and_keeps_its_trailing_column() {
    let theme = Theme::dark();
    let step = crate::chrome::disclosure_gutter(&theme);
    let rect = Rect::new(0.0, 0.0, 300.0, 40.0);
    let top = geometry(&ListRowFace::new("Top"), rect);
    let nested = geometry(&ListRowFace::new("Under").depth(2), rect);
    assert!(
        (nested.leading.x0 - (top.leading.x0 + 2.0 * step)).abs() < 0.5,
        "the nested row's content stands two gutters in: {} from {}",
        nested.leading.x0,
        top.leading.x0
    );
    assert!(
        (nested.text_x - (top.text_x + 2.0 * step)).abs() < 0.5,
        "its title moves with its content"
    );
    assert!(
        (nested.value_right - top.value_right).abs() < 0.5,
        "depth never moves the row's trailing facts: {} and {}",
        nested.value_right,
        top.value_right
    );
}

/// A row of a tree keeps one disclosure step free in front of its content, whether or not it
/// has anything to fold: a leaf's title starts on the column its branch sibling's does, and
/// the step is where the list paints the chevron that folds it.
#[test]
fn a_leaf_stands_on_the_column_its_branch_sibling_keeps() {
    let theme = Theme::dark();
    let step = crate::chrome::disclosure_gutter(&theme);
    let rect = Rect::new(0.0, 0.0, 300.0, 40.0);
    let branch = geometry(
        &ListRowFace::new("Find the last service date").tree_row(true),
        rect,
    );
    let leaf = geometry(
        &ListRowFace::new("Ask whether Tuesday morning works").tree_row(true),
        rect,
    );
    let outside = geometry(&ListRowFace::new("Not a tree's row"), rect);
    assert!(
        (leaf.text_x - branch.text_x).abs() < 0.01,
        "one column at one depth, disclosure or not: {} and {}",
        leaf.text_x,
        branch.text_x
    );
    assert!(
        (branch.text_x - (outside.text_x + step)).abs() < 0.5,
        "the step stands free in front of the tree's content"
    );
    assert!(
        (leaf.leading.x0 - branch.leading.x0).abs() < 0.01,
        "what leads the row moves with it"
    );
}

/// A row given its own height stands at it instead of the list's row height for what it holds:
/// a tree's rows are its own token, shorter than a list's row of the same theme, and its title
/// reads at the row's size, not a dense row's caption.
#[test]
fn a_row_given_its_own_height_stands_at_it() {
    let list_row = face_height(&ListRowFace::new("Project"), 1.0);
    let tree_row = face_height(&ListRowFace::new("Project").row_height(28.0), 1.0);
    assert!(
        (tree_row - 28.0).abs() < 0.5,
        "the tree's own row token: {tree_row}"
    );
    assert!(
        tree_row < list_row,
        "a tree's row stands under the list's row of the same theme"
    );
}
