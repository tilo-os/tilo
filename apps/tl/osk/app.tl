app so.tilo.osk "On-Screen Keyboard" (
    icon: .keyboard,
    category: .system,
    tagline: "Types into the field you are in",
    keywords: ["keyboard", "on-screen keyboard", "touch keyboard", "typing", "swipe", "autocorrect", "suggestions", "emoji", "bildschirmtastatur", "tastatur", "tippen", "wischen"],
)

use so.tilo.session as session

// The keyboard is a view of the session's keyboard (`session.keyboard`): the field in front, what
// it asks of typing, and the person's keyboard settings. What it types it asks the session to
// type, and what typing teaches it it keeps in the person's settings through the same resource,
// so it holds no copy of the field and nothing of its own but the word being composed.
// ---------------------------------------------------------------- what is being typed
/// What the person is typing now, as the keyboard holds it while it stands: the word being
/// composed, the word before it, what the bar offers, and the keys' own state (Shift, the page of
/// keys, the held modifiers). It belongs to the field it was typed in: another field starts it
/// afresh.
type Typing {
    /// The field this belongs to, as the session counts fields.
    field: Int = -1
    /// The word being composed, shown in the field as text not yet committed.
    word: Text = ""
    /// The word committed before it, which says which word may come next.
    previous: Text = ""
    /// The composing word is a swipe's: the next space or letter commits it as it is.
    swiped: Bool = false
    /// What the bar offers, in order: `w:` a word, `p:` one of the person's own words, `e:` an
    /// emoji, `t:` the word as typed before a correction.
    chips: List<Text>
    /// The word as typed before a correction made it another, and what followed it: a Backspace
    /// right after it, or the as-typed chip, types it back.
    typed: Text = ""
    tail: Text = ""
    /// Shift: 0 off, 1 the next letter, 2 Caps Lock.
    shift: Int = 0
    /// The keys shown: `letters`, `symbols`, `emoji` (the picker in the keys' place) or
    /// `emoji-search` (the picker over the letters, which type its search).
    page: Text = "letters"
    /// The emoji search the letters typed.
    query: Text = ""
    /// Ctrl and Alt: 0 off, 1 the next key, 2 locked.
    ctrl: Int = 0
    alt: Int = 0
    /// The next letter starts a sentence.
    sentence: Bool = true
    /// The keys are put away: only the strip that brings them back stands.
    parked: Bool = false
    /// What a long press offers on each letter, the typing language's forms first, and the
    /// language and case they were asked in.
    forms: List<Text>
    forms_for: Text = ""
}

// ---------------------------------------------------------------- what the device answers
/// One word typing taught, as the device's prediction reads it.
type Taught {
    context: Text
    word: Text
    count: Int
}

type SuggestAsk {
    word: Text
    previous: Text
    language: Text
    personal: List<Text>
    personalization: Bool
    learned: List<Taught>
    kept: List<Text>
    limit: Int
}

/// One word offered.
type Offer {
    word: Text = ""
    score: Float = 0.0
    personal: Bool = false
    emoji: Bool = false
}

type Suggested {
    completions: List<Offer>
    next: List<Offer>
}

type CorrectAsk {
    word: Text
    language: Text
    rows: List<Text>
    kept: List<Text>
}

type Corrected {
    correction: Option<Text>
}

type SwipeAsk {
    path: List<Float>
    area: List<Float>
    rows: List<Text>
    language: Text
    shift: Text
    limit: Int
    judged: Bool
    aspect: Float
}

type Candidate {
    word: Text = ""
    score: Float = 0.0
}

type Swiped {
    swipe: Bool = false
    candidates: List<Candidate>
}

type TouchAsk {
    point: List<Float>
    area: List<Float>
    rows: List<Text>
    previous: Text
    word: Text
    language: Text
    offsets: List<Offset>
    personalization: Bool
}

/// Where the person's taps land on one letter, as the device keeps it: the ask carries it as a
/// value, since the reading's own offsets are records of the keyboard's, which JSON does not write.
type Offset {
    letter: Text
    dx: Float
    dy: Float
    samples: Int
}

/// Where the person's taps land on one letter, as the device learned it from this tap.
type Landed {
    ch: Text = ""
    dx: Float = 0.0
    dy: Float = 0.0
    samples: Int = 0
}

type Touched {
    key: Option<List<Int>>
    taught: Option<Landed>
}

type AccentsAsk {
    keys: List<Text>
    language: Text
    shift: Text
}

type Accents {
    forms: Map<List<Text>>
}

