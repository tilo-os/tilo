//! One turn of a conversation, as it stands on the canvas where it was asked (canvas.md §3).

use super::{
    Channel, Gap, Message, Node, Person, ResourceRef, Value, ViewCx, ViewResource, attachment_row,
    failed_row, node, sdk, ui, view,
};

/// One turn of a conversation with an agent (canvas.md §3): the message a person asked in, and
/// what the agent answered — its replies and what they did. A place on the canvas shows it where
/// the person asked, so an answer stands where it was asked rather than in a chat window; the
/// whole conversation is the channel, which the turn opens on request.
///
/// A turn is a resource: a place names it by the message that asked
/// (`chat.workspace.turns:<message>`), the session publishes its reading, and [`turn`] draws it.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Turn {
    /// The message that asked, by its id: what the turn is named by.
    #[serde(default)]
    pub id: String,
    /// The conversation it is in, by its id after `chat.workspace/channels/`.
    #[serde(default)]
    pub channel: String,
    /// The people its messages name.
    #[serde(default)]
    pub people: Vec<Person>,
    /// What the person asked; none for a turn not read yet.
    #[serde(default)]
    pub question: Option<Message>,
    /// What the agent answered, oldest first: every message after the question until a person
    /// spoke again.
    #[serde(default)]
    pub replies: Vec<Message>,
}

impl Turn {
    /// The turn the message `question` of `channel` opened: the message, and the agents'
    /// messages after it until a person wrote again. `None` when the channel holds no such
    /// message.
    #[must_use]
    pub fn of(channel: &Channel, question: &str) -> Option<Self> {
        let at = channel
            .messages
            .iter()
            .position(|message| message.id == question)?;
        let asked = channel.messages.get(at)?.clone();
        let replies: Vec<Message> = channel
            .messages
            .iter()
            .skip(at + 1)
            .take_while(|message| {
                channel
                    .person(&message.author)
                    .map_or_else(|| !message.author.is_person(), Person::is_agent)
            })
            .cloned()
            .collect();
        let mut people: Vec<Person> = Vec::new();
        for message in std::iter::once(&asked).chain(&replies) {
            if let Some(person) = channel.person(&message.author)
                && !people.iter().any(|held| held.id == person.id)
            {
                people.push(person.clone());
            }
        }
        Some(Self {
            id: asked.id.clone(),
            channel: channel.id.clone(),
            people,
            question: Some(asked),
            replies,
        })
    }

    /// The conversation the turn is in, as the channel resource Chat publishes: what the turn
    /// opens when the person asks for the whole conversation.
    #[must_use]
    pub fn conversation(&self) -> ResourceRef {
        ResourceRef {
            id: super::channel_address(&self.channel),
            ty: <Channel as ViewResource>::KIND.to_owned(),
        }
    }
}

/// The kind a turn is published as (`chat.workspace.turns:<message>`).
pub const TURN_KIND: &str = "chat.workspace.turns";

impl ViewResource for Turn {
    // A turn of a conversation, as the session publishes it (`chat.workspace.turns:<message>`).
    const KIND: &'static str = TURN_KIND;
    const APP: &'static str = "chat";
}

