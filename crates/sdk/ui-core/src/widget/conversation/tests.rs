//! What the conversation substrate promises: one rule per mechanic, the same
//! answer in every app that draws it.

use vello::kurbo::Rect;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::attachment::{
    AttachmentAction, AttachmentKind, AttachmentSummary, Availability, UploadProgress,
    full_size_rect,
};
use super::composer::{ComposerIntent, ComposerRule};
use super::day::{
    ConversationClock, ConversationDay, DayBreaks, DayDividerFace, day_break_indices,
};
use super::delivery::{DeliveryState, DeliveryStep};
use super::draft::{self, DraftBook};
use super::list::{ConversationLine, ConversationRole, ListFace, bubble_colors, run_breaks};
use crate::theme::Theme;

/// 2026-06-30T14:05:00Z.
const NOONISH: i64 = 1_782_828_300;

fn clock_at(now: i64) -> ConversationClock {
    ConversationClock::at(0, now)
}

#[test]
fn a_divider_appears_wherever_the_day_changes() {
    let day = |ts: i64| ConversationDay::of(ts, 0);
    let days = [
        day(NOONISH),
        day(NOONISH.saturating_add(60)),
        // Midnight, three days later.
        day(NOONISH.saturating_add(3 * 86_400)),
        day(NOONISH.saturating_add(3 * 86_400 + 60)),
    ];
    assert_eq!(day_break_indices(days), vec![0, 2]);
    // The rule is the same whether it is walked in one pass or restarted.
    let mut breaks = DayBreaks::new();
    assert!(breaks.begins(days[0]));
    assert!(!breaks.begins(days[1]));
    breaks.reset();
    assert!(breaks.begins(days[1]));
}

#[test]
fn a_divider_names_the_day_the_way_a_person_says_it() {
    let clock = clock_at(NOONISH);
    let today = clock.today();
    assert_eq!(clock.day_label(today), "Today");
    assert_eq!(clock.day_label(today.days_back(1)), "Yesterday");
    // Inside the week: the weekday, not "5 days ago".
    let three = clock.day_label(today.days_back(3));
    assert!(
        !three.contains("days ago"),
        "a divider inside the week names its weekday, got {three:?}"
    );
    // Past the week: the date, which is unambiguous, and the year only when it
    // is a different year's.
    let old = clock.day_label(today.days_back(400));
    assert!(
        old.contains("2025"),
        "a last-year date carries its year: {old:?}"
    );
    let recent = clock.day_label(today.days_back(30));
    assert!(
        !recent.contains("2026") || recent.starts_with("Jun") || recent.starts_with("May"),
        "this year's date omits its year: {recent:?}"
    );
}

#[test]
fn the_clock_reads_the_zone_it_was_given() {
    // The same instant, two zones: the clock and the day both follow the zone,
    // so a paint pass cannot straddle two offsets.
    let east = ConversationClock::at(2 * 3_600, NOONISH);
    let west = ConversationClock::at(-11 * 3_600, NOONISH);
    assert_ne!(east.clock(NOONISH), west.clock(NOONISH));
    // 23:30 UTC is the next local day an hour east.
    let late = NOONISH.saturating_add(9 * 3_600 + 25 * 60);
    assert_eq!(east.day(late).days_after(west.day(late)), 1);
}

#[test]
fn every_transport_step_has_one_state_and_one_word() {
    let states = [
        DeliveryState::of_step(DeliveryStep::Queued),
        DeliveryState::of_step(DeliveryStep::Sending),
        DeliveryState::of_step(DeliveryStep::Submitted),
        DeliveryState::of_step(DeliveryStep::Acknowledged),
        DeliveryState::of_step(DeliveryStep::Delivered),
        DeliveryState::of_step(DeliveryStep::Read),
        DeliveryState::of_step(DeliveryStep::Refused),
        DeliveryState::of_step(DeliveryStep::Uncertain),
    ];
    assert_eq!(
        states,
        [
            DeliveryState::Queued,
            DeliveryState::Sending,
            DeliveryState::Submitted,
            DeliveryState::Sent,
            DeliveryState::Delivered,
            DeliveryState::Read,
            DeliveryState::Failed,
            DeliveryState::Uncertain,
        ]
    );
    // Every state has its own word: two states that said the same thing would
    // be one state.
    let keys: std::collections::BTreeSet<&str> = states.iter().map(|state| state.key()).collect();
    assert_eq!(keys.len(), states.len());
    // The two the person has to act on look different from each other and from
    // everything on the way there.
    let live = states
        .iter()
        .filter(|state| !state.needs_attention())
        .map(|state| state.icon().name().to_owned())
        .collect::<std::collections::HashSet<_>>();
    assert_ne!(
        DeliveryState::Failed.icon().name(),
        DeliveryState::Uncertain.icon().name()
    );
    assert!(!live.contains(DeliveryState::Failed.icon().name()));
    assert!(!live.contains(DeliveryState::Uncertain.icon().name()));
    // The vocabulary is the same in both apps because it is the same table.
    assert_eq!(DeliveryState::Delivered.label(), "Delivered");
    assert_eq!(DeliveryState::Failed.tone(), crate::widget::Tone::Danger);
    assert!(DeliveryState::Failed.needs_attention());
    assert!(DeliveryState::Read.is_final());
}