// ---------------------------------------------------------------- the keys
/// The keys of a layout, row by row. A key is what it types (`q`, `,`, `.com`), or a key that does
/// something, written `!what` (`!shift`, `!backspace`, `!symbols`, `!emoji`, `!space`, `!layout`,
/// `!return`); a width other than one letter's follows `~` .
fn rows_named(name: Text) -> List<List<Text>> {
    if name == "azerty" {
        [
            ["a", "z", "e", "r", "t", "y", "u", "i", "o", "p"],
            ["q", "s", "d", "f", "g", "h", "j", "k", "l", "m"],
            ["!shift~1.5", "w", "x", "c", "v", "b", "n", "!backspace~1.5"],
            ["!symbols~1.5", ",", "!emoji~1.5", "!space~5", ".", "!layout~1.5", "!return~1.5"],
        ]
    } else if name == "symbols" {
        [
            ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"],
            ["@", "#", "$", "_", "&", "-", "+", "(", ")", "/"],
            ["%", "*", "\"", "'", ":", ";", "!", "?", "!backspace~2"],
            ["!symbols~1.5", ",", "!space~5", ".", "!return~1.5"],
        ]
    } else if name == "numeric" or name == "decimal" {
        [
            ["1", "2", "3", "!backspace"],
            ["4", "5", "6", if name == "numeric" { "-" } else { "." }],
            ["7", "8", "9", if name == "numeric" { "." } else { "," }],
            ["0~3", "!return"],
        ]
    } else if name == "phone" {
        [["1", "2", "3"], ["4", "5", "6"], ["7", "8", "9"], ["*", "0", "#"], ["+", "!backspace~2"]]
    } else if name == "pin" {
        [["1", "2", "3"], ["4", "5", "6"], ["7", "8", "9"], ["!backspace", "0", "!return"]]
    } else if name == "email" or name == "url" {
        [
            ["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"],
            ["a", "s", "d", "f", "g", "h", "j", "k", "l"],
            ["!shift~1.5", "z", "x", "c", "v", "b", "n", "m", "!backspace~1.5"],
            if name == "email" {
                ["!symbols~1.5", "@", ".", "!space~3.5", ".com~1.5", "!return~1.5"]
            } else {
                ["!symbols~1.5", ".", "/", "!space~3.5", ".com~1.5", "!return~1.5"]
            },
        ]
    } else {
        [
            ["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"],
            ["a", "s", "d", "f", "g", "h", "j", "k", "l"],
            ["!shift~1.5", "z", "x", "c", "v", "b", "n", "m", "!backspace~1.5"],
            ["!symbols~1.5", ",", "!emoji~1.5", "!space~5", ".", "!layout~1.5", "!return~1.5"],
        ]
    }
}

/// The layout the field's purpose types on: a number pad, a dialpad, a code's digits, the address
/// keyboards, or the person's letters.
fn purpose_layout(k: session.Keyboard) -> Text {
    let p = k.purpose
    if p == "number" or p == "date" or p == "time" {
        "numeric"
    } else if p == "decimal" {
        "decimal"
    } else if p == "telephone" {
        "phone"
    } else if p == "pin" {
        "pin"
    } else if p == "email" { "email" } else if p == "url" { "url" } else { letters_of(k) }
}

/// The person's letter layout.
fn letters_of(k: session.Keyboard) -> Text {
    if k.layout == "azerty" { "azerty" } else { "qwerty" }
}

/// The layouts the language key cycles through, in the person's order.
fn layout_order(k: session.Keyboard) -> List<Text> {
    let known = k.layouts.filter(l -> l == "qwerty" or l == "azerty")
    if known.is_empty() { ["qwerty", "azerty"] } else { known }
}

/// The layout the language key moves to from the one typed with.
fn next_layout(k: session.Keyboard) -> Text {
    let order = layout_order(k)
    let at = order.index_of(letters_of(k)) ?? -1
    order.at((at + 1) % order.len()) ?? letters_of(k)
}

/// The keys shown now: the symbols over the letters' purposes while they are asked for.
fn shown_layout(k: session.Keyboard, t: Typing) -> Text {
    let base = purpose_layout(k)
    if t.page == "emoji-search" {
        letters_of(k)
    } else if t.page == "symbols"
        and base != "numeric"
        and base != "decimal"
        and base != "phone"
        and base != "pin" {
        "symbols"
    } else { base }
}

fn shown_rows(k: session.Keyboard, t: Typing) -> List<List<Text>> {
    rows_named(shown_layout(k, t))
}

/// Every key shown, in reading order: what the pad's indices count.
fn shown_keys(k: session.Keyboard, t: Typing) -> List<Text> {
    shown_rows(k, t).flat_map(r -> r)
}

/// The layout's rows as the device reads them: each key `label:width`.
fn device_rows(rows: List<List<Text>>) -> List<Text> {
    rows.map(r -> r.map(key -> "{device_label(key)}:{width_of(key)}").join(","))
}

/// A key as the device reads it: a key that does something by its name, and `,` named too, since
/// commas part the keys of a row.
fn device_label(key: Text) -> Text {
    let what = what_of(key)
    if what.starts_with("!") {
        what.slice(1, what.len())
    } else if what == "," { "comma" } else { what }
}

/// What a key is, without its width.
fn what_of(key: Text) -> Text {
    key.split("~").first() ?? key
}

/// How many letter keys wide a key is.
fn width_of(key: Text) -> Float {
    let parts = key.split("~")
    if parts.len() > 1 { (parts.at(1) ?? "1").to_float() ?? 1.0 } else { 1.0 }
}

fn is_letter(text: Text) -> Bool {
    text.len() == 1 and text.lower() != text.upper()
}

/// A letter cased for Shift.
fn cased(text: Text, shift: Int) -> Text {
    if shift > 0 { text.upper() } else { text }
}

