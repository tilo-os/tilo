//! The agent's Fluent catalogue: every fixed sentence it says to a person.
//!
//! A run's reply, its progress and why it stopped are the agent's own words,
//! so they are messages here, resolved in the language the person chose for
//! their desktop: their `appearance.language` setting, read the way every
//! app's words are, and the machine's own locale when they chose none. What a
//! model answers is not here; it arrives in whatever language it was asked in.
//!
//! [`follow`] selects that language when a run starts, so a task begun after
//! the person changed it speaks the one they now read.

use tilo_actions::task_step::TaskStep;

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

/// Select the person's language before a run says anything: their settings if
/// they chose one, the machine's locale otherwise. A run starts long after the
/// app did, and the language may have changed in between.
pub fn follow() {
    CATALOG.ensure();
    ui::i18n::follow_configured_language();
}

/// One step of a run, said to the person: what it is doing, and to what.
///
/// The step itself is a small vocabulary a run keeps so it says each step once
/// (`tilo_actions::task_step`); its words are here, so the reply and the note
/// beside it read in the person's language.
#[must_use]
pub fn step(step: &TaskStep) -> String {
    let (key, subject) = match step {
        TaskStep::Searching(what) => ("agent-step-searching", what),
        TaskStep::Reading(what) => ("agent-step-reading", what),
        TaskStep::Editing(what) => ("agent-step-editing", what),
        TaskStep::Exploring(what) => ("agent-step-exploring", what),
    };
    t_args(key, &[("what", ui::i18n::ArgValue::from(bounded(subject)))])
}

/// One step of a run once it is done, said to the person: what it did, and to
/// what. The record of a turn's steps says what happened; [`step`] says what is
/// under way.
#[must_use]
pub fn step_done(step: &TaskStep) -> String {
    let (key, subject) = match step {
        TaskStep::Searching(what) => ("agent-step-searched", what),
        TaskStep::Reading(what) | TaskStep::Exploring(what) => ("agent-step-read", what),
        TaskStep::Editing(what) => return did("Edit", &bounded(what)),
    };
    t_args(key, &[("what", ui::i18n::ArgValue::from(bounded(subject)))])
}

/// A program that read and changed nothing, said as done: it looked up what its
/// answer names, by the kind of the first thing it named ("Looked up your
/// tasks"), or looked things up when it named nothing.
#[must_use]
pub fn looked_up(kind: Option<&str>) -> String {
    match kind.map(said::things).filter(|things| !things.is_empty()) {
        Some(things) => t_args(
            "agent-step-looked-up",
            &[("what", ui::i18n::ArgValue::from(things))],
        ),
        None => t("agent-step-program"),
    }
}

/// What an action did, in the person's words: its label (an app's, "Show a
/// resource on the canvas") said as done, with `subject`, the title of what it
/// acted on, in the place the label leaves for it ("Showed All inboxes on the
/// canvas", "Moved Buy milk to Done", "Looked up your tasks"). Said in the
/// person's language: where that is not English, an app's label is said as it
/// is, with what it acted on beside it.
#[must_use]
pub fn did(label: &str, subject: &str) -> String {
    let (label, subject) = (label.trim(), bounded(subject));
    let english = ui::i18n::I18nManager::current_locale().starts_with("en");
    if english {
        return said::done(label, &subject);
    }
    if subject.is_empty() || label.contains(subject.as_str()) {
        return label.to_owned();
    }
    t_args(
        "agent-step-on",
        &[
            ("action", ui::i18n::ArgValue::from(label)),
            ("subject", ui::i18n::ArgValue::from(subject)),
        ],
    )
}

/// English verb phrases said as done: the grammar [`did`] says an app's label in.
mod said {
    /// Words after a verb that belong to it ("Look up"): the subject follows them.
    const PARTICLES: &[&str] = &[
        "up", "down", "out", "back", "away", "off", "over", "forward",
    ];
    /// Words a label's own phrase goes on with after its object ("Move to Done", "Mark as
    /// read"): the subject stands before them.
    const LINKS: &[&str] = &[
        "to", "into", "onto", "as", "from", "with", "beside", "in", "on",
    ];

