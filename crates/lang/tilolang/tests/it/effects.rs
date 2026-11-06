//! `uses web, random, time`: what a declaration says its body reads that a replay cannot compute
//! again, and what the checker makes of a body that reads one without saying so.

use crate::common::{codes, compile_with};
use tilolang::{parse, print_module};

const MAIL: &str = "type Mail {\n    at: Time\n    day: Date\n    hold: Int\n}\n";

fn with_mail(rest: &str) -> String {
    format!("{MAIL}{rest}")
}

#[test]
fn an_action_reading_the_clock_declares_uses_time() {
    let read = "action send(m: Mail) {\n    m.at = now\n}\n";
    assert_eq!(codes(&with_mail(read)), ["E121"]);
    let declared = "action send(m: Mail) uses time {\n    m.at = now\n}\n";
    assert_eq!(codes(&with_mail(declared)), Vec::<String>::new());
}

#[test]
fn the_error_names_the_effect_the_declaration_is_missing() {
    let src = format!(
        "app t.x \"X\"\n{}",
        with_mail("action send(m: Mail) {\n    m.at = now\n}\n")
    );
    let errs = compile_with(&src, &[]).expect_err("`uses time` is missing");
    let message = errs.first().map(|d| d.message.clone()).unwrap_or_default();
    assert!(
        message.contains("uses time") && message.contains("`send`"),
        "the message says what to declare and where: {message}"
    );
}

#[test]
fn a_flow_reading_the_clock_declares_uses_time() {
    let read = "flow tick() -> Time {\n    return now\n}\n";
    assert_eq!(codes(read), ["E121"]);
    let declared = "flow tick() -> Time uses time {\n    return now\n}\n";
    assert_eq!(codes(declared), Vec::<String>::new());
}

#[test]
fn a_time_of_no_zone_and_a_phrase_read_in_the_zone_are_time() {
    let wall = "action stamp(m: Mail) {\n    m.at = 2026-06-30T09:00\n}\n";
    assert_eq!(codes(&with_mail(wall)), ["E121"]);
    let phrase =
        "action stamp(m: Mail, text: Text) {\n    m.at = Time.parse(text, near: now) ?? now\n}\n";
    let got = codes(&with_mail(phrase));
    assert!(
        !got.is_empty() && got.iter().all(|c| c == "E121"),
        "each read of the clock or the zone in the action says so: {got:?}"
    );
    let day = "action stamp(m: Mail) {\n    m.day = now.day()\n}\n";
    assert_eq!(codes(&with_mail(day)), ["E121"]);
}

#[test]
fn a_fn_reads_what_it_is_given_so_the_action_that_calls_it_says_what_it_uses() {
    // A `fn` computes values, so the read is the invocation's, and the action that inlines it
    // is what declares it.
    let via_fn = "fn day_of_it(at: Time) -> Date {\n    return at.day()\n}\n\
                  action stamp(m: Mail) {\n    m.day = day_of_it(m.at)\n}\n";
    assert_eq!(codes(&with_mail(via_fn)), ["E121"]);
    let declared = via_fn.replacen(
        "action stamp(m: Mail) {",
        "action stamp(m: Mail) uses time {",
        1,
    );
    assert_eq!(codes(&with_mail(&declared)), Vec::<String>::new());
}

#[test]
fn a_view_reads_the_zone_and_has_no_now() {
    let view = "view look(m: Mail) {\n    text(\"{m.at.day()}\")\n}\n";
    assert_eq!(
        codes(&with_mail(view)),
        Vec::<String>::new(),
        "the person's zone is an input of what a view draws, not an effect"
    );
    let clock = "view look(m: Mail) {\n    text(\"{now}\")\n}\n";
    assert_eq!(codes(&with_mail(clock)), ["E049"]);
}

#[test]
fn an_effect_word_is_one_of_web_random_and_time() {
    let unknown = "action send(m: Mail) uses net {\n    m.at = now\n}\n";
    assert_eq!(codes(&with_mail(unknown)), ["E121"]);
    let twice = "action send(m: Mail) uses time, time {\n    m.at = now\n}\n";
    assert_eq!(codes(&with_mail(twice)), ["E121"]);
    let all = "action send(m: Mail) uses web, random, time {\n    m.at = now\n}\n";
    assert_eq!(codes(&with_mail(all)), Vec::<String>::new());
}

#[test]
fn a_scenario_reads_every_effect_and_declares_none() {
    // Seed data runs where a developer runs it, and is never replayed.
    let seed = "scenario \"seed\" {\n    let m = Mail(at: now, hold: 0)\n}\n";
    assert_eq!(codes(&with_mail(seed)), Vec::<String>::new());
}

#[test]
fn a_filter_searches_by_the_day_it_is() {
    // The day is the query's input, read where the view is drawn: nothing is replayed, so no
    // action declares it.
    let search = "type Message {\n    subject: Text (title)\n    received: Time\n    due: Date\n    search {\n        filter \"Today\" = due <= today\n    }\n}\n\
                  action touch(m: Message) {\n    m.received = now\n}\n";
    assert_eq!(codes(search), ["E121"], "the action's own `now` is its");
    let declared = search.replacen(
        "action touch(m: Message) {",
        "action touch(m: Message) uses time {",
        1,
    );
    assert_eq!(codes(&declared), Vec::<String>::new());
}

#[test]
fn the_clause_keeps_its_place_in_the_canonical_form() {
    let src = "app so.tilo.demo \"Demo\"\n\n\
               type Mail {\n    at: Time\n}\n\n\
               primary action send(m: Mail) -> Mail uses web, time proposal compose(m) (icon: .send) {\n    m.at = now\n    return m\n}\n\n\
               flow tick(m: Mail) uses time {\n    m.at = now\n}\n\n\
               view compose(m: Mail) {\n    ui.Text(\"{m.at}\")\n}\n";
    let module = parse(src).unwrap_or_else(|d| panic!("{d:#?}"));
    assert_eq!(print_module(&module), src);
}