/// The pad's keys for what is shown now.
fn pad_keys(k: session.Keyboard, t: Typing) -> List<ui.PadKey> {
    let rows = shown_rows(k, t)
    rows.flat_map(
        row -> row.map(key -> pad_key(k, t, key, rows.index_of(row) ?? 0, row.index_of(key) ?? 0)),
    )
}

fn pad_key(k: session.Keyboard, t: Typing, key: Text, r: Int, c: Int) -> ui.PadKey {
    let what = what_of(key)
    let width = width_of(key)
    if what == "!shift" {
        ui.PadKey.key(
            label: std.say("Shift"),
            name: if t.shift == 2 {
                std.say("Caps Lock, on")
            } else if t.shift == 1 {
                std.say("Shift, on for the next letter")
            } else { std.say("Shift") },
            icon: "arrow-big-up",
            tone: if t.shift > 0 { .active } else { .modifier },
            width: width,
            row: r,
            column: c,
            holds: [],
        )
    } else if what == "!backspace" {
        ui.PadKey.key(
            label: std.say("Backspace"),
            name: std.say("Backspace"),
            icon: "delete",
            tone: .modifier,
            width: width,
            row: r,
            column: c,
            holds: [],
            repeat: true,
        )
    } else if what == "!return" {
        ui.PadKey.key(
            label: std.say("Return"),
            name: std.say("Return"),
            icon: "corner-down-left",
            tone: .primary,
            width: width,
            row: r,
            column: c,
            holds: [],
        )
    } else if what == "!space" {
        ui.PadKey.key(
            label: if k.purpose == "url" { std.say("no space") } else { std.say("space") },
            name: std.say("Space"),
            icon: "",
            tone: .plain,
            width: width,
            row: r,
            column: c,
            holds: [],
            caret: true,
        )
    } else if what == "!symbols" {
        ui.PadKey.key(
            label: if t.page == "symbols" { std.say("ABC") } else { "?123" },
            name: if t.page == "symbols" {
                std.say("Letters")
            } else { std.say("Numbers and symbols") },
            icon: "",
            tone: .modifier,
            width: width,
            row: r,
            column: c,
            holds: [],
        )
    } else if what == "!emoji" {
        ui.PadKey.key(
            label: std.say("Emoji"),
            name: std.say("Emoji"),
            icon: "smile",
            tone: .modifier,
            width: width,
            row: r,
            column: c,
            holds: [],
        )
    } else if what == "!layout" {
        let target = next_layout(k)
        ui.PadKey.key(
            label: if target == "azerty" { "FR" } else { "EN" },
            name: std.say("Switch to {layout_name(target)}"),
            icon: "",
            tone: .modifier,
            width: width,
            row: r,
            column: c,
            holds: [],
        )
    } else if is_letter(what) {
        let shown = cased(what, t.shift)
        ui.PadKey.key(
            label: shown,
            name: shown,
            icon: "",
            tone: .plain,
            width: width,
            row: r,
            column: c,
            holds: if k.secret { [] } else { forms_of(t, what) },
        )
    } else {
        ui.PadKey.key(
            label: what,
            name: what,
            icon: "",
            tone: .plain,
            width: width,
            row: r,
            column: c,
            holds: [],
        )
    }
}

/// What a long press on `letter` offers: the letter first, then its forms, cased for Shift. A
/// letter with no other form offers nothing.
fn forms_of(t: Typing, letter: Text) -> List<Text> {
    let forms = (t.forms.find(e -> e.starts_with("{letter}|")) ?? "")
        .split("|")
        .filter(f -> f != "")
    if forms.len() < 2 { [] } else { forms.map(f -> cased(f, t.shift)) }
}

/// What a letter layout is called.
fn layout_name(layout: Text) -> Text {
    if layout == "azerty" { std.say("French (AZERTY)") } else { std.say("English (QWERTY)") }
}

// ---------------------------------------------------------------- what the field asks
/// The language the keyboard reads words in: the person's choice, or the letter layout's.
fn language_of(k: session.Keyboard) -> Text {
    if k.typing_language == "en" or k.typing_language == "fr" {
        k.typing_language
    } else if letters_of(k) == "azerty" { "fr" } else { "en" }
}

/// Whether one of the field's content hints is set: they are the text-input protocol's bits.
fn hinted(k: session.Keyboard, bit: Int) -> Bool {
    k.hints / bit % 2 == 1
}

/// Words typed here are composed, offered, corrected and learned: the field is no secret, and
/// what it takes is words (a text, an address); a number, a code or a terminal's line is typed as
/// it is.
fn composes(k: session.Keyboard) -> Bool {
    not k.secret and (k.purpose == "text" or k.purpose == "email" or k.purpose == "url")
}

/// A word typed here may be corrected: a word in a plain text field, while corrections are on.
fn corrects(k: session.Keyboard) -> Bool {
    k.autocorrect and composes(k) and k.purpose == "text"
}

/// What a word typed here may be learned from.
fn learns(k: session.Keyboard) -> Bool {
    k.personalization and composes(k)
}

/// The keyboard follows the field in front: in another field what was composed in the last one is
/// gone, and the keys start where that field's case asks.
fn here(k: session.Keyboard, t: Typing) -> Typing {
    if t.field == k.field {
        t
    } else {
        settle_case(
            k,
            Typing(field: k.field, forms: t.forms, forms_for: t.forms_for, parked: t.parked),
        )
    }
}