    /// `label` said as done with `subject` in it.
    pub(super) fn done(label: &str, subject: &str) -> String {
        // The label's own clause: what follows a comma or a bracket explains it.
        let clause = label
            .split([',', '('])
            .next()
            .unwrap_or(label)
            .trim()
            .trim_end_matches('\u{2026}')
            .trim();
        let mut words = clause.split_whitespace();
        let Some(verb) = words.next() else {
            return subject.to_owned();
        };
        let rest: Vec<&str> = words.collect();
        // A label already saying what is under way, or one naming a thing rather than doing
        // one, is said as it is.
        // An action's id where it has no label ("move_card") is no words to say as done.
        if verb.to_lowercase().ends_with("ing") || !verb.chars().all(char::is_alphabetic) {
            return label.to_owned();
        }
        if verb.eq_ignore_ascii_case("new") {
            // "New note": what it made.
            return join(&[&cased("made", verb), "a new"], &rest);
        }
        let past = past(verb);
        let particles = rest
            .iter()
            .take_while(|word| PARTICLES.contains(&word.to_lowercase().as_str()))
            .count();
        let (particles, after) = rest.split_at(particles);
        let mut head: Vec<&str> = vec![&past];
        head.extend_from_slice(particles);
        if subject.is_empty() || clause.contains(subject) {
            return join(&head, after);
        }
        match after {
            // "Close a place": the subject is the place it acted on.
            [article, _object, more @ ..] if is_article(article) => {
                head.push(subject);
                join(&head, more)
            }
            // "Show", "Look up", "Move to Done": the subject follows the verb.
            [] => {
                head.push(subject);
                join(&head, &[])
            }
            [link, ..] if LINKS.contains(&link.to_lowercase().as_str()) => {
                head.push(subject);
                join(&head, after)
            }
            // The label names its own object ("Turn Wi-Fi on or off"): it says what it did.
            _ => join(&head, after),
        }
    }

    fn is_article(word: &str) -> bool {
        matches!(word.to_lowercase().as_str(), "a" | "an")
    }