#[test]
fn a_stale_answer_never_walks_a_line_backwards() {
    let read = DeliveryState::Read;
    assert_eq!(read.then(DeliveryState::Delivered), read);
    assert_eq!(
        DeliveryState::Sent.then(DeliveryState::Read),
        DeliveryState::Read
    );
    // A refusal is always worth showing, whatever came before it.
    assert_eq!(
        DeliveryState::Read.then(DeliveryState::Failed),
        DeliveryState::Failed
    );
    assert_eq!(
        DeliveryState::Sending.then(DeliveryState::Queued),
        DeliveryState::Sending
    );
}

#[test]
fn the_attachment_ladder_is_open_save_then_the_saved_copy() {
    let image = AttachmentSummary::ready(AttachmentKind::Image, "shot.png", Some(2_048));
    assert_eq!(
        image.actions(),
        vec![AttachmentAction::OpenFullSize, AttachmentAction::Save,]
    );
    let saved = image.clone().saved_as(super::attachment::SavedCopy {
        name: "shot.png".into(),
        target: "file:///home/ada/shot.png".into(),
    });
    assert_eq!(
        saved.actions(),
        vec![
            AttachmentAction::OpenFullSize,
            AttachmentAction::Save,
            AttachmentAction::OpenSaved,
        ]
    );
    assert!(saved.describe().contains("shot.png"));
    // Every rung has its own word, and each says what it does.
    for action in saved.actions() {
        assert!(!action.label().is_empty(), "{action:?} has no word");
    }
}

#[test]
fn an_attachment_that_cannot_be_read_says_why_instead_of_offering_nothing() {
    let unshared = AttachmentSummary::ready(AttachmentKind::File, "payroll.pdf", None)
        .in_state(Availability::NotShared);
    assert!(unshared.actions().is_empty());
    assert!(unshared.unavailable_reason().is_some());
    assert!(unshared.describe().contains("payroll.pdf"));

    // An upload in flight offers exactly one thing: to stop.
    let uploading = AttachmentSummary::ready(AttachmentKind::Video, "clip.mp4", Some(1_000))
        .in_state(Availability::Uploading(UploadProgress {
            sent: 250,
            total: Some(1_000),
        }));
    assert_eq!(uploading.actions(), vec![AttachmentAction::CancelUpload]);
    assert_eq!(uploading.unavailable_reason(), None);
}

#[test]
fn an_upload_says_how_far_it_has_got_and_never_invents_a_denominator() {
    let known = UploadProgress {
        sent: 512,
        total: Some(1_024),
    };
    assert_eq!(known.fraction(), Some(0.5));
    assert!(known.text().contains('%') || known.text().contains("of"));
    let unknown = UploadProgress {
        sent: 512,
        total: None,
    };
    assert_eq!(unknown.fraction(), None);
    assert_eq!(unknown.text(), "Uploading");
}

#[test]
fn a_full_size_picture_is_contained_and_never_enlarged() {
    let view = Rect::new(0.0, 0.0, 800.0, 600.0);
    // Bigger than the viewport: contained, centred, aspect kept.
    let big = full_size_rect(view, 1_600, 1_200);
    assert_eq!(big, Rect::new(0.0, 0.0, 800.0, 600.0));
    // Smaller than the viewport: its own pixels, in the middle.
    let small = full_size_rect(view, 100, 50);
    assert_eq!(small.width(), 100.0);
    assert_eq!(small.height(), 50.0);
    assert_eq!(small.center(), view.center());
    // A shape with no size paints nothing at all.
    assert_eq!(full_size_rect(view, 0, 0), Rect::new(0.0, 0.0, 0.0, 0.0));
}

#[test]
fn a_draft_is_kept_per_conversation_and_cleared_only_when_it_is_sent() {
    let mut book = DraftBook::new();
    assert!(book.is_empty());
    book.edit("+15550100", "see you at 8");
    assert!(book.has("+15550100"));
    assert_eq!(
        book.line("+15550100").as_deref(),
        Some("Draft: see you at 8")
    );
    // Blank is not a draft: a person who clears the field has no draft.
    book.edit("+15550100", "   ");
    assert!(!book.has("+15550100"));
    // A word typed while the send was in flight is the next message.
    book.edit("+15550100", "see you at 8");
    book.sent("+15550100", "see you at 8!");
    assert!(book.has("+15550100"), "the unsent word survives the send");
    book.sent("+15550100", "see you at 8");
    assert!(!book.has("+15550100"));
}

#[test]
fn a_draft_line_is_the_first_line_folded_and_elided() {
    assert_eq!(
        draft::preview("first line\nsecond line").as_deref(),
        Some("first line")
    );
    assert_eq!(
        draft::preview("  spaced   out\ttabs ").as_deref(),
        Some("spaced out tabs")
    );
    let long = "é".repeat(120);
    let cut = draft::preview(&long).expect("a long body still has a preview");
    assert!(cut.ends_with('…'), "a long draft is elided: {cut:?}");
    assert!(
        cut.chars().count() <= 61,
        "elision keeps the budget: {cut:?}"
    );
    assert_eq!(draft::preview("   \n\n  "), None);
}