/// Shift as the field's case asks now: Caps Lock throughout an upper-case field, off in a
/// lower-case one, on for the next letter at a sentence's start where the field capitalizes.
fn settle_case(k: session.Keyboard, typing: Typing) -> Typing {
    let t = typing
    if hinted(k, 16) {
        t.shift = 2
    } else if hinted(k, 8) {
        t.shift = 0
    } else if t.shift != 2 and hinted(k, 4) and composes(k) and t.sentence and t.word == "" {
        t.shift = 1
    }
    t
}

/// Shift as the device reads it.
fn shift_name(t: Typing) -> Text {
    if t.shift == 2 { "caps" } else if t.shift == 1 { "shift" } else { "" }
}

/// The word is one the person keeps as they type it: their own, kept, or one typing taught.
fn own_word(k: session.Keyboard, word: Text) -> Bool {
    let lower = word.lower()
    k.personal.any(w -> w.lower() == lower)
        or k.kept.any(w -> w.lower() == lower)
        or k.transitions.any(x -> x.word == lower)
}

// ---------------------------------------------------------------- typing
/// A key of the pad was pressed: `index` counts the keys shown, `at` is where the finger was, as
/// fractions of the pad's box.
action pressed(k: session.Keyboard, typing: Typing, index: Int, at: List<Float>) -> Typing {
    let t = here(k, typing)
    let key = what_of(shown_keys(k, t).at(index) ?? "")
    if key == "" {
        return t
    }
    if t.page == "emoji-search" {
        return searched(t, key)
    }
    if is_letter(key) {
        return letter(k, t, meant(k, t, key, at))
    }
    press(k, t, key)
}

/// A long press chose one of a key's forms.
action held(k: session.Keyboard, typing: Typing, choice: List<Text>) -> Typing {
    let t = here(k, typing)
    let form = choice.at(1) ?? ""
    if form == "" {
        return t
    }
    if t.page == "emoji-search" {
        return searched(t, form)
    }
    letter(k, t, form)
}

/// The letter a tap meant. Where this person's taps tend to land and which letter may come next
/// move the edge between two letters, never the keys drawn; what the tap taught of where they
/// land is kept. Only in a field of words, and never a secret's.
action meant(k: session.Keyboard, t: Typing, key: Text, at: List<Float>) -> Text {
    if not composes(k) or t.page != "letters" or at.len() < 2 {
        return key
    }
    let rows = shown_rows(k, t)
    let asked = json.encode(
        TouchAsk(
            point: at,
            area: [0.0, 0.0, 1.0, 1.0],
            rows: device_rows(rows),
            previous: t.previous,
            word: t.word.lower(),
            language: language_of(k),
            offsets: k.touch.map(
                o -> Offset(letter: o.letter, dx: o.dx, dy: o.dy, samples: o.samples),
            ),
            personalization: k.personalization,
        ),
    )
    match engine.call("touch", asked) {
        .ok(text) => match json.decode<Touched>(text) {
            .ok(got) => {
                if let taught = got.taught {
                    if k.personalization {
                        session.teach(
                            k,
                            letter: taught.ch,
                            dx: taught.dx,
                            dy: taught.dy,
                            samples: taught.samples,
                        )
                    }
                }
                if let place = got.key {
                    let row = rows.at(place.at(0) ?? -1) ?? []
                    let found = what_of(row.at(place.at(1) ?? -1) ?? key)
                    if is_letter(found) { found } else { key }
                } else { key }
            }
            .err(_) => key,
        },
        .err(_) => key,
    }
}

/// A key typed while the letters type the emoji search: it edits the search, and Return, the
/// emoji key or ABC go back to the picker.
fn searched(typing: Typing, key: Text) -> Typing {
    let t = typing
    if key == "!backspace" {
        t.query = if t.query == "" { "" } else { t.query.slice(0, t.query.len() - 1) }
    } else if key == "!space" {
        t.query = t.query + " "
    } else if key == "!shift" {
        t.shift = (t.shift + 1) % 3
    } else if key == "!return" or key == "!emoji" or key == "!symbols" {
        t.page = "emoji"
    } else if not key.starts_with("!") {
        t.query = t.query + key.lower()
    }
    t
}

/// The elision a French word starts with, with its apostrophe (`l'`, `qu'`), or nothing: what
/// follows it is the word that is corrected and learned.
fn elided(word: Text) -> Text {
    let parts = word.split("'")
    let head = (parts.first() ?? "").lower()
    if parts.len() == 2
        and (parts.at(1) ?? "") != ""
        and ["c", "d", "j", "l", "m", "n", "s", "t", "qu", "jusqu", "lorsqu", "puisqu"]
            .contains(head) {
        "{parts.first() ?? ""}'"
    } else { "" }
}

/// Some word of the language goes on past `stem'`: `don'`, `l'`, `aujourd'`.
action continues(k: session.Keyboard, t: Typing, stem: Text) -> Bool {
    let asked = json.encode(
        SuggestAsk(
            word: "{stem.lower()}'",
            previous: t.previous.lower(),
            language: language_of(k),
            personal: k.personal,
            personalization: k.personalization,
            learned: [],
            kept: k.kept,
            limit: 1,
        ),
    )
    match engine.call("suggest", asked) {
        .ok(text) => match json.decode<Suggested>(text) {
            .ok(got) => not got.completions.is_empty() or elided("{stem}'x") != "",
            .err(_) => false,
        },
        .err(_) => false,
    }
}