    fn join(head: &[&str], rest: &[&str]) -> String {
        head.iter()
            .chain(rest)
            .copied()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `word` in the case `like` is written in: capitalised when it is.
    fn cased(word: &str, like: &str) -> String {
        if like.chars().next().is_some_and(char::is_uppercase) {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        } else {
            word.to_owned()
        }
    }

    /// The verb `verb` in the past tense, in its case.
    pub(super) fn past(verb: &str) -> String {
        let lower = verb.to_lowercase();
        let said = IRREGULAR
            .iter()
            .find(|(present, _)| *present == lower)
            .map_or_else(|| regular(&lower), |(_, past)| (*past).to_owned());
        cased(&said, verb)
    }

    /// Verbs whose past tense is not the regular one.
    const IRREGULAR: &[(&str, &str)] = &[
        ("bring", "brought"),
        ("build", "built"),
        ("buy", "bought"),
        ("choose", "chose"),
        ("cut", "cut"),
        ("do", "did"),
        ("draw", "drew"),
        ("find", "found"),
        ("forget", "forgot"),
        ("get", "got"),
        ("give", "gave"),
        ("go", "went"),
        ("hide", "hid"),
        ("hold", "held"),
        ("keep", "kept"),
        ("know", "knew"),
        ("lay", "laid"),
        ("leave", "left"),
        ("let", "let"),
        ("make", "made"),
        ("pay", "paid"),
        ("put", "put"),
        ("read", "read"),
        ("redo", "redid"),
        ("reset", "reset"),
        ("run", "ran"),
        ("say", "said"),
        ("see", "saw"),
        ("send", "sent"),
        ("set", "set"),
        ("shut", "shut"),
        ("sit", "sat"),
        ("split", "split"),
        ("stand", "stood"),
        ("submit", "submitted"),
        ("take", "took"),
        ("tell", "told"),
        ("think", "thought"),
        ("undo", "undid"),
        ("unpin", "unpinned"),
        ("write", "wrote"),
    ];

    /// The regular past tense: `-d` after an `e`, `-ied` for a `y` after a consonant, the last
    /// consonant doubled after one short vowel in one syllable ("pin", "stop"), else `-ed`.
    fn regular(verb: &str) -> String {
        let vowel = |c: char| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u');
        let chars: Vec<char> = verb.chars().collect();
        match chars.as_slice() {
            [.., 'e'] => format!("{verb}d"),
            [.., before, 'y'] if !vowel(*before) => {
                format!("{}ied", verb.strip_suffix('y').unwrap_or(verb))
            }
            [.., before, middle, last]
                if !vowel(*before)
                    && vowel(*middle)
                    && !vowel(*last)
                    && !matches!(last, 'w' | 'x' | 'y')
                    && chars.iter().filter(|c| vowel(**c)).count() == 1 =>
            {
                format!("{verb}{last}ed")
            }
            _ => format!("{verb}ed"),
        }
    }

    /// The things a kind names, as a person says them: its own name, plural and lower case
    /// (`tasks.Task` is "tasks", `mail.Mailbox` "mailboxes").
    pub(super) fn things(kind: &str) -> String {
        let name = kind
            .rsplit(['.', ':'])
            .next()
            .unwrap_or(kind)
            .trim()
            .to_lowercase();
        if name.is_empty() {
            return name;
        }
        let chars: Vec<char> = name.chars().collect();
        let vowel = |c: char| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u');
        match chars.as_slice() {
            [.., before, 'y'] if !vowel(*before) => {
                format!("{}ies", name.strip_suffix('y').unwrap_or(&name))
            }
            [.., 's' | 'x' | 'z'] | [.., 'c' | 's', 'h'] => format!("{name}es"),
            _ => format!("{name}s"),
        }
    }
}

/// The most characters of a query or an address a step's note carries: enough
/// to say what the step is about, short enough to read as one line.
const MOST_SUBJECT_CHARS: usize = 80;

/// `text` on one line (its runs of whitespace as one space each) of at most
/// [`MOST_SUBJECT_CHARS`], with an ellipsis where it was cut.
fn bounded(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut bounded: String = one_line.chars().take(MOST_SUBJECT_CHARS).collect();
    if one_line.chars().count() > MOST_SUBJECT_CHARS {
        bounded.push('\u{2026}');
    }
    bounded
}

/// Make `locale` the language the person chose, as their settings say it, for
/// as long as the returned scope lives: a test then drives the resolution a
/// desktop does, not only the catalogue.
#[cfg(test)]
pub(crate) fn chosen(locale: &str) -> settings_store::SourceScope {
    let document = serde_json::json!({"appearance": {"language": locale}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    settings_store::scoped_source(Some(std::sync::Arc::new(
        settings_store::HostDocument::new(document, |_| Ok(())),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every message the agent can say exists in every language it ships, so a
    /// German desktop never reads one English sentence among German ones.
    #[test]
    fn every_sentence_is_written_in_every_language() {
        let keys = [
            "agent-plan-model",
            "agent-plan-words-asked",
            "agent-plan-words-no-model",
            "agent-plan-no-action",
            "agent-plan-unmatched-asked",
            "agent-plan-no-model",
            "agent-plan-model-failed",
            "agent-done-words-asked",
            "agent-done-words-no-model",
            "agent-match-nothing",
            "agent-match-several",
            "agent-match-one-of",
            "agent-match-needs-text",
            "agent-run-reading-actions",
            "agent-run-step",
            "agent-model-turn-completed",
            "agent-step-searching",
            "agent-step-reading",
            "agent-step-editing",
            "agent-step-exploring",
            "agent-step-program",
            "agent-step-looked-up",
            "agent-step-on",
            "agent-step-searched",
            "agent-step-read",
            "agent-run-needs-decision",
            "agent-run-directions-require-review",
            "agent-run-cancelled-before-planning",
            "agent-run-cancelled-before-action",
            "agent-run-cancelled-in-action",
            "agent-cancel-task-center",
            "agent-cancel-thinking",
            "agent-cancel-superseded",
            "agent-fail-no-terminal-state",
            "agent-fail-local-only",
            "agent-fail-arguments",
            "agent-fail-turns",
            "agent-fail-time",
            "agent-fail-budget",
            "agent-fail-turn-not-finished",
            "agent-fail-native-outcome",
            "agent-fail-direction-pages",
        ];
        for (locale, source) in [
            ("en-US", include_str!("../locales/en-US.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ] {
            let messages = tilo_copy_lint::catalog::messages(source);
            for key in keys {
                let text = messages.get(key).unwrap_or_else(|| {
                    panic!("{locale} has no {key}");
                });
                assert!(!text.trim().is_empty(), "{locale}: {key} says nothing");
            }
        }
    }

    /// The language a person chose in Settings is the one a run speaks: the
    /// choice is read through their settings, as every app reads it.
    #[test]
    fn the_persons_choice_is_the_language_a_run_speaks() {
        let _german = chosen("de-DE");
        follow();
        assert_eq!(ui::i18n::I18nManager::current_locale(), "de-DE");
        assert_eq!(
            t("agent-match-nothing"),
            "Das kann ich hier nicht erledigen."
        );
    }

    /// A step is said in the language the run is speaking, with the thing it is
    /// about inside the sentence.
    #[test]
    fn a_step_is_said_in_the_language_of_the_run() {
        ui::i18n::I18nManager::init("en-US");
        assert_eq!(
            step(&TaskStep::Editing("Notizen".into())),
            "Editing Notizen…"
        );
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_eq!(
            step(&TaskStep::Editing("Notizen".into())),
            "Ändert Notizen…"
        );
        assert_eq!(
            step(&TaskStep::Searching("borrow checker".into())),
            "Sucht im Web nach „borrow checker“…"
        );
    }

    /// A step line says what happened, in the person's words: any action's label said as done,
    /// with the title of what it acted on where the label leaves room for it — never the
    /// action's label as it is, and never the same line whatever it acted on.
    #[test]
    fn a_step_says_what_it_did_to_what() {
        ui::i18n::I18nManager::init("en-US");
        for (label, subject, said) in [
            (
                "Show a resource on the canvas, as one of its views",
                "All inboxes",
                "Showed All inboxes on the canvas",
            ),
            ("Show", "All inboxes", "Showed All inboxes"),
            ("Look up", "your tasks", "Looked up your tasks"),
            ("Close a place", "Q3 plan", "Closed Q3 plan"),
            (
                "Pin a place, so an agent's arrangement leaves it alone",
                "Notes",
                "Pinned Notes",
            ),
            ("Move to Done", "Buy milk", "Moved Buy milk to Done"),
            (
                "Mark as read",
                "Dinner on Friday?",
                "Marked Dinner on Friday? as read",
            ),
            ("Reply", "Mia", "Replied Mia"),
            ("Turn Wi-Fi on or off", "Network", "Turned Wi-Fi on or off"),
            ("Open Mail (opens so.tilo.lang.mail)", "Mail", "Opened Mail"),
            ("Find tasks", "", "Found tasks"),
            ("New note", "Groceries", "Made a new note"),
            ("Undo", "", "Undid"),
            ("move_card", "Write the notes", "move_card"),
            (
                "Searching the web for “x”…",
                "",
                "Searching the web for “x”…",
            ),
        ] {
            assert_eq!(did(label, subject), said, "{label} on {subject}");
        }
        assert_eq!(
            looked_up(Some("tl.so.tilo.lang.tasks.Task")),
            "Looked up your tasks"
        );
        assert_eq!(looked_up(Some("mail.Mailbox")), "Looked up your mailboxes");
        assert_eq!(looked_up(Some("notes.Entry")), "Looked up your entries");
        assert_eq!(looked_up(None), "Looked things up");
        assert_eq!(
            step_done(&TaskStep::Searching("borrow checker".into())),
            "Searched the web for “borrow checker”"
        );
        // Another language says the app's label as it is, with what it acted on beside it.
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_eq!(
            did("Anzeigen", "Alle Posteingänge"),
            "Anzeigen: Alle Posteingänge"
        );
    }

    /// A long query stays one line: the note is a line, not a paragraph.
    #[test]
    fn a_long_subject_is_cut_to_one_line() {
        ui::i18n::I18nManager::init("en-US");
        let note = step(&TaskStep::Reading(format!(
            "https://example.org/{}",
            "a".repeat(500)
        )));
        assert!(note.chars().count() <= 110, "{note}");
        assert!(!note.contains('\n'), "{note}");
    }

    const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");
    const DE_FTL: &str = include_str!("../locales/de-DE.ftl");

    /// Every message the source catalogue has, the German one has too, so a
    /// German conversation never mixes languages.
    #[test]
    fn the_german_catalogue_covers_every_message() {
        let english = tilo_copy_lint::catalog::messages(SOURCE_FTL);
        let german = tilo_copy_lint::catalog::messages(DE_FTL);
        assert!(!english.is_empty());
        for id in english.keys() {
            assert!(german.contains_key(id), "de-DE lacks {id}");
        }
        for id in german.keys() {
            assert!(english.contains_key(id), "en-US lacks {id}");
        }
    }

    /// Every message the source catalogue has reads as a sentence a person
    /// could be shown: it carries nothing only the agent's record needs.
    #[test]
    fn no_failure_sentence_carries_the_runs_record() {
        let messages = tilo_copy_lint::catalog::messages(SOURCE_FTL);
        assert!(!messages.is_empty());
        for (id, said) in &messages {
            assert!(!said.trim().is_empty(), "{id} says nothing");
            assert!(!said.contains("Retry inference"), "{id}: {said}");
            assert!(!said.contains("replay completed actions"), "{id}: {said}");
        }
    }
}
