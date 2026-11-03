//! What the printer does with text that runs past its width: a signature, a call, a chain and a
//! condition break over their lines, the way rustfmt writes this repository's Rust.

use tilolang::{parse, print_module};

/// The canonical text of `src`, panicking on a syntax error.
fn print_of(src: &str) -> String {
    print_module(&parse(src).unwrap_or_else(|d| panic!("{d:#?}")))
}

/// The canonical text of `src` is `want`, and printing it again gives the same text.
fn prints_as(src: &str, want: &str) {
    let once = print_of(src);
    assert_eq!(once, want);
    assert_eq!(
        print_of(&once),
        once,
        "the canonical text is what it prints"
    );
}

#[test]
fn a_long_signature_puts_one_parameter_per_line() {
    let src = "app t.x \"X\"\n\nfn tally(entries: List<Entry>, by: Text = \"day\", until: Option<Date> = .none, empty: Text = \"Nothing yet\", note: Text = \"Counted by the day\") -> Int {\n    return entries.count(e -> true)\n}\n";
    prints_as(
        src,
        "app t.x \"X\"\n\nfn tally(\n    entries: List<Entry>,\n    by: Text = \"day\",\n    until: Option<Date> = .none,\n    empty: Text = \"Nothing yet\",\n    note: Text = \"Counted by the day\",\n) -> Int {\n    return entries.count(e -> true)\n}\n",
    );
}

#[test]
fn a_long_view_breaks_only_the_part_too_wide_for_the_page() {
    let src = "app t.x \"X\"\n\nview rows<T>(items: List<T>, empty: Text, primary: Option<host.Action>, icon: Option<Text>, row: (T) -> View) {\n    text(empty)\n}\n";
    prints_as(
        src,
        "app t.x \"X\"\n\nview rows<T>(\n    items: List<T>,\n    empty: Text,\n    primary: Option<host.Action>,\n    icon: Option<Text>,\n    row: (T) -> View,\n) {\n    text(empty)\n}\n",
    );
    // The items fit and the options do not: the options take the new lines.
    let opts = "app t.x \"X\"\n\ndestructive action delete(to: TextChannel | Thread | Direct, m: Message) (icon: .trash, label: \"Delete\") {\n    m.deleted = true\n}\n";
    prints_as(
        opts,
        "app t.x \"X\"\n\ndestructive action delete(to: TextChannel | Thread | Direct, m: Message) (\n    icon: .trash,\n    label: \"Delete\",\n) {\n    m.deleted = true\n}\n",
    );
}

#[test]
fn a_long_call_puts_one_argument_per_line_with_a_trailing_comma() {
    let src = "app t.x \"X\"\n\nfn f(l: Ledger) -> Int {\n    let n = tally(l.entries, by: \"week\", until: .none, empty: \"Nothing yet\", note: \"Counted by the week\")\n    return n\n}\n";
    prints_as(
        src,
        "app t.x \"X\"\n\nfn f(l: Ledger) -> Int {\n    let n = tally(\n        l.entries,\n        by: \"week\",\n        until: .none,\n        empty: \"Nothing yet\",\n        note: \"Counted by the week\",\n    )\n    return n\n}\n",
    );
}

#[test]
fn nested_calls_break_the_outer_list_and_keep_an_inner_one_that_fits() {
    let src = "app t.x \"X\"\n\nfn f(l: Ledger) -> Int {\n    let day = tally(sum(l.entries, of: x -> x.size, note: \"Total size of the day, per entry\"), by: \"week\")\n    return day\n}\n";
    prints_as(
        src,
        "app t.x \"X\"\n\nfn f(l: Ledger) -> Int {\n    let day = tally(\n        sum(l.entries, of: x -> x.size, note: \"Total size of the day, per entry\"),\n        by: \"week\",\n    )\n    return day\n}\n",
    );
    // An inner call too long for the line it lands on breaks its own arguments.
    let deep = "app t.x \"X\"\n\nfn f(l: Ledger) -> Int {\n    let day = tally(sum(l.entries, of: x -> x.size, note: \"Total size of the day, counted per entry and per note\"), by: \"week\")\n    return day\n}\n";
    prints_as(
        deep,
        "app t.x \"X\"\n\nfn f(l: Ledger) -> Int {\n    let day = tally(\n        sum(\n            l.entries,\n            of: x -> x.size,\n            note: \"Total size of the day, counted per entry and per note\",\n        ),\n        by: \"week\",\n    )\n    return day\n}\n",
    );
}

#[test]
fn a_long_chain_breaks_before_each_dot() {
    let src = "app t.x \"X\"\n\nfn f(l: Ledger) -> Int {\n    let shown = l.entries.filter(e -> e.done).sort_by(e -> e.title).map(e -> text(e.title)).count(e -> true)\n    return shown\n}\n";
    prints_as(
        src,
        "app t.x \"X\"\n\nfn f(l: Ledger) -> Int {\n    let shown = l.entries\n        .filter(e -> e.done)\n        .sort_by(e -> e.title)\n        .map(e -> text(e.title))\n        .count(e -> true)\n    return shown\n}\n",
    );
}

#[test]
fn a_long_condition_breaks_before_its_operator() {
    let src = "app t.x \"X\"\n\nfn f(l: Ledger) -> Text {\n    let title = l.entries.find(e -> e.key == key)?.title ?? if l.entries.is_empty() { meta(l).title } else { everything }\n    return title\n}\n";
    prints_as(
        src,
        "app t.x \"X\"\n\nfn f(l: Ledger) -> Text {\n    let title = l.entries.find(e -> e.key == key)?.title\n        ?? if l.entries.is_empty() { meta(l).title } else { everything }\n    return title\n}\n",
    );
    let longer = "app t.x \"X\"\n\nfn f(l: Ledger, other: Ledger) -> Bool {\n    return l.entries.is_empty() and other.entries.is_empty() and l.name == other.name and l.owner == other.owner\n}\n";
    prints_as(
        longer,
        "app t.x \"X\"\n\nfn f(l: Ledger, other: Ledger) -> Bool {\n    return l.entries.is_empty()\n        and other.entries.is_empty()\n        and l.name == other.name\n        and l.owner == other.owner\n}\n",
    );
}

#[test]
fn text_that_cannot_break_stays_on_its_line() {
    // A string literal has no line to break to, and a comment keeps what its author wrote.
    let line = "A paragraph long enough to run past the width the printer keeps, so it stays long.";
    let src = format!("app t.x \"X\"\n\nfn f() -> Text {{\n    return \"{line}\"\n}}\n");
    prints_as(
        &src,
        &format!("app t.x \"X\"\n\nfn f() -> Text {{\n    return \"{line}\"\n}}\n"),
    );
}