/// A letter typed: composed into the word where the field takes words, else typed as it is. With
/// Ctrl or Alt held, the key is a chord: pressed, not typed.
action letter(k: session.Keyboard, typing: Typing, key: Text) -> Typing {
    let t = typing
    if t.ctrl > 0 or t.alt > 0 {
        return chord(k, t, key.lower())
    }
    let typed = cased(key, t.shift)
    if t.shift == 1 {
        t.shift = 0
    }
    t.typed = ""
    t.tail = ""
    t.sentence = false
    if not composes(k) {
        session.type_text(k, text: typed)
        return t
    }
    if t.swiped {
        // A letter after a swiped word starts the next one: the swiped word goes in with the space
        // a swipe leaves.
        t = commit(k, t, " ", false)
        t.sentence = false
    }
    if t.word.len() >= 128 {
        // No word is this long: it goes in as it is, and composing starts again.
        t = commit(k, t, "", false)
    }
    t.word = t.word + typed
    session.preedit(k, text: t.word)
    offer(k, t)
}

/// A key that is no letter: one of the keys' own, an editing key, or a mark.
action press(k: session.Keyboard, typing: Typing, key: Text) -> Typing {
    let t = typing
    if key == "!shift" {
        t.shift = (t.shift + 1) % 3
        return t
    }
    if key == "!symbols" {
        t.page = if t.page == "symbols" { "letters" } else { "symbols" }
        return t
    }
    if key == "!emoji" {
        t.page = "emoji"
        return t
    }
    if key == "!layout" {
        t = commit(k, t, "", false)
        session.set_layouts(k, layouts: layout_order(k), active: next_layout(k))
        return t
    }
    if key == "!backspace" {
        if t.ctrl > 0 or t.alt > 0 {
            return chord(k, t, "backspace")
        }
        return backspace(k, t)
    }
    if key == "!return" {
        if t.ctrl > 0 or t.alt > 0 {
            return chord(k, t, "return")
        }
        // Return sends the word as typed: a correction would land in a line already sent.
        t = commit(k, t, "", false)
        session.press_key(k, name: "return")
        t.previous = ""
        t.typed = ""
        t.chips = []
        t.sentence = true
        return settle_case(k, offer(k, t))
    }
    if t.ctrl > 0 or t.alt > 0 {
        return chord(k, t, if key == "!space" { "space" } else { key })
    }
    if key == "!space" {
        t = commit(k, t, " ", true)
        return settle_case(k, offer(k, t))
    }
    if (key == "'" or key == "’")
        and t.word != ""
        and not t.swiped
        and composes(k)
        and continues(k, t, t.word) {
        // An apostrophe in a word goes on composing it where some word goes on past it:
        // `don't`, `l'homme`. The rest of the word is letters, so the keys go back to them.
        t.word = t.word + "'"
        if t.page == "symbols" {
            t.page = "letters"
        }
        session.preedit(k, text: t.word)
        return offer(k, t)
    }
    // A mark ends the word and goes in with it.
    t = commit(k, t, key, true)
    t.sentence = key == "." or key == "!" or key == "?"
    if t.shift == 1 {
        t.shift = 0
    }
    if t.page == "symbols" and key == "'" {
        t.page = "letters"
    }
    offer(k, t)
}

/// The composing word goes into the field with `after` behind it: corrected first when `correct`
/// asks and the field takes corrections, and learned from. With no word composing, `after` is
/// typed alone.
action commit(k: session.Keyboard, typing: Typing, after: Text, correct: Bool) -> Typing {
    let t = typing
    if t.word == "" {
        if after != "" { session.type_text(k, text: after) }
        t.tail = ""
        return t
    }
    let word = t.word
    // In `l'homme` the word is `homme`: only it is corrected and learned.
    let elision = elided(word)
    let stem = word.slice(elision.len(), word.len())
    let final = if correct and corrects(k) and not t.swiped and not own_word(k, stem) {
        elision + corrected(k, t, stem)
    } else { word }
    session.preedit(k, text: "")
    session.type_text(k, text: final + after)
    if learns(k) {
        session.learn(
            k,
            before: t.previous.lower(),
            word: final.slice(elision.len(), final.len()).lower(),
        )
    }
    t.typed = if final != word { word } else { "" }
    t.tail = after
    t.previous = final
    t.word = ""
    t.swiped = false
    t
}

/// What the device would correct `word` to, or the word itself.
action corrected(k: session.Keyboard, t: Typing, word: Text) -> Text {
    let asked = json.encode(
        CorrectAsk(
            word: word,
            language: language_of(k),
            rows: device_rows(shown_rows(k, t)),
            kept: k.kept + k.personal,
        ),
    )
    match engine.call("correct", asked) {
        .ok(text) => match json.decode<Corrected>(text) {
            .ok(got) => got.correction ?? word,
            .err(_) => word,
        },
        .err(_) => word,
    }
}

