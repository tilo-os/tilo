//! One turn of a conversation, as it stands on the canvas where it was asked (canvas.md §3).

use super::{
    Channel, Gap, Message, Node, Person, ResourceRef, ViewCx, ViewResource, attachment_row,
    failed_row, is_agent, node, ui, view, writing_row,
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
    /// What the person asked.
    #[serde(default)]
    pub question: Message,
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
                    .map_or_else(|| is_agent(&message.author), |person| person.agent)
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
            question: asked,
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

/// A turn, as it stands on the canvas where it was asked (canvas.md §3): the person's words as a
/// quiet line, the agent's reply under it at reading size, and what the agent did folded under
/// that. No byline, no avatars and no composer: it is one exchange, not a conversation, and the
/// whole conversation opens from its foot.
#[view(default)]
pub(super) fn turn(t: &Turn, cx: &ViewCx) -> Node {
    let mut column: Vec<Node> = vec![
        ui::TextBuilder::new(&cx.key("asked"), t.question.text.trim())
            .role(ui::TextRole::Label)
            .secondary(true)
            .into(),
    ];
    let mut steps: Vec<String> = Vec::new();
    for reply in &t.replies {
        if let Some(work) = &reply.work {
            steps.extend(work.steps.iter().cloned());
            if work.writing() {
                column.push(writing_row(reply, work, cx));
            }
        }
        if !reply.text.trim().is_empty() {
            column.push(
                ui::TextBuilder::new(&cx.key(format!("reply-{}", reply.id)), reply.text.trim())
                    .role(ui::TextRole::Body)
                    .into(),
            );
        }
        for (index, attachment) in reply.attachments.iter().enumerate() {
            column.push(attachment_row(
                attachment,
                &cx.key(format!("attachment-{}-{index}", reply.id)),
                cx,
            ));
        }
        if let Some(work) = &reply.work
            && work.failed()
        {
            column.push(failed_row(reply, work, cx));
        }
    }
    if t.replies.is_empty() {
        // Asked, and nothing answered yet: the turn says it is waiting where the answer will
        // stand, so the person sees where it lands.
        let waiting: Node = ui::SpinnerBuilder::new(&cx.key("waiting"))
            .label("Thinking…")
            .beside(ui::TextRole::Caption)
            .into();
        column.push(waiting);
    }
    if !steps.is_empty() {
        let label = if steps.len() == 1 {
            "1 step".to_owned()
        } else {
            format!("{} steps", steps.len())
        };
        let folded: Node = ui::DisclosureRowBuilder::new(&cx.key("steps"), label).into();
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
    // The whole conversation is the channel underneath: it opens as content on request, beside
    // the turn, through the system's open.
    let whole: Node = ui::ButtonBuilder::new(&cx.key("conversation"), "Whole conversation")
        .icon("message-square")
        .role(ui::ButtonRole::Plain)
        .into();
    let opened = cx.open(&whole, "press", t.conversation(), None);
    column.push(node::on(whole, "press", opened));
    ui::fill(ui::stack(&cx.key("turn"), Gap::Related, column))
}
