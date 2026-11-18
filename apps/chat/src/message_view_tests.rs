//! A view on a message: who posts one, answers with one and writes one
//! again, whose it is, what is refused, and what a reader that shows no view
//! says of it.

use automerge::AutoCommit;
use ui::tilo_resource::{Id, MemberId, NonBlank, Replica, Resource as _};

use crate::demo::apply_part_doc_as;
use crate::message_view::{MAX_EARLIER, MAX_READS, MAX_SOURCE_BYTES, View, line, named};
use crate::messages::chat_part;
use crate::model::ME;
use crate::parts::ChatPart;
use crate::resource::Chat;

/// The agent the person talks with.
const AGENT: &str = "tide";
/// A program the person set up: a bot installation of theirs.
const BOT: &str = "b_events";
/// Another member of the workspace.
const ARIA: &str = "u_aria";
const GENERAL: &str = "c_general";

/// A view over what Tasks keeps for the workspace.
const PROJECTS: &str =
    "view projects() {\n    for a in tasks.tracker.areas {\n        text(a.name)\n    }\n}";
/// The same question answered another way.
const NAMES: &str =
    "view names() {\n    for a in tasks.tracker.areas {\n        row(a.name)\n    }\n}";

/// Who a verb is attributed to, as the workspace admits them.
#[derive(Clone, Copy)]
enum By {
    /// The person, themselves.
    Person(&'static str),
    /// An agent acting for a person.
    AgentFor(&'static str, &'static str),
    /// The bot [`BOT`], installed by a person and acting for them.
    BotOf(&'static str),
}

/// One part of a chat's messages, written by whoever a test says, each
/// action a second after the one before.
struct Part {
    doc: AutoCommit,
    chat: Chat,
    now_ms: i64,
}

impl Part {
    fn new() -> Self {
        Self {
            doc: AutoCommit::new(),
            chat: crate::demo::demo(),
            now_ms: 1_000_000,
        }
    }

    fn apply(
        &mut self,
        by: By,
        invoke: impl Into<ui::tilo_resource::Invoke<ChatPart>>,
    ) -> Result<(), ui::tilo_actions::ApiError> {
        let attribute = move |replica: Replica<ChatPart>| match by {
            By::Person(person) => replica.as_invoker(person),
            By::AgentFor(agent, person) => replica.as_agent_for(agent, person),
            By::BotOf(person) => replica.as_bot_for(BOT, person),
        };
        self.now_ms += 1_000;
        apply_part_doc_as(
            &mut self.doc,
            &self.chat,
            attribute,
            Some(self.now_ms),
            invoke,
        )
    }

    fn read(&self) -> ChatPart {
        ChatPart::read(&self.doc).expect("the part reads")
    }

    /// The view the message `id` carries now.
    fn view(&self, id: &Id) -> Option<View> {
        self.read()
            .messages
            .get(id)
            .expect("the message is there")
            .view
            .clone()
    }

    /// The id of the newest message.
    fn newest(&self) -> Id {
        self.read()
            .messages
            .ids()
            .last()
            .cloned()
            .expect("a message was posted")
    }

    /// The id of the reply `agent` wrote to the message `to`.
    fn reply_of(&self, agent: &str, to: &Id) -> Id {
        let author = MemberId::agent(agent);
        self.read()
            .messages
            .iter()
            .find(|(_, held)| held.author == author && held.parent.as_ref() == Some(to))
            .map(|(id, _)| id.clone())
            .expect("the agent replied")
    }
}

fn names(apps: &[&str]) -> Vec<String> {
    apps.iter().map(|app| (*app).to_owned()).collect()
}

fn post_view(source: &str, reads: &[&str]) -> ui::tilo_resource::Invoke<ChatPart> {
    ChatPart::POST_VIEW.with(chat_part::PostView {
        channel: Id::from_raw(GENERAL),
        text: NonBlank::parse("The projects, as they stand").expect("words"),
        source: source.to_owned(),
        reads: names(reads),
        parent: None,
    })
}

fn answer(
    message: &Id,
    source: &str,
    reads: &[&str],
    say: Option<&str>,
) -> ui::tilo_resource::Invoke<ChatPart> {
    ChatPart::ANSWER_WITH_VIEW.with(chat_part::AnswerWithView {
        message: message.clone(),
        source: source.to_owned(),
        reads: names(reads),
        say: say.map(str::to_owned),
    })
}

fn rewrite(message: &Id, source: &str, reads: &[&str]) -> ui::tilo_resource::Invoke<ChatPart> {
    ChatPart::REWRITE_VIEW.with(chat_part::RewriteView {
        message: message.clone(),
        source: source.to_owned(),
        reads: names(reads),
    })
}

/// The person's own conversation with their agent, holding their question:
/// the part and the question's id.
fn asked() -> (Part, Id) {
    let mut part = Part::new();
    let thread = Chat::direct_id([MemberId::person(ME), MemberId::agent(AGENT)]);
    part.apply(
        By::Person(ME),
        crate::demo::post(thread.as_str(), "Which projects are there?", None),
    )
    .expect("the person asks their agent");
    let question = part.newest();
    (part, question)
}

/// A view that reads fields the ways the guide names: an option, a list and its length.
const BY_TYPE: &str = "view people() {\n    for a in tasks.tracker.areas {\n        text(a.name ?? \"unnamed\")\n        text(a.people.len().to_string())\n    }\n}";

#[test]
fn the_guide_says_how_a_view_reads_a_field_by_its_type_and_chat_takes_such_a_view() {
    // What a model is told: each way a field is read, by its type.
    for way in [
        "`??`",
        "`if let v = x {}`",
        "`for i in xs {}`",
        "`.len()`",
        "`.map()`",
        "`.field`",
        "`== .name`",
    ] {
        assert!(
            crate::message_view::GUIDE.contains(way),
            "the guide names {way}"
        );
    }
    // And what a reader meets where a field cannot be read.
    assert!(
        crate::message_view::GUIDE.contains("fails alone and says so in the view's place"),
        "the guide says what a field that cannot be read does"
    );
    // Chat takes a view written that way under the app's name alone.
    let view = View::written(BY_TYPE.to_owned(), vec!["tasks".to_owned()], 5).expect("a view");
    assert_eq!(view.reads, ["tasks"]);
}

#[test]
fn a_posted_view_is_state_of_its_message_and_its_author_is_whoever_invoked() {
    let mut part = Part::new();
    part.apply(By::Person(ME), post_view(PROJECTS, &["tasks", "tasks"]))
        .expect("a person posts a view");
    let held = part.read();
    let message = held.messages.get(&part.newest()).expect("the message");
    let view = message.view.as_ref().expect("the message carries the view");
    assert_eq!(view.source, PROJECTS);
    assert_eq!(view.reads, ["tasks"], "each app once");
    assert_eq!((view.version, view.at), (1, 1_001));
    assert!(view.earlier.is_empty());
    // The words are the message's own: what every reader has.
    assert_eq!(message.text, "The projects, as they stand");
    // Whose code it is: the message's author, pinned from who invoked. The
    // verb takes no author, and the view keeps none.
    assert_eq!(message.author, MemberId::person(ME));
    assert!(message.acting.is_none(), "a person's own words");

    // What software posts for a person is the software's, never the
    // person's own code.
    for by in [By::AgentFor(AGENT, ME), By::BotOf(ME)] {
        part.apply(by, post_view(PROJECTS, &["tasks"]))
            .expect("software posts a view for its person");
        let held = part.read();
        let message = held.messages.get(&part.newest()).expect("the message");
        assert!(message.view.is_some());
        assert!(!message.author.is_person());
        assert!(
            message.acting.is_some(),
            "the message says what wrote it for whom"
        );
    }
}

#[test]
fn a_view_that_shows_nothing_is_too_long_or_lists_what_it_does_not_name_is_refused() {
    let seventeen: Vec<String> = (0..=MAX_READS).map(|n| format!("app{n}")).collect();
    let naming_seventeen = seventeen
        .iter()
        .map(|app| format!("text({app}.name)"))
        .collect::<Vec<_>>()
        .join("\n");
    let listed: Vec<&str> = seventeen.iter().map(String::as_str).collect();
    let too_long = format!("{PROJECTS}\n{}", "/".repeat(MAX_SOURCE_BYTES));
    let refused: [(&str, &[&str], &str); 6] = [
        ("", &[], "empty"),
        ("  \n\t", &[], "empty"),
        (&too_long, &["tasks"], "too long"),
        (PROJECTS, &["tasks", "mail"], "does not name mail"),
        (PROJECTS, &["tasks", ""], "named as the view calls it"),
        (&naming_seventeen, &listed, "at most 16"),
    ];
    for (source, reads, why) in refused {
        let mut part = Part::new();
        let error = part
            .apply(By::Person(ME), post_view(source, reads))
            .expect_err("the view is refused");
        assert!(
            format!("{error:?}").contains(why),
            "refused for another reason than {why:?}: {error:?}"
        );
        assert!(part.read().messages.is_empty(), "nothing was posted");
    }

    // An answer that is refused leaves no reply behind either.
    let (mut part, question) = asked();
    part.apply(
        By::AgentFor(AGENT, ME),
        answer(&question, PROJECTS, &["mail"], Some("Your projects.")),
    )
    .expect_err("a view that lists what it does not name");
    assert_eq!(part.read().messages.len(), 1, "only the question stands");
}

#[test]
fn an_agent_answers_with_a_view_on_its_own_reply_to_the_message_that_asked_it() {
    let (mut part, question) = asked();
    part.apply(
        By::AgentFor(AGENT, ME),
        answer(
            &question,
            PROJECTS,
            &["tasks"],
            Some("Your projects, as they stand."),
        ),
    )
    .expect("the agent answers with a view");

    // The view is on the agent's own reply to that message, the one its
    // work is kept on, so a turn's work and its view are one message.
    let reply = part.reply_of(AGENT, &question);
    let held = part.read();
    let message = held.messages.get(&reply).expect("the reply");
    assert!(
        message.acting.is_some(),
        "written by the agent for its person"
    );
    assert_eq!(message.text, "Your projects, as they stand.");
    let view = message.view.as_ref().expect("the reply carries the view");
    assert_eq!((view.version, view.source.as_str()), (1, PROJECTS));
    assert_eq!(view.reads, ["tasks"]);
    assert_eq!(held.messages.len(), 2, "one question, one reply");

    // Answered again in the same turn, it is the next version of the same
    // reply's view, and the reply keeps the words it had.
    part.apply(
        By::AgentFor(AGENT, ME),
        answer(&question, NAMES, &["tasks"], Some("Ignored: it has words.")),
    )
    .expect("the agent writes its answer's view again");
    let held = part.read();
    assert_eq!(held.messages.len(), 2, "still one reply");
    let message = held.messages.get(&reply).expect("the reply");
    assert_eq!(message.text, "Your projects, as they stand.");
    let view = message.view.as_ref().expect("the view");
    assert_eq!((view.version, view.source.as_str()), (2, NAMES));
    assert_eq!(view.earlier.len(), 1);
    assert_eq!(
        (view.earlier[0].version, view.earlier[0].source.as_str()),
        (1, PROJECTS)
    );

    // Another agent's answer to the same message is a reply of its own.
    part.apply(
        By::AgentFor("scout", ME),
        answer(&question, PROJECTS, &["tasks"], None),
    )
    .expect("another agent answers");
    assert_eq!(part.read().messages.len(), 3);
    assert_eq!(part.view(&reply).expect("untouched").version, 2);
    let theirs = part.reply_of("scout", &question);
    assert_eq!(part.view(&theirs).expect("their own").version, 1);
}

#[test]
fn only_its_author_writes_a_view_again_and_the_forms_before_are_kept() {
    let mut part = Part::new();
    part.apply(By::Person(ME), post_view(PROJECTS, &["tasks"]))
        .expect("a person posts a view");
    let id = part.newest();

    // Nobody else writes it again: not another member, and not what acts
    // for its author, whose code would then stand as the person's own.
    for other in [By::Person(ARIA), By::AgentFor(AGENT, ME), By::BotOf(ME)] {
        let error = part
            .apply(other, rewrite(&id, NAMES, &["tasks"]))
            .expect_err("only its author writes a view again");
        assert!(
            format!("{error:?}").contains("only its author"),
            "{error:?}"
        );
    }
    let view = part.view(&id).expect("the view");
    assert_eq!((view.version, view.source.as_str()), (1, PROJECTS));

    // Its author does, and the form before is kept with its time.
    part.apply(By::Person(ME), rewrite(&id, NAMES, &["tasks"]))
        .expect("its author writes it again");
    let view = part.view(&id).expect("the view");
    assert_eq!((view.version, view.source.as_str()), (2, NAMES));
    assert_eq!(view.earlier.len(), 1);
    assert_eq!(view.earlier[0].source, PROJECTS);
    assert!(
        view.at > view.earlier[0].at,
        "each version says when it was written"
    );
    assert!(
        !part.read().messages.get(&id).expect("the message").edited,
        "its words were not changed"
    );

    // The same writing again is no new version.
    part.apply(By::Person(ME), rewrite(&id, NAMES, &["tasks"]))
        .expect("the same again changes nothing");
    assert_eq!(part.view(&id).expect("the view").version, 2);

    // A refused writing leaves the view as it was.
    part.apply(By::Person(ME), rewrite(&id, PROJECTS, &["mail"]))
        .expect_err("it lists what it does not name");
    assert_eq!(part.view(&id).expect("the view").version, 2);

    // The forms before are bounded: the oldest go first.
    let more = MAX_EARLIER + 3;
    for n in 0..more {
        part.apply(
            By::Person(ME),
            rewrite(&id, &format!("{PROJECTS}\n// {n}"), &["tasks"]),
        )
        .expect("written again");
    }
    let view = part.view(&id).expect("the view");
    let newest = 2 + u32::try_from(more).expect("small");
    assert_eq!(view.version, newest);
    assert_eq!(view.earlier.len(), MAX_EARLIER);
    assert_eq!(view.earlier.last().expect("kept").version, newest - 1);
    assert_eq!(
        view.earlier.first().expect("kept").version,
        newest - u32::try_from(MAX_EARLIER).expect("small")
    );

    // A message that carries no view has none to write again.
    part.apply(
        By::Person(ME),
        crate::demo::post(GENERAL, "plain words", None),
    )
    .expect("a plain message");
    let plain = part.newest();
    let error = part
        .apply(By::Person(ME), rewrite(&plain, NAMES, &["tasks"]))
        .expect_err("no view to write again");
    assert!(
        format!("{error:?}").contains("carries no view"),
        "{error:?}"
    );
}

#[test]
fn a_reader_that_shows_no_view_has_the_words_and_a_line_saying_what_it_is_a_view_of() {
    crate::i18n::ensure();
    // Fluent sets what it fills in apart with direction marks.
    let said = |view: &View| line(view).replace(['\u{2068}', '\u{2069}'], "");
    let of_tasks = View::written(PROJECTS.to_owned(), names(&["tasks"]), 5).expect("a view");
    assert_eq!(said(&of_tasks), "A view of Tasks is attached");
    let of_two = View::written(
        "view both() {\n    text(tasks.tracker.name)\n    text(word_processor.documents.name)\n}"
            .to_owned(),
        names(&["tasks", "word_processor"]),
        5,
    )
    .expect("a view");
    assert_eq!(said(&of_two), "A view of Tasks, Word processor is attached");
    let of_nothing = View::written("view hello() { text(\"Hello\") }".to_owned(), Vec::new(), 5)
        .expect("a view");
    assert_eq!(said(&of_nothing), "A view is attached");

    // The window's row is read from the same message: it carries the view
    // beside the words.
    let mut part = Part::new();
    part.apply(By::Person(ME), post_view(PROJECTS, &["tasks"]))
        .expect("a person posts a view");
    let id = part.newest();
    let held = part.read();
    let message = held.messages.get(&id).expect("the message");
    let row = crate::projection::message_row(&id, message);
    assert_eq!(row.view, message.view);
    assert_eq!(row.text, "The projects, as they stand");
}

#[test]
fn every_name_before_a_dot_is_a_name_the_view_can_reach_an_app_by() {
    assert_eq!(
        named(
            "view v() {\n    for a in tasks.tracker.areas { text(a.name) }\n    text(mail.inbox.title)\n    text(tasks.tracker.name)\n}"
        ),
        ["tasks", "tracker", "a", "mail", "inbox"]
    );
    assert!(named("view v() { text(\"Hello\") }").is_empty());
}

/// One document of the word processor, named in a view as the language names any resource.
const POEM: &str = "view poem() {\n    text(@word_processor.Document:uuid:0f2b1c00-0000-4000-8000-000000000001.body.text)\n}";

#[test]
fn the_guide_says_how_a_view_names_one_document_and_chat_takes_such_a_view() {
    // What a model is told: the reference, and where the document's id is found.
    assert!(
        crate::message_view::GUIDE.contains("@word_processor.Document:uuid:"),
        "the guide shows one document named in a view"
    );
    assert!(
        crate::message_view::GUIDE.contains("the last part of the link"),
        "and says where the document's id is found"
    );
    // Chat takes such a view under the app's name alone.
    let view = View::written(POEM.to_owned(), names(&["word_processor"]), 5).expect("a view");
    assert_eq!(view.reads, ["word_processor"]);
}