/// Backspace: right after a correction it types the word back as the person typed it, and keeps
/// it so; in a word it takes its last letter (a swiped word goes whole); elsewhere it reaches the
/// field.
action backspace(k: session.Keyboard, typing: Typing) -> Typing {
    let t = typing
    if t.typed != "" and t.word == "" {
        return undo_correction(k, t)
    }
    if t.word != "" {
        t.word = if t.swiped { "" } else { t.word.slice(0, t.word.len() - 1) }
        t.swiped = false
        session.preedit(k, text: t.word)
        return settle_case(k, offer(k, t))
    }
    session.press_key(k, name: "backspace")
    t.tail = ""
    t
}

/// The correction just made is taken back: the word goes in again as typed, and is never
/// corrected again.
action undo_correction(k: session.Keyboard, typing: Typing) -> Typing {
    let t = typing
    session.press_key(k, name: "backspace", times: (t.previous + t.tail).len())
    session.type_text(k, text: t.typed + t.tail)
    session.keep(k, word: t.typed)
    t.previous = t.typed
    t.typed = ""
    t.tail = ""
    offer(k, t)
}

/// A key pressed with Ctrl or Alt held. A modifier latched for one key lets go after it.
action chord(k: session.Keyboard, typing: Typing, name: Text) -> Typing {
    let t = commit(k, typing, "", false)
    session.press_key(k, name: name, ctrl: t.ctrl > 0, alt: t.alt > 0)
    if t.ctrl == 1 {
        t.ctrl = 0
    }
    if t.alt == 1 {
        t.alt = 0
    }
    t
}

/// The space bar was dragged: the caret moves by `steps` letters, after the word goes in.
action caret(k: session.Keyboard, typing: Typing, steps: Int) -> Typing {
    let t = commit(k, here(k, typing), "", false)
    let name = if steps < 0 { "left" } else { "right" }
    session.press_key(k, name: name, times: if steps < 0 { 0 - steps } else { steps })
    t.typed = ""
    t.chips = []
    t
}

/// A finger crossed the letters: the word it spelled composes, cased for Shift, and the other
/// words it may have meant stand in the bar. A swiped word is never corrected.
action swiped(k: session.Keyboard, typing: Typing, path: List<Float>, aspect: Float) -> Typing {
    let t = here(k, typing)
    if not composes(k) or t.page != "letters" or path.len() < 4 {
        return t
    }
    let asked = json.encode(
        SwipeAsk(
            path: path,
            area: [0.0, 0.0, 1.0, 1.0],
            aspect: aspect,
            rows: device_rows(shown_rows(k, t)),
            language: language_of(k),
            shift: shift_name(t),
            limit: 5,
            judged: true,
        ),
    )
    let found = match engine.call("swipe", asked) {
        .ok(text) => match json.decode<Swiped>(text) {
            .ok(got) => got.candidates.map(c -> c.word).filter(w -> w != ""),
            .err(_) => [],
        },
        .err(_) => [],
    }
    let first = found.first() ?? ""
    if first == "" {
        return t
    }
    // A swipe after a word starts the next one.
    if t.word != "" {
        t = commit(k, t, " ", not t.swiped)
    } else if t.previous != "" and t.tail == "" { session.type_text(k, text: " ") }
    if t.shift == 1 {
        t.shift = 0
    }
    t.word = first
    t.swiped = true
    t.typed = ""
    t.tail = ""
    t.sentence = false
    session.preedit(k, text: first)
    t.chips = found.filter(w -> w != first).map(w -> "w:{w}")
    t
}

/// What the bar offers now: the words the one composing may become, or the ones that may come
/// next, the person's own first, then up to two emoji.
action offer(k: session.Keyboard, typing: Typing) -> Typing {
    let t = typing
    let typed_back = if t.typed != "" and t.word == "" { ["t:{t.typed}"] } else { [] }
    if not k.suggestions or not composes(k) {
        t.chips = typed_back
        return t
    }
    let asked = json.encode(
        SuggestAsk(
            word: t.word.lower(),
            previous: t.previous.lower(),
            language: language_of(k),
            personal: k.personal,
            personalization: k.personalization,
            learned: k.transitions.map(
                x -> Taught(context: x.before, word: x.word, count: x.count),
            ),
            kept: k.kept,
            limit: 5,
        ),
    )
    let offers = match engine.call("suggest", asked) {
        .ok(text) => match json.decode<Suggested>(text) {
            .ok(got) => if t.word == "" { got.next } else { got.completions },
            .err(_) => [],
        },
        .err(_) => [],
    }
    let own = offers.filter(o -> o.personal and not o.emoji).map(o -> "p:{o.word}")
    let words = offers
        .filter(o -> not o.personal and not o.emoji and o.word.lower() != t.word.lower())
        .map(o -> "w:{shaped(t, o.word)}")
    let emoji = offers.filter(o -> o.emoji).take(2).map(o -> "e:{o.word}")
    t.chips = (typed_back + own + words).take(5) + emoji
    t
}

/// An offered word cased as the person is typing it: capitalized where the word so far is.
fn shaped(t: Typing, word: Text) -> Text {
    if t.shift == 2 {
        word.upper()
    } else if t.word != "" and t.word.slice(0, 1) != t.word.slice(0, 1).lower() {
        word.slice(0, 1).upper() + word.slice(1, word.len())
    } else { word }
}