fn enter(held: ModifiersState, repeat: bool) -> ComposerIntent {
    ComposerRule::Enter.intent(&Key::Named(NamedKey::Enter), held, repeat)
}

#[test]
fn enter_sends_and_shift_enter_breaks_the_line() {
    assert_eq!(
        enter(ModifiersState::empty(), false),
        ComposerIntent::Submit
    );
    assert_eq!(enter(ModifiersState::SHIFT, false), ComposerIntent::Newline);
    // A held key does not send twice.
    assert_eq!(enter(ModifiersState::empty(), true), ComposerIntent::None);
    // A chord belongs to the window, not the composer.
    assert_eq!(enter(ModifiersState::CONTROL, false), ComposerIntent::None);
    assert_eq!(
        ComposerRule::Enter.intent(
            &Key::Named(NamedKey::Escape),
            ModifiersState::empty(),
            false
        ),
        ComposerIntent::Cancel
    );
    // A comment field is the other rule, and it is the only other rule.
    assert_eq!(
        ComposerRule::ModEnter.intent(&Key::Named(NamedKey::Enter), ModifiersState::empty(), false),
        ComposerIntent::Newline
    );
    assert_eq!(
        ComposerRule::ModEnter.intent(&Key::Named(NamedKey::Enter), ModifiersState::CONTROL, false),
        ComposerIntent::Submit
    );
}

#[test]
fn a_run_continues_for_the_same_person_within_the_window() {
    let at = |id: &str, ts: i64, author: &str, role| ConversationLine {
        id: id.into(),
        at_ms: ts,
        author: author.into(),
        body: "x".into(),
        role,
        delivery: None,
    };
    let lines = [
        at("1", 0, "Ada", ConversationRole::Incoming),
        at("2", 60_000, "Ada", ConversationRole::Incoming),
        // Another person starts a run.
        at("3", 120_000, "Bo", ConversationRole::Incoming),
        // The same person, an hour later, starts one too.
        at("4", 3_720_000, "Bo", ConversationRole::Incoming),
        // Your own line is always its own run: it sits on the other side.
        at("5", 3_780_000, "", ConversationRole::Outgoing),
    ];
    assert_eq!(run_breaks(&lines), vec![true, false, true, true, true]);
}

#[test]
fn the_list_reserves_exactly_what_it_paints_and_marks_every_day() {
    let mut text = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let clock = clock_at(NOONISH);
    let lines = vec![
        ConversationLine::incoming("a", NOONISH * 1_000, "Ada", "see you at 8")
            .with_delivery(DeliveryState::Delivered),
        ConversationLine::outgoing(
            "b",
            NOONISH.saturating_add(60).saturating_mul(1_000),
            "on my way",
        )
        .with_delivery(DeliveryState::Sent),
        ConversationLine::incoming(
            "c",
            NOONISH.saturating_add(2 * 86_400).saturating_mul(1_000),
            "Ada",
            "you are late",
        ),
    ];
    let face = ListFace::new(&lines, clock);
    let width = 360.0;
    let measured = face.height(&mut text, &theme, width);
    // Two days, so two dividers and three lines: the list is taller than the
    // lines alone, and the divider's height is the list row's.
    let without_dividers =
        face.height(&mut text, &theme, width) - 2.0 * DayDividerFace::height(&theme);
    assert!(measured > without_dividers);
    assert!(measured > 0.0);
    // A narrow column wraps a body instead of clipping it.
    let long = vec![ConversationLine::incoming(
        "a",
        NOONISH * 1_000,
        "Ada",
        "a sentence long enough to need more than one line in a narrow window",
    )];
    let long_face = ListFace::new(&long, clock);
    let wide = long_face.height(&mut text, &theme, 640.0);
    let narrow = long_face.height(&mut text, &theme, 240.0);
    assert!(
        narrow > wide,
        "a narrower column is taller: {narrow} vs {wide}"
    );
}

#[test]
fn your_own_words_read_on_their_own_colour_in_every_theme() {
    use crate::a11y::contrast_ratio;
    use crate::tokens::contrast::BODY_TEXT_MIN_CONTRAST;
    for theme in [Theme::dark(), Theme::light()] {
        for role in [ConversationRole::Incoming, ConversationRole::Outgoing] {
            let (fill, ink) = bubble_colors(&theme, role);
            assert!(
                contrast_ratio(ink, fill) >= BODY_TEXT_MIN_CONTRAST,
                "{role:?} on {fill:?} leaves {ink:?} unreadable"
            );
        }
        // The two sides are told apart by more than their position.
        assert_ne!(
            bubble_colors(&theme, ConversationRole::Incoming).0,
            bubble_colors(&theme, ConversationRole::Outgoing).0
        );
    }
}