/// A turn, as it stands on the canvas where it was asked (canvas.md §3, §7 "An answer reads like
/// an answer"): the person's words as a quiet line (the caption its place shows over it, the
/// turn's title, so the card does not say them twice), the agent's reply as prose at body
/// size with its Markdown set as text (never as its source), and what the agent did as one quiet
/// line that unfolds. While the agent works, one live line says what it is doing now, from the
/// step it took last, in place of a count. No byline, no avatars, no composer and no button: it
/// is one exchange, and the whole conversation is reached from the place's handle.
///
/// The turn holds its words and nothing else, so its place hugs them (`canvas.tl`'s `hugs`).
#[view(default)]
pub(super) fn turn(t: &Turn, cx: &ViewCx) -> Node {
    // The person's words are the place's own quiet line, over the card (its caption, the turn's
    // title): the card holds the answer.
    let mut column: Vec<Node> = Vec::new();
    let mut steps: Vec<String> = Vec::new();
    // What the agent is doing now, while it works: the line under the words it has so far.
    let mut working: Option<String> = None;
    for reply in &t.replies {
        if let Some(work) = &reply.work {
            steps.extend(work.steps.iter().cloned());
            if work.writing() {
                working = Some(live_line(work));
            }
        }
        if !reply.text.trim().is_empty() {
            column.push(prose(
                &cx.key(format!("reply-{}", reply.id)),
                reply.text.trim(),
            ));
        }
        for (index, attachment) in reply.attachments.iter().enumerate() {
            column.push(attachment_row(
                attachment,
                &cx.key(format!("attachment-{}-{index}", reply.id)),
                cx,
            ));
        }
        if let Some(work) = &reply.work
            && let Some(answered) = &work.answered
        {
            // Which model wrote this answer, what it cost and used: kept with the reply, so a
            // turn read later still names the model that wrote it.
            column.push(
                ui::TextBuilder::new(
                    &cx.key(format!("answered-{}", reply.id)),
                    answered.summary(),
                )
                .role(ui::TextRole::Caption)
                .secondary(true)
                .into(),
            );
        }
        if let Some(work) = &reply.work
            && work.failed()
            && reply.text.trim().is_empty()
        {
            // Only when the reply itself says nothing does the failure stand as its own card:
            // a reply that already says what went wrong in the person's words (the agent's own
            // sentence) keeps its details in the steps disclosure, not a second surface saying
            // the same thing again.
            column.push(failed_row(reply, work, cx));
        }
    }
    if t.replies.is_empty() {
        // Asked, and nothing answered yet: the turn says it is thinking where the answer will
        // stand, so the person sees where it lands.
        working = Some(THINKING.to_owned());
    }
    if let Some(line) = working {
        let spinner: Node = ui::SpinnerBuilder::new(&cx.key("working-spinner"))
            .label(line.clone())
            .beside(ui::TextRole::Caption)
            .into();
        let said: Node = ui::TextBuilder::new(&cx.key("working"), &line)
            .role(ui::TextRole::Caption)
            .secondary(true)
            .into();
        column.push(ui::row(
            &cx.key("working-row"),
            Gap::Tight,
            vec![spinner, said],
        ));
    } else if !steps.is_empty() {
        let folded: Node = ui::DisclosureRowBuilder::new(&cx.key("steps"), steps_line(&steps))
            .dense(true)
            .into();
        let lines: Vec<Node> = steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                ui::TextBuilder::new(&cx.key(format!("step-{index}")), step)
                    .role(ui::TextRole::Caption)
                    .secondary(true)
                    .into()
            })
            .collect();
        column.push(node::with(folded, lines));
    }
    // A card's padding on every side, as an object's card keeps it: the turn stands on the
    // canvas's surface with no frame, so its words keep their room from its edges. Not a page:
    // a page takes the height its pane gives it, where the turn hugs its words.
    let mut card = ui::stack(&cx.key("turn"), Gap::Related, column);
    card.props
        .insert("padding".to_owned(), Value::text("section"));
    // The card scrolls its words once they come to more than the canvas shows: its place hugs
    // them up to the visible area and no further (canvas.md §7, "Everything stays in view"), and
    // the words wrap at the card's width and scroll inside it. Its bar stands while they
    // overflow, as a list's beside it does, so a capped answer says there is more below.
    let scroll = ui::ScrollBuilder::new(&cx.key("turn-scroll"))
        .shows_bar(true)
        .into();
    node::with(scroll, vec![card])
}

/// What a turn says while the agent has taken no step and said nothing of what it does.
const THINKING: &str = "Thinking\u{2026}";

/// The agent's reply as it reads: its Markdown as text, at body size (`**bold**` bold, a list a
/// list, a heading a heading), through the toolkit's own reading of Markdown
/// (`BlockEditor::reading_markdown`), shown read-only as a document is.
pub(super) fn prose(key: &str, markdown: &str) -> Node {
    let reading = sdk::rich_edit::BlockEditor::reading_markdown(markdown);
    let blocks: Vec<sdk::rich_edit::BlockContent> = reading
        .blocks()
        .iter()
        .map(sdk::rich_edit::Block::content)
        .collect();
    let rich = sdk::rich_edit::rich_doc::rich_of_blocks(
        &blocks,
        &tilo_view::patch::rich::RichText::default(),
    );
    ui::BlockEditorBuilder::new(key).value(rich).into()
}

/// What the agent is doing now, in one line beside its spinner: the step it took last, as the
/// agent said it ("Looked up Tasks"); what the run said is under way when it took no step
/// yet; else that it is thinking.
pub(super) fn live_line(work: &super::Work) -> String {
    if let Some(step) = work
        .steps
        .last()
        .map(|step| step.trim())
        .filter(|step| !step.is_empty())
    {
        return step.to_owned();
    }
    let detail = work.detail.trim();
    if detail.is_empty() {
        THINKING.to_owned()
    } else {
        detail.to_owned()
    }
}

/// What the agent did, in one quiet line: its one step as its app names it, or the first and how
/// many more, each of them listed when the line unfolds.
pub(super) fn steps_line(steps: &[String]) -> String {
    match steps {
        [] => String::new(),
        [one] => one.clone(),
        [first, rest @ ..] if rest.len() == 1 => format!("{first} and 1 more step"),
        [first, rest @ ..] => format!("{first} and {} more steps", rest.len()),
    }
}

#[cfg(test)]
#[path = "views_turn_tests.rs"]
mod tests;