/// One of the bar's offers was taken.
action took(k: session.Keyboard, typing: Typing, chip: Text) -> Typing {
    let t = here(k, typing)
    let kind = chip.slice(0, 2)
    let word = chip.slice(2, chip.len())
    if kind == "t:" {
        return undo_correction(k, t)
    }
    if kind == "e:" {
        t = commit(k, t, if t.word != "" { " " } else { "" }, false)
        session.type_text(k, text: word)
        return offer(k, t)
    }
    if t.swiped {
        // Another reading of the swipe: it composes instead, and the swipe's first stands in the
        // bar in its place.
        let others = t.chips.filter(c -> c != chip) + ["w:{t.word}"]
        t.word = word
        session.preedit(k, text: word)
        t.chips = others
        return t
    }
    t.word = word
    t = commit(k, t, " ", false)
    settle_case(k, offer(k, t))
}

/// An emoji from the picker goes in after the word.
action picked(k: session.Keyboard, typing: Typing, emoji: Text) -> Typing {
    let t = commit(k, here(k, typing), if typing.word != "" { " " } else { "" }, false)
    session.type_text(k, text: emoji)
    t.page = "letters"
    offer(k, t)
}

// ---------------------------------------------------------------- the strip above the keys
/// One of the strip's commands.
action tool(k: session.Keyboard, typing: Typing, name: Text) -> Typing {
    let t = here(k, typing)
    if name == "paste" {
        t = commit(k, t, "", false)
        session.paste(k)
    } else if name == "left"
        or name == "right"
        or name == "up"
        or name == "down"
        or name == "escape"
        or name == "tab" {
        t = commit(k, t, "", false)
        session.press_key(k, name: name, ctrl: t.ctrl > 0, alt: t.alt > 0)
        if t.ctrl == 1 {
            t.ctrl = 0
        }
        if t.alt == 1 {
            t.alt = 0
        }
        t.typed = ""
        t.chips = []
    } else if name == "ctrl" {
        t.ctrl = (t.ctrl + 1) % 3
    } else if name == "alt" {
        t.alt = (t.alt + 1) % 3
    } else if name == "hide" {
        t = commit(k, t, "", false)
        t.parked = false
        session.set_visible(k, visible: false)
    } else if name == "letters" {
        t.page = "letters"
        t.query = ""
    } else if name == "search" {
        t.page = "emoji-search"
        t.query = ""
    } else if name == "delete" {
        t = commit(k, t, "", false)
        session.press_key(k, name: "backspace")
    } else if name == "park" {
        t.parked = true
    } else if name == "unpark" {
        t.parked = false
    }
    t
}

/// What a latching modifier says it is doing.
fn latch_name(name: Text, state: Int) -> Text {
    if state == 2 {
        "{name}, locked"
    } else if state == 1 { "{name}, for the next key" } else { name }
}

/// The forms a long press offers on every letter, asked of the device in the language typed.
action with_forms(k: session.Keyboard, typing: Typing) -> Typing {
    let t = typing
    let language = language_of(k)
    if t.forms_for == language {
        return t
    }
    let letters = rows_named(letters_of(k))
        .flat_map(r -> r)
        .map(key -> what_of(key))
        .filter(key -> is_letter(key))
    let asked = json.encode(AccentsAsk(keys: letters, language: language, shift: ""))
    match engine.call("accents", asked) {
        .ok(text) => match json.decode<Accents>(text) {
            .ok(got) => {
                t.forms = letters
                    .map(l -> (got.forms.get(l) ?? []).join("|"))
                    .filter(e -> e.contains("|"))
                t.forms_for = language
            }
            .err(_) => {}
        },
        .err(_) => {}
    }
    t
}

