//! `uses web, random, time`: what a declaration says its body reads that a replay cannot compute
//! again, and what the checker makes of a body that reads one without saying so.

use crate::common::{codes, compile_with};
use tilolang::{parse, print_module};

const MAIL: &str = "type Mail {\n    at: Time\n    day: Date\n    hold: Int\n}\n";

fn with_mail(rest: &str) -> String {
    format!("{MAIL}{rest}")
}

tilo_test_env::scenario! {
    /// `uses time`: who reads the clock, and the `uses` clause itself.
    fn reading_the_clock_is_declared_with_uses_time;

    phase an_action_reading_the_clock_declares_uses_time;
    {
        let read = "action send(m: Mail) {\n    m.at = now\n}\n";
        assert_eq!(codes(&with_mail(read)), ["E121"]);
        let declared = "action send(m: Mail) uses time {\n    m.at = now\n}\n";
        assert_eq!(codes(&with_mail(declared)), Vec::<String>::new());
    }

    phase the_error_names_the_effect_the_declaration_is_missing;
    {
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

    phase the_day_it_is_in_an_action_is_the_clock_too;
    {
        // `today` is where the clock falls for the person, so an action that reads it says so; a
        // filter's `today` is the query's input and asks nothing of the action that stamps a value.
        let read = "action due(m: Mail) {\n    m.day = today\n}\n";
        assert_eq!(codes(&with_mail(read)), ["E121"]);
        let declared = "action due(m: Mail) uses time {\n    m.day = today\n}\n";
        assert_eq!(codes(&with_mail(declared)), Vec::<String>::new());
    }

    phase a_time_of_no_zone_and_a_phrase_read_in_the_zone_are_time;
    {
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

    phase a_fn_reads_what_it_is_given_so_the_action_that_calls_it_says_what_it_uses;
    {
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

    phase a_view_reads_the_zone_and_has_no_now;
    {
        let view = "view look(m: Mail) {\n    text(\"{m.at.day()}\")\n}\n";
        assert_eq!(
            codes(&with_mail(view)),
            Vec::<String>::new(),
            "the person's zone is an input of what a view draws, not an effect"
        );
        let clock = "view look(m: Mail) {\n    text(\"{now}\")\n}\n";
        assert_eq!(codes(&with_mail(clock)), ["E049"]);
        // A moment's day in a named zone is the same everywhere: no clock and no host zone, so a
        // view draws it and an action reads it without declaring `uses time`.
        let zoned = "view look(m: Mail) {\n    text(\"{m.at.day_in(\\\"Europe/Berlin\\\")}\")\n}\n";
        assert_eq!(codes(&with_mail(zoned)), Vec::<String>::new());
        let in_action =
            "action stamp(m: Mail) {\n    m.day = m.at.day_in(\"Europe/Berlin\")\n}\n";
        assert_eq!(codes(&with_mail(in_action)), Vec::<String>::new());
        let wall = "action stamp(m: Mail) {\n    m.at = Time.from_wall(\"Europe/Berlin\", \"2026-11-24T09:00\") ?? now\n}\n";
        // `from_wall` names the zone its text is read in, so the read is no effect either.
        assert!(
            codes(&with_mail(wall)).iter().all(|c| c == "E121"),
            "`?? now` still reads the clock"
        );
    }

    phase an_effect_word_is_one_of_web_random_and_time;
    {
        let unknown = "action send(m: Mail) uses net {\n    m.at = now\n}\n";
        assert_eq!(codes(&with_mail(unknown)), ["E121"]);
        let twice = "action send(m: Mail) uses time, time {\n    m.at = now\n}\n";
        assert_eq!(codes(&with_mail(twice)), ["E121"]);
        let all = "action send(m: Mail) uses web, random, time {\n    m.at = now\n}\n";
        assert_eq!(codes(&with_mail(all)), Vec::<String>::new());
    }

    phase a_scenario_reads_every_effect_and_declares_none;
    {
        // Seed data runs where a developer runs it, and is never replayed.
        let seed = "scenario \"seed\" {\n    let m = Mail(at: now, hold: 0)\n}\n";
        assert_eq!(codes(&with_mail(seed)), Vec::<String>::new());
    }

    phase a_filter_searches_by_the_day_it_is;
    {
        // The day is the query's input, read where the view is drawn: nothing is replayed, so no
        // action declares it.
        let search = "type Message {\n    subject: Text\n    received: Time\n    due: Date\n    search {\n        filter \"Today\" = due <= today\n    }\n}\n\
                  action touch(m: Message) {\n    m.received = now\n}\n";
        assert_eq!(codes(search), ["E121"], "the action's own `now` is its");
        let declared = search.replacen(
            "action touch(m: Message) {",
            "action touch(m: Message) uses time {",
            1,
        );
        assert_eq!(codes(&declared), Vec::<String>::new());
    }

    phase the_clause_keeps_its_place_in_the_canonical_form;
    {
        let src = "app so.tilo.demo \"Demo\"\n\n\
               type Mail {\n    at: Time\n}\n\n\
               primary action send(m: Mail) -> Mail uses web, time (icon: .send) {\n    m.at = now\n    return m\n}\n\n\
               action tick(m: Mail) uses time {\n    m.at = now\n}\n\n\
               view compose(m: Mail) {\n    ui.Text(\"{m.at}\")\n}\n";
        let module = parse(src).unwrap_or_else(|d| panic!("{d:#?}"));
        assert_eq!(print_module(&module), src);
    }
}

/// A type whose fields take every draw: a number, a text, and a choice's option.
const DRAWER: &str = "type Drawer {\n    n: Int\n    mark: Text\n    ask: Option<Int>\n}\n";

fn with_drawer(rest: &str) -> String {
    format!("{DRAWER}{rest}")
}

tilo_test_env::scenario! {
    /// `uses random`: the draws and who may make one.
    fn a_draw_is_declared_with_uses_random;

    phase every_draw_is_the_effect_an_action_declares;
    {
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

    phase a_draw_says_which_one_and_what_to_declare;
    {
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

    phase an_action_that_draws_declares_uses_random;
    {
        let draw = "action tick() -> Int {\n    return random.int(1, 6)\n}\n";
        assert_eq!(codes(draw), ["E121"]);
        let declared = "action tick() -> Int uses random {\n    return random.int(1, 6)\n}\n";
        assert_eq!(codes(declared), Vec::<String>::new());
    }

    phase a_view_does_not_draw;
    {
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

    phase a_fn_does_not_draw_so_a_view_calling_it_cannot;
    {
        // A `fn` computes from the values it is given; a draw is written where the invocation is.
        let via_fn = "fn roll() -> Int {\n    return random.int(1, 6)\n}\n\
                  action draw(d: Drawer) {\n    d.n = roll()\n}\n";
        assert_eq!(codes(&with_drawer(via_fn)), ["E121"]);
    }

    phase random_is_a_module_of_draws_and_nothing_else;
    {
        let unknown = "action draw(d: Drawer) uses random {\n    d.n = random.dice(6)\n}\n";
        assert_eq!(codes(&with_drawer(unknown)), ["E014"]);
        // Its list draws take a list, and a collection or a set is read as one.
        let not_a_list = "action draw(d: Drawer) uses random {\n    d.ask = random.choice(d.n)\n}\n";
        assert_eq!(codes(&with_drawer(not_a_list)), ["E020"]);
    }
}

const BOARD: &str = "type Board {\n    feed: Text\n    count: Int\n}\n\n";

tilo_test_env::scenario! {
    /// `uses web`: requests, responses and the hosts the header declares.
    fn a_request_is_declared_with_uses_web;

    phase a_request_declares_uses_web;
    {
        let read = format!(
            "{BOARD}action fetch(b: Board) {{\n    b.feed = web.get(\"https://example.com/a\").text()\n}}\n"
        );
        assert_eq!(codes(&read), ["E121"]);
        let declared = format!(
            "{BOARD}action fetch(b: Board) uses web {{\n    b.feed = web.get(\"https://example.com/a\").text()\n}}\n"
        );
        assert_eq!(codes(&declared), Vec::<String>::new());
        let posted = format!(
            "{BOARD}action send() -> Bool uses web {{\n    return web.post(\"https://example.com/a\", body: \"x\").ok\n}}\n"
        );
        assert_eq!(codes(&posted), Vec::<String>::new());
        let undeclared = format!(
            "{BOARD}action send() -> Bool {{\n    return web.post(\"https://example.com/a\", body: \"x\").ok\n}}\n"
        );
        assert_eq!(codes(&undeclared), ["E121"]);
    }

    phase a_view_makes_no_request;
    {
        let view = format!(
            "{BOARD}view look(b: Board) {{\n    row(web.get(\"https://example.com/a\").text())\n}}\n"
        );
        assert_eq!(
            codes(&view),
            ["E121"],
            "a view is drawn again, never replayed: it makes no request"
        );
    }

    phase a_response_has_its_fields_and_asks_the_body_as_text;
    {
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

    phase the_app_header_declares_the_hosts_its_requests_reach;
    {
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

    phase the_manifest_keeps_the_hosts_the_header_declares;
    {
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
}

/// A note with a person to tell: what a notice is about.
const NOTE: &str = "type Note {\n    title: Text\n    who: Option<Actor> = .none\n}\n";

fn telling(body: &str) -> Vec<String> {
    codes(&format!("{NOTE}{body}"))
}

const TELL: &str = "notify(to: invoker, title: \"It is time\", body: \"Anything\", opens: n)";

tilo_test_env::scenario! {
    /// `uses notify`: telling a person, and what a notice carries.
    fn telling_a_person_is_declared_with_uses_notify;

    phase an_action_that_tells_a_person_declares_uses_notify;
    {
        let read = format!("action tell(n: Note) {{\n    {TELL}\n}}\n");
        assert_eq!(telling(&read), ["E121"]);
        let declared = "action tell(n: Note) uses notify {\n    ".to_owned() + TELL + "\n}\n";
        assert_eq!(telling(&declared), Vec::<String>::new());
    }

    phase the_error_names_telling_a_person_and_what_to_declare;
    {
        let src = format!("app t.x \"X\"\n{NOTE}action tell(n: Note) {{\n    {TELL}\n}}\n");
        let errs = compile_with(&src, &[]).expect_err("`uses notify` is missing");
        let message = errs.first().map(|d| d.message.clone()).unwrap_or_default();
        assert!(
            message.contains("telling a person something")
                && message.contains("uses notify")
                && message.contains("`tell`"),
            "the message says what it reads and where to declare it: {message}"
        );
    }

    phase a_view_and_a_fn_do_not_tell_a_person;
    {
        // A view is drawn again from what it is shown, and a notice is not drawn: telling is an
        // action's, recorded in its change as every other outside read is.
        let view = format!("view look(n: Note) {{\n    {TELL}\n}}\n");
        let found = telling(&view);
        assert_eq!(found, ["E060"], "a view cannot tell a person");
        let body = format!("fn tell_it(n: Note) -> Bool {{\n    {TELL}\n    true\n}}\n");
        assert_eq!(telling(&body), ["E049"], "a fn cannot tell a person");
    }

    phase a_notice_is_written_with_what_it_says_and_what_it_opens;
    {
        let asking =
            "action tell(n: Note) uses notify {\n    notify(to: invoker, title: \"It is time\")\n}\n";
        assert_eq!(telling(asking), ["E022"], "every part of a notice is named");
        let unnamed =
            "action tell(n: Note) uses notify {\n    notify(invoker, \"It is time\", \"\", n)\n}\n";
        let found = telling(unnamed);
        assert!(
            !found.is_empty() && found.iter().all(|c| c == "E022"),
            "a notice is named, not positional: {found:?}"
        );
        let unknown = "action tell(n: Note) uses notify {\n    notify(to: invoker, title: \"t\", body: \"b\", opens: n, tone: .loud)\n}\n";
        assert!(
            telling(unknown).contains(&"E022".to_owned()),
            "a notice has no other part"
        );
        let opens_nothing = "action tell(n: Note) uses notify {\n    notify(to: invoker, title: \"t\", body: \"b\", opens: \"n\")\n}\n";
        assert!(
            telling(opens_nothing).contains(&"E022".to_owned()),
            "what a notice opens is a resource, not words"
        );
        let all = "action tell(n: Note) uses notify, time {\n    notify(to: invoker, title: n.title, body: \"Then\", opens: n)\n}\n";
        assert_eq!(telling(all), Vec::<String>::new());
    }
}

tilo_test_env::scenario! {
    /// `uses clipboard`: copying reaches the person's device and a paste reads what is on it now,
    /// so neither is written without saying so, and neither is a view's.
    fn the_clipboard_is_declared_with_uses_clipboard;

    phase an_action_that_copies_or_pastes_declares_uses_clipboard;
    {
        let copy = "type N {\n    t: Text\n}\naction keep(n: N) {\n    clipboard.copy(n.t)\n}\n";
        assert_eq!(codes(copy), ["E121"]);
        let declared =
            "type N {\n    t: Text\n}\naction keep(n: N) uses clipboard {\n    clipboard.copy(n.t)\n}\n";
        assert_eq!(codes(declared), Vec::<String>::new());
    }

    phase a_paste_answers_an_option_of_what_it_holds;
    {
        let text = "type N {\n    t: Text\n}\naction read(n: N) uses clipboard {\n    n.t = clipboard.paste() ?? n.t\n}\n";
        assert_eq!(codes(text), Vec::<String>::new());
        let image = "type N {\n    t: Text\n}\naction read(n: N) uses clipboard {\n    let f = clipboard.paste_image()\n    if let chosen = f {\n        n.t = chosen.name\n    }\n}\n";
        assert_eq!(codes(image), Vec::<String>::new());
        // What it answers is an `Option`, so it is not read as the value itself.
        let unguarded =
            "type N {\n    t: Text\n}\naction read(n: N) uses clipboard {\n    n.t = clipboard.paste()\n}\n";
        assert_eq!(codes(unguarded), ["E020"]);
    }

    phase a_copy_takes_its_value_by_position_and_an_image_is_a_file;
    {
        let named = "type N {\n    t: Text\n}\naction keep(n: N) uses clipboard {\n    clipboard.copy(text: n.t)\n}\n";
        assert!(codes(named).contains(&"E022".to_owned()), "a copy takes the value by position");
        let not_an_image = "type N {\n    t: Text\n}\naction keep(n: N) uses clipboard {\n    clipboard.copy_image(n.t)\n}\n";
        assert_eq!(codes(not_an_image), ["E020"], "an image is a file, not text");
    }

    phase a_view_cannot_reach_the_clipboard;
    {
        let view = "type N {\n    t: Text\n}\nview look(n: N) {\n    text(clipboard.paste() ?? n.t)\n}\n";
        assert_eq!(codes(view), ["E121"], "a view is drawn again from what it is shown");
    }
}

tilo_test_env::scenario! {
    /// `uses image`: an image's pixels are the device's to decode, encode and preview, so the
    /// action says so, and a view works on no pixels.
    fn an_images_pixels_are_declared_with_uses_image;

    phase an_action_that_reads_or_makes_pixels_declares_uses_image;
    {
        let read = "type N {\n    t: Text\n}\naction look(n: N, f: File) {\n    if let info = f.image_info() {\n        n.t = \"{info.width}\"\n    }\n}\n";
        assert_eq!(codes(read), ["E121"]);
        let declared = "type N {\n    t: Text\n}\naction look(n: N, f: File) uses image {\n    if let info = f.image_info() {\n        n.t = \"{info.width}\"\n    }\n}\n";
        assert_eq!(codes(declared), Vec::<String>::new());

        let crop = "type N {\n    t: Text\n}\naction cut(n: N, f: File) {\n    let out = f.cropped(0.0, 0.0, 0.5, 1.0)\n}\n";
        assert_eq!(codes(crop), ["E121"]);
        let turn = "type N {\n    t: Text\n}\naction spin(n: N, f: File) {\n    let out = f.turned(1)\n}\n";
        assert_eq!(codes(turn), ["E121"]);
        let resize = "type N {\n    t: Text\n}\naction fit(n: N, f: File) {\n    let out = f.resized(2, 2)\n}\n";
        assert_eq!(codes(resize), ["E121"]);
        let encode = "type N {\n    t: Text\n}\naction write(n: N, f: File) {\n    let out = f.encoded(format: \"png\")\n}\n";
        assert_eq!(codes(encode), ["E121"]);
        let thumbnail = "type N {\n    t: Text\n}\naction preview(n: N, f: File) {\n    let out = f.thumbnail(64)\n}\n";
        assert_eq!(codes(thumbnail), ["E121"]);
    }

    phase a_view_works_on_no_pixels;
    {
        let view = "type N {\n    t: Text\n    f: Option<File> = .none\n}\nview look(n: N) {\n    if let file = n.f {\n        let info = file.image_info()\n    }\n}\n";
        assert_eq!(codes(view), ["E121"], "a view is drawn again from what it is shown");
        let preview = "type N {\n    t: Text\n    f: Option<File> = .none\n}\nview look(n: N) {\n    if let file = n.f {\n        let thumb = file.thumbnail(64)\n    }\n}\n";
        assert_eq!(codes(preview), ["E121"]);
    }

    phase the_operations_name_what_they_take;
    {
        let not_four = "type N {\n    t: Text\n}\naction cut(n: N, f: File) uses image {\n    let out = f.cropped(0.0, 0.0)\n}\n";
        assert_eq!(codes(not_four), ["E022"]);
        let unnamed = "type N {\n    t: Text\n}\naction write(n: N, f: File) uses image {\n    let out = f.encoded(png: \"png\")\n}\n";
        assert_eq!(codes(unnamed), ["E022"]);
    }
}

tilo_test_env::scenario! {
    /// A file is the person's to choose: `file.choose` fills one, or the several a `List<File>`
    /// takes, and `file.choose_folder` fills the files of a folder. Both stand on a control.
    fn a_chooser_fills_a_file_or_the_files_of_a_folder;

    phase a_list_of_files_and_a_folder_are_filled_by_a_chooser;
    {
        let many = "type N {\n    t: Text\n}\naction take(n: N, files: List<File>) {\n    n.t = \"{files.len()}\"\n}\nview look(n: N) {\n    stack {\n        ui.Button(\"Choose\") {\n            on press: file.choose(take(n))\n        }\n    }\n}\n";
        assert_eq!(codes(many), Vec::<String>::new());
        let folder = "type N {\n    t: Text\n}\naction take(n: N, files: List<File>) {\n    n.t = \"{files.len()}\"\n}\nview look(n: N) {\n    stack {\n        ui.Button(\"Choose\") {\n            on press: file.choose_folder(take(n))\n        }\n    }\n}\n";
        assert_eq!(codes(folder), Vec::<String>::new());
        let one = "type N {\n    t: Text\n}\naction take(n: N, f: File) {\n    n.t = f.name\n}\nview look(n: N) {\n    stack {\n        ui.Button(\"Choose\") {\n            on press: file.choose(take(n))\n        }\n    }\n}\n";
        assert_eq!(codes(one), Vec::<String>::new());
    }

    phase a_parameter_a_chooser_cannot_fill_says_so;
    {
        let text = "type N {\n    t: Text\n}\naction take(n: N, words: List<Text>) {\n    n.t = \"{words.len()}\"\n}\nview look(n: N) {\n    stack {\n        ui.Button(\"Choose\") {\n            on press: file.choose(take(n))\n        }\n    }\n}\n";
        let got = codes(text);
        assert!(
            got.iter().any(|code| code == "E022"),
            "a chooser fills no list of text: {got:?}"
        );
    }

    phase a_chooser_stands_on_a_control_not_in_an_action;
    {
        let src = "type N {\n    t: Text\n}\naction take(n: N, files: List<File>) {\n    n.t = \"x\"\n}\naction go(n: N) {\n    let a = file.choose(take(n))\n}\n";
        assert_eq!(codes(src), ["E067"]);
    }
}

tilo_test_env::scenario! {
    /// `uses files`: an action reads the device's folders — a listing, one entry, a path's bytes
    /// and a watch that runs an action — and a view reads none of them.
    fn reading_the_devices_folders_is_declared_with_uses_files;

    phase an_action_reading_the_disk_declares_uses_files;
    {
        let listed = "type N {\n    body: RichText\n}\naction scan(n: N) uses files {\n    match File.list(\"~/Books\") {\n        .ok(entries) => { for e in entries { n.body = RichText(\"{e.name} {e.path} {e.size} {e.modified} {e.folder} {e.hidden}\") } },\n        .err(why) => { n.body = RichText(why) }\n    }\n}\n";
        assert_eq!(codes(listed), Vec::<String>::new());
        let undeclared = listed.replace(" uses files", "");
        assert_eq!(codes(&undeclared), ["E121"], "the disk is an outside read");
    }

    phase one_entry_and_a_paths_bytes_are_read_through_the_result;
    {
        let stat = "type N {\n    body: RichText\n}\naction one(n: N) uses files {\n    match File.stat(\"~/Books/a.epub\") {\n        .ok(found) => { if let e = found { n.body = RichText(\"{e.size} {e.modified}\" ) } },\n        .err(why) => { n.body = RichText(why) }\n    }\n}\n";
        assert_eq!(codes(stat), Vec::<String>::new());
        let read = "type N {\n    body: RichText\n}\naction bytes(n: N) uses files {\n    match File.read(\"~/Books/a.epub\") {\n        .ok(b) => { n.body = RichText(\"{b.len()}\") },\n        .err(why) => { n.body = RichText(why) }\n    }\n}\n";
        assert_eq!(codes(read), Vec::<String>::new());
        let not_bytes = "type N {\n    body: RichText\n}\naction bytes(n: N) uses files {\n    let b = File.read(\"~/Books/a.epub\")\n    n.body = RichText(\"{b.len()}\")\n}\n";
        assert_eq!(codes(not_bytes), ["E027"], "what it answers is a Result, not the bytes");
    }

    phase a_watch_names_one_of_the_packages_actions;
    {
        let watched = "type N {\n    body: RichText\n}\naction scan(n: N) uses files {\n    n.body = RichText(\"\")\n}\naction again(n: N) uses files {\n    File.watch(\"~/Books\", \"scan\")\n}\n";
        assert_eq!(codes(watched), Vec::<String>::new());
        let named = "type N {\n    body: RichText\n}\naction again(n: N) uses files {\n    File.watch(path: \"~/Books\", run: \"scan\")\n}\n";
        assert!(codes(named).contains(&"E022".to_owned()), "a watch takes its path and action by position");
    }

    phase a_write_to_the_disk_declares_the_same_use;
    {
        let made = "type N {\n    body: RichText\n}\naction make(n: N) uses files {\n    match File.mkdir(\"~/Books\") {\n        .ok(e) => { n.body = RichText(e.name) },\n        .err(why) => { n.body = RichText(why) }\n    }\n}\n";
        assert_eq!(codes(made), Vec::<String>::new());
        let undeclared = made.replace(" uses files", "");
        assert_eq!(codes(&undeclared), ["E121"], "the disk is an outside read");
        let trashed = "type N {\n    body: RichText\n}\naction drop(n: N) uses files {\n    match File.trash_entries() {\n        .ok(items) => { n.body = RichText(\"{items.len()}\") },\n        .err(why) => { n.body = RichText(why) }\n    }\n}\n";
        assert_eq!(codes(trashed), Vec::<String>::new());
    }

    phase a_file_opens_in_the_app_that_opens_its_kind;
    {
        let path = "type N {\n    body: RichText\n}\naction open_at(n: N) uses files {\n    match File.open(\"~/Books/plan.md\") {\n        .ok => { n.body = RichText(\"opened\") },\n        .err(why) => { n.body = RichText(why) }\n    }\n}\n";
        assert_eq!(codes(path), Vec::<String>::new());
        let held = "type N {\n    body: RichText\n}\naction open_it(n: N, file: File) uses files {\n    match File.open(file) {\n        .ok => { n.body = RichText(\"opened\") },\n        .err(why) => { n.body = RichText(why) }\n    }\n}\n";
        assert_eq!(codes(held), Vec::<String>::new());
        let undeclared = held.replace(" uses files", "");
        assert_eq!(codes(&undeclared), ["E121"], "the open is an outside read");
        let wrong = "type N {\n    body: RichText\n}\naction open_it(n: N) uses files {\n    File.open(3)\n}\n";
        assert!(
            codes(wrong).contains(&"E020".to_owned()),
            "an open takes a path or a file, not a number"
        );
    }

    phase a_view_opens_no_file;
    {
        let view = "type N {\n    body: RichText\n}\nview look(n: N) {\n    match File.open(\"~/Books/plan.md\") {\n        .ok => { text(\"opened\") },\n        .err(why) => { text(why) }\n    }\n}\n";
        assert_eq!(codes(view), ["E121"], "a view is drawn again from what it is shown");
    }

    phase a_view_reads_none_of_the_disk;
    {
        let view = "type N {\n    body: RichText\n}\nview look(n: N) {\n    match File.list(\"~/Books\") {\n        .ok(entries) => { text(\"{entries.len()}\") },\n        .err(why) => { text(why) }\n    }\n}\n";
        assert_eq!(codes(view), ["E121"], "a view is drawn again from what it is shown");
    }
}

tilo_test_env::scenario! {
    /// `engine.start(op, input, done: j -> act(r, j))`: what a job runs when it ends runs later, as
    /// its own call on the device that holds the job, so it is one of the app's actions handed the
    /// job as it is, never a computation the starting action would have to be there for.
    fn what_a_job_runs_when_it_moves_is_an_action_handed_the_job;

    phase an_action_handed_the_job_is_accepted;
    {
        let src = "type P {\n    n: Int\n    job: Text\n}\naction fin(p: P, j: host.Job) {\n    p.n = j.done\n}\naction go(p: P) {\n    match engine.start(\"x\", \"\", done: j -> fin(p, j), progress: j -> fin(p, j)) {\n        .ok(job) => {\n            p.job = job\n        }\n        .err(why) => {\n            p.job = why\n        }\n    }\n}\n";
        assert_eq!(codes(src), Vec::<String>::new());
    }

    phase a_computation_over_the_job_is_refused;
    {
        let src = "type P {\n    n: Int\n    job: Text\n}\naction keep(p: P, n: Int) {\n    p.n = n\n}\naction go(p: P) {\n    match engine.start(\"x\", \"\", done: j -> keep(p, j.done)) {\n        .ok(job) => {\n            p.job = job\n        }\n        .err(why) => {\n            p.job = why\n        }\n    }\n}\n";
        assert_eq!(codes(src), ["E063"]);
    }

    phase a_handler_that_is_no_action_or_no_function_of_the_job_is_refused;
    {
        let not_a_lambda = "type P {\n    n: Int\n    job: Text\n}\naction fin(p: P) {\n    p.n = 1\n}\naction go(p: P) {\n    match engine.start(\"x\", \"\", done: fin(p)) {\n        .ok(job) => {\n            p.job = job\n        }\n        .err(why) => {\n            p.job = why\n        }\n    }\n}\n";
        assert_eq!(codes(not_a_lambda), ["E063"]);
        let a_function = "type P {\n    n: Int\n    job: Text\n}\nfn f(p: P, j: host.Job) -> Int {\n    return j.done\n}\naction go(p: P) {\n    match engine.start(\"x\", \"\", done: j -> f(p, j)) {\n        .ok(job) => {\n            p.job = job\n        }\n        .err(why) => {\n            p.job = why\n        }\n    }\n}\n";
        assert_eq!(codes(a_function), ["E063"]);
    }

    phase a_jobs_progress_is_read_not_asked;
    {
        let status = "type P {\n    job: Text\n}\naction go(p: P) {\n    match engine.status(p.job) {\n        .ok(said) => {\n            p.job = said\n        }\n        .err(why) => {\n            p.job = why\n        }\n    }\n}\n";
        assert_eq!(codes(status), ["E022"], "a job is read, never polled");
        let read = "type P {\n    job: Text\n}\nview v(p: P) {\n    if let j = engine.job(p.job) {\n        text(\"{j.done} of {j.total}\")\n    }\n}\n";
        assert_eq!(codes(read), Vec::<String>::new(), "a view reads a job, and follows it");
        let named = "type P {\n    job: Text\n}\naction go(p: P) {\n    match engine.start(\"x\", \"\", finished: p) {\n        .ok(job) => {\n            p.job = job\n        }\n        .err(why) => {\n            p.job = why\n        }\n    }\n}\n";
        assert_eq!(codes(named), ["E022"]);
    }
}

tilo_test_env::scenario! {
    /// `uses shell`: a command a shell runs changes the machine, where no Undo reaches, so a
    /// destructive action that opens one reaches outside and its person is asked before an
    /// agent's runs; opening a shell to show it changes only the session.
    fn a_destructive_action_that_opens_a_shell_reaches_outside;

    phase each_shell_action_is_classed_by_what_it_does;
    {
        let src = "type S {\n    shell: Text\n}\n\
            action show(s: S) uses shell {\n    s.shell = \"open\"\n}\n\
            destructive action run(s: S, command: Text) uses shell {\n    s.shell = command\n}\n\
            destructive action again(s: S) {\n    run(s, \"again\")\n}\n\
            destructive action clear(s: S) {\n    s.shell = \"\"\n}\n";
        assert_eq!(codes(src), Vec::<String>::new());
        let compiled = crate::common::compile_body(src);
        let class = |name: &str| {
            compiled
                .meta
                .actions
                .iter()
                .find(|action| action.name == name)
                .unwrap_or_else(|| panic!("the action {name}"))
                .class
        };
        use tilolang::types::EffectClass;
        assert_eq!(class("run"), EffectClass::Outside, "a command runs on the machine");
        assert_eq!(class("again"), EffectClass::Outside, "it runs one through `run`");
        assert_eq!(class("show"), EffectClass::Undoable, "opening a shell changes the session");
        assert_eq!(class("clear"), EffectClass::Undoable, "no shell, nothing outside");
    }
}
