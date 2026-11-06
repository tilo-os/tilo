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
fn the_day_it_is_in_an_action_is_the_clock_too() {
    // `today` is where the clock falls for the person, so an action that reads it says so; a
    // filter's `today` is the query's input and asks nothing of the action that stamps a value.
    let read = "action due(m: Mail) {\n    m.day = today\n}\n";
    assert_eq!(codes(&with_mail(read)), ["E121"]);
    let declared = "action due(m: Mail) uses time {\n    m.day = today\n}\n";
    assert_eq!(codes(&with_mail(declared)), Vec::<String>::new());
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

/// A type whose fields take every draw: a number, a text, and a choice's option.
const DRAWER: &str = "type Drawer {\n    n: Int\n    mark: Text\n    ask: Option<Int>\n}\n";

fn with_drawer(rest: &str) -> String {
    format!("{DRAWER}{rest}")
}

#[test]
fn every_draw_is_the_effect_an_action_declares() {
    let bodies = [
        "action draw(d: Drawer) {\n    d.n = random.int(1, 6)\n}\n",
        "action draw(d: Drawer) {\n    d.n = random.float().to_int()\n}\n",
        "action draw(d: Drawer) {\n    d.ask = random.choice([1, 2, 3])\n}\n",
        "action draw(d: Drawer) {\n    d.n = random.shuffle([1, 2, 3]).len()\n}\n",
        "action draw(d: Drawer) {\n    d.mark = random.id()\n}\n",
    ];
    for body in bodies {
        assert_eq!(codes(&with_drawer(body)), ["E121"], "{body}");
        let declared = body.replacen(
            "action draw(d: Drawer) {",
            "action draw(d: Drawer) uses random {",
            1,
        );
        assert_eq!(
            codes(&with_drawer(&declared)),
            Vec::<String>::new(),
            "{declared}"
        );
    }
}

#[test]
fn a_draw_says_which_one_and_what_to_declare() {
    let src = format!(
        "app t.x \"X\"\n{}",
        with_drawer("action draw(d: Drawer) {\n    d.n = random.int(1, 6)\n}\n")
    );
    let errs = compile_with(&src, &[]).expect_err("`uses random` is missing");
    let message = errs.first().map(|d| d.message.clone()).unwrap_or_default();
    assert!(
        message.contains("`random.int`")
            && message.contains("a draw")
            && message.contains("uses random")
            && message.contains("`draw`"),
        "the message names the draw, what it reads and where to declare it: {message}"
    );
}

#[test]
fn a_flow_that_draws_declares_uses_random() {
    let draw = "flow tick() -> Int {\n    return random.int(1, 6)\n}\n";
    assert_eq!(codes(draw), ["E121"]);
    let declared = "flow tick() -> Int uses random {\n    return random.int(1, 6)\n}\n";
    assert_eq!(codes(declared), Vec::<String>::new());
}

#[test]
fn a_view_does_not_draw() {
    // A view is drawn again from what it is shown: a draw there would make two draws of the
    // same thing on two runnings, and nothing records either.
    let view = "view look(d: Drawer) {\n    text(\"{random.int(1, 6)}\")\n}\n";
    let found = codes(&with_drawer(view));
    assert_eq!(found, ["E121"], "a view's draw is refused");
    let src = format!(
        "app t.x \"X\"\n{}",
        with_drawer("view look(d: Drawer) {\n    text(\"{random.id()}\")\n}\n")
    );
    let errs = compile_with(&src, &[]).expect_err("a view cannot draw");
    let message = errs.first().map(|d| d.message.clone()).unwrap_or_default();
    assert!(
        message.contains("a view cannot"),
        "the message says a view cannot draw: {message}"
    );
}

#[test]
fn a_fn_does_not_draw_so_a_view_calling_it_cannot() {
    // A `fn` computes from the values it is given; a draw is written where the invocation is.
    let via_fn = "fn roll() -> Int {\n    return random.int(1, 6)\n}\n\
                  action draw(d: Drawer) {\n    d.n = roll()\n}\n";
    assert_eq!(codes(&with_drawer(via_fn)), ["E121"]);
}

#[test]
fn random_is_a_module_of_draws_and_nothing_else() {
    let unknown = "action draw(d: Drawer) uses random {\n    d.n = random.dice(6)\n}\n";
    assert_eq!(codes(&with_drawer(unknown)), ["E014"]);
    // Its list draws take a list, and a collection or a set is read as one.
    let not_a_list = "action draw(d: Drawer) uses random {\n    d.ask = random.choice(d.n)\n}\n";
    assert_eq!(codes(&with_drawer(not_a_list)), ["E020"]);
}

const BOARD: &str = "type Board {\n    feed: Text\n    count: Int\n}\n\n";

#[test]
fn a_request_declares_uses_web() {
    let read = format!(
        "{BOARD}action fetch(b: Board) {{\n    b.feed = web.get(\"https://example.com/a\").text()\n}}\n"
    );
    assert_eq!(codes(&read), ["E121"]);
    let declared = format!(
        "{BOARD}action fetch(b: Board) uses web {{\n    b.feed = web.get(\"https://example.com/a\").text()\n}}\n"
    );
    assert_eq!(codes(&declared), Vec::<String>::new());
    let posted = format!(
        "{BOARD}flow send() -> Bool uses web {{\n    return web.post(\"https://example.com/a\", body: \"x\").ok\n}}\n"
    );
    assert_eq!(codes(&posted), Vec::<String>::new());
    let undeclared = format!(
        "{BOARD}flow send() -> Bool {{\n    return web.post(\"https://example.com/a\", body: \"x\").ok\n}}\n"
    );
    assert_eq!(codes(&undeclared), ["E121"]);
}

#[test]
fn a_view_makes_no_request() {
    let view = format!(
        "{BOARD}view look(b: Board) {{\n    row(web.get(\"https://example.com/a\").text())\n}}\n"
    );
    assert_eq!(
        codes(&view),
        ["E121"],
        "a view is drawn again, never replayed: it makes no request"
    );
}

#[test]
fn a_response_has_its_fields_and_asks_the_body_as_text() {
    let fields = format!(
        "{BOARD}action fetch(b: Board) uses web {{\n    let r = web.get(\"https://example.com/a\")\n    b.feed = if r.ok {{ r.text() }} else {{ r.status_text }}\n    b.count = r.status + r.body.len() + r.headers.len()\n}}\n"
    );
    assert_eq!(codes(&fields), Vec::<String>::new());
    let missing = format!(
        "{BOARD}action fetch(b: Board) uses web {{\n    let r = web.get(\"https://example.com/a\")\n    let x = r.missing\n}}\n"
    );
    let errs = compile_with(&format!("app t.x \"X\"\n{missing}"), &[])
        .expect_err("a Response has no such field");
    let message = errs.first().map(|d| d.message.clone()).unwrap_or_default();
    assert!(
        message.contains("status_text") && message.contains("status"),
        "the message lists what a Response has: {message}"
    );
    // The answer of one run is no field: a record keeps the text it read, not the response.
    assert_eq!(
        codes(&format!("{BOARD}type Held {{\n    r: Response\n}}\n")),
        ["E023"]
    );
}

#[test]
fn the_app_header_declares_the_hosts_its_requests_reach() {
    let src = "app so.tilo.feed \"Feed\" (web: [\"example.com\", \"*.rss.org\"])\n";
    let module = parse(src).unwrap_or_else(|d| panic!("{d:#?}"));
    assert_eq!(
        module.app.web,
        ["example.com".to_owned(), "*.rss.org".to_owned()]
    );
    assert_eq!(print_module(&module), src);
    let one = "app so.tilo.feed \"Feed\" (web: \"example.com\")\n";
    let module = parse(one).unwrap_or_else(|d| panic!("{d:#?}"));
    assert_eq!(module.app.web, ["example.com".to_owned()]);
    let unknown = "app so.tilo.feed \"Feed\" (net: [\"example.com\"])\n";
    let errs = compile_with(unknown, &[]).expect_err("only `web` grants hosts");
    let message = errs.first().map(|d| d.message.clone()).unwrap_or_default();
    assert!(
        message.contains("web") && message.contains("net"),
        "the message names the option: {message}"
    );
}

#[test]
fn the_manifest_keeps_the_hosts_the_header_declares() {
    let src = "app so.tilo.feed \"Feed\" (web: [\"example.com\", \"*.rss.org\"])\n";
    let compiled = compile_with(src, &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    assert_eq!(compiled.meta.web, ["example.com", "*.rss.org"]);
    let json = serde_json::to_string(&compiled.meta).unwrap();
    let back: tilolang::PackageMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back.web, compiled.meta.web,
        "the grant travels with the manifest"
    );
    let none = compile_with("app t.x \"X\"\n", &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    assert!(none.meta.web.is_empty(), "no `web:` grants no host");
}