// ---------------------------------------------------------------- the view
/// The keyboard as it stands under the field: the strip of commands, the bar of offers and the
/// keys, or the emoji picker in the keys' place, or only a strip once the keys are put away.
view keyboard(k: session.Keyboard) {
    state typing: Typing = Typing()
    let t = here(k, typing)
    let terminal = k.purpose == "terminal"
    ui.Timer(after: 0.01, restart: k.typing_language + k.layout) {
        on tick: {
            let next = with_forms(k, typing)
            set(typing, next)
        }
    }
    if k.visible {
        stack(gap: .none, fill) {
            if t.parked {
                ui.Toolbar {
                    ui.ToolbarSpacer()
                    ui.ToolbarButton("Show keys", icon: .keyboard) {
                        on press: {
                            let next = tool(k, typing, "unpark")
                            set(typing, next)
                        }
                    }
                    ui.ToolbarButton("Hide keyboard", icon: .chevron_down) {
                        on press: {
                            let next = tool(k, typing, "hide")
                            set(typing, next)
                        }
                    }
                }
            } else {
                if terminal {
                    ui.Toolbar {
                        ui.ToolbarButton("Esc") {
                            on press: {
                                let next = tool(k, typing, "escape")
                                set(typing, next)
                            }
                        }
                        ui.ToolbarButton("Tab") {
                            on press: {
                                let next = tool(k, typing, "tab")
                                set(typing, next)
                            }
                        }
                        ui.ToolbarButton(latch_name("Ctrl", t.ctrl), toggled: .some(t.ctrl > 0)) {
                            on press: {
                                let next = tool(k, typing, "ctrl")
                                set(typing, next)
                            }
                        }
                        ui.ToolbarButton(latch_name("Alt", t.alt), toggled: .some(t.alt > 0)) {
                            on press: {
                                let next = tool(k, typing, "alt")
                                set(typing, next)
                            }
                        }
                        ui.ToolbarSeparator()
                        ui.ToolbarButton("Left", icon: .arrow_left) {
                            on press: {
                                let next = tool(k, typing, "left")
                                set(typing, next)
                            }
                        }
                        ui.ToolbarButton("Down", icon: .arrow_down) {
                            on press: {
                                let next = tool(k, typing, "down")
                                set(typing, next)
                            }
                        }
                        ui.ToolbarButton("Up", icon: .arrow_up) {
                            on press: {
                                let next = tool(k, typing, "up")
                                set(typing, next)
                            }
                        }
                        ui.ToolbarButton("Right", icon: .arrow_right) {
                            on press: {
                                let next = tool(k, typing, "right")
                                set(typing, next)
                            }
                        }
                        ui.ToolbarSpacer()
                        ui.ToolbarButton("Hide keyboard", icon: .chevron_down) {
                            on press: {
                                let next = tool(k, typing, "hide")
                                set(typing, next)
                            }
                        }
                    }
                } else {
                    ui.Toolbar {
                        ui.ToolbarButton("Paste", icon: .clipboard) {
                            on press: {
                                let next = tool(k, typing, "paste")
                                set(typing, next)
                            }
                        }
                        ui.ToolbarButton("Move caret left", icon: .chevron_left) {
                            on press: {
                                let next = tool(k, typing, "left")
                                set(typing, next)
                            }
                        }
                        ui.ToolbarButton("Move caret right", icon: .chevron_right) {
                            on press: {
                                let next = tool(k, typing, "right")
                                set(typing, next)
                            }
                        }
                        ui.ToolbarSpacer()
                        ui.ToolbarButton("Put the keys away", icon: .minimize) {
                            on press: {
                                let next = tool(k, typing, "park")
                                set(typing, next)
                            }
                        }
                        ui.ToolbarButton("Hide keyboard", icon: .chevron_down) {
                            on press: {
                                let next = tool(k, typing, "hide")
                                set(typing, next)
                            }
                        }
                    }
                    if composes(k) and (t.page == "letters" or t.page == "symbols") {
                        if not k.suggestions {
                            ui.Text("Suggestions are off", role: .caption, secondary: true)
                        } else if t.chips.is_empty() {
                            ui.Text(
                                "Type or swipe to see suggestions",
                                role: .caption,
                                secondary: true,
                            )
                        } else {
                            // The offers stand in a toolbar: however narrow the keyboard, those
                            // that do not fit stay reachable in its More menu, the last first.
                            ui.Toolbar {
                                for chip in t.chips {
                                    ui.ToolbarButton(
                                        if chip.starts_with("t:") {
                                            "“{chip.slice(2, chip.len())}”"
                                        } else { chip.slice(2, chip.len()) },
                                        description: if chip.starts_with("t:") {
                                            .some("Undo the correction")
                                        } else { .none },
                                    ) {
                                        on press: {
                                            let next = took(k, typing, chip)
                                            set(typing, next)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if (t.page == "emoji" or t.page == "emoji-search") and not k.secret {
                    ui.EmojiPane(open: true, label: .some("Emoji"), query: .some(t.query)) {
                        on pick(chosen): {
                            let next = picked(k, typing, chosen)
                            set(typing, next)
                        }
                        on dismiss: {
                            let next = tool(k, typing, "letters")
                            set(typing, next)
                        }
                    }
                    if t.page == "emoji" {
                        ui.Toolbar {
                            ui.ToolbarButton("ABC", description: .some("Back to letters")) {
                                on press: {
                                    let next = tool(k, typing, "letters")
                                    set(typing, next)
                                }
                            }
                            ui.ToolbarButton("Search emoji", icon: .search) {
                                on press: {
                                    let next = tool(k, typing, "search")
                                    set(typing, next)
                                }
                            }
                            ui.ToolbarSpacer()
                            ui.ToolbarButton("Delete", icon: .delete) {
                                on press: {
                                    let next = tool(k, typing, "delete")
                                    set(typing, next)
                                }
                            }
                        }
                    }
                }
                if t.page != "emoji" {
                    ui.KeyPad(
                        pad_keys(k, t),
                        size: if k.size == "compact" {
                            .compact
                        } else if k.size == "large" { .large } else { .standard },
                        caps: t.shift == 2,
                        secret: k.secret,
                        label: .some("On-screen keyboard"),
                    ) {
                        on press(i, at): {
                            let next = pressed(k, typing, i, at)
                            set(typing, next)
                        }
                        on hold(choice): {
                            let next = held(k, typing, choice)
                            set(typing, next)
                        }
                        on swipe(path, aspect): {
                            let next = swiped(k, typing, path, aspect)
                            set(typing, next)
                        }
                        on caret(steps): {
                            let next = caret(k, typing, steps)
                            set(typing, next)
                        }
                    }
                }
            }
        }
    }
}
