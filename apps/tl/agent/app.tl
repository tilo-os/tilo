app so.tilo.agent "Agents" (icon: .sparkles, category: .system)

/// Where an ask stands. The workspace's agent engine moves it on, except that its asker
/// withdraws it and a person stops a turn that is working.
enum AskState {
    /// Asked, and no device has taken the turn yet.
    waiting
    /// A device took the turn and the agent is working on it.
    working (icon: .refresh, tone: .info)
    /// The agent answered.
    done (icon: .circle_check, tone: .success)
    /// The turn ended without an answer; its reply says why.
    failed (icon: .triangle_alert, tone: .danger)
    /// A person stopped the turn while it worked.
    stopped (icon: .square)
    /// The asker took it back, or removed the mention that asked, before a device took it.
    withdrawn
    /// The agent may not read what it was asked about, so nothing of it was copied here.
    cannot_read (icon: .lock, tone: .warning)
}

/// Someone asked an agent something about a resource: by mentioning it in a chat message, a note
/// or a task, by writing to it directly, or from the canvas. An ask is made from the resource it
/// is about (`created(a)?.from`), and who asked is who made it (`created(a)?.who`). What the agent
/// reads around it is where that resource is: the conversation a message is in. Whoever may read
/// that resource may read the ask; nobody else.
type Ask {
    /// The agent asked.
    agent: Actor
    /// What was asked, as it read when asked: a later edit of the text asks nothing new. None
    /// when the agent may not read what it was asked about.
    said: Option<RichText> = .none
    /// The mention that asked: the agent's reference as the text marks it; empty for an ask made
    /// outright. With what the ask is made from it names the ask, so mentioning an agent in a
    /// text makes one ask however often the text is edited, undone or redone.
    mention: Text = ""
    state: AskState = .waiting
}

/// One step of a turn: an action the agent called, with what it answered.
type Step {
    /// The action it called, by its address.
    called: Text
    /// What it answered, as the agent read it.
    answered: RichText
    /// Whether it failed.
    failed: Bool = false
}

/// A view an agent answered with, as a Chat message sends one: what it is called, and the view
/// as tilolang source, drawn live for each reader as they read.
type SentView {
    title: Text
    definition: Text
}

/// A thing an agent shows in its reply: a card of it, live, which opens it. Each reader reads it
/// with what they hold: one who may not read it reads only that something is there.
type Shown {
    /// The card: one paragraph projecting the thing.
    card: RichText
}

/// An agent's answer to an ask, as it writes it. Asking again makes a new reply.
type Reply {
    ask: Ask
    /// The answer, growing while the agent writes.
    body: RichText
    /// What the agent's model thought on the way, where its provider shows it: growing while
    /// it thinks, and never part of the answer.
    thinking: Text = ""
    state: AskState = .working
    /// Which time it was asked: the first reply is 1.
    attempt: Int = 1
    /// What the agent called on the way, in order.
    steps: Collection<Step>
    /// The views it answered with, in order.
    views: Collection<SentView>
    /// The things it showed, in order.
    shown: Collection<Shown>
    /// The provider that answered the reply, as the workspace's settings name it (`Google`):
    /// empty where no model answered.
    provider: Text = ""
    /// The model that answered it, as a person reads it (`Gemini 2.5 Pro`), or its id where its
    /// provider labels it none: empty where no model answered.
    model: Text = ""
    /// What the turn's model calls cost, in millionths of the workspace's currency.
    cost: Int = 0
    /// Whether another device's answer to the same ask stands instead: two devices offline at
    /// once may both have answered.
    superseded: Bool = false
}

/// What an agent keeps of what a conversation said before the part a turn is shown, so a
/// conversation longer than a turn can keep what it said earlier. The agent writes it while it
/// answers; the next turn there is told it before the part it is shown. One per conversation
/// and agent.
type Summary {
    /// The agent that keeps it.
    agent: Actor
    /// What the conversation said before, in the agent's words.
    text: Text = ""
    /// The item it reaches: the conversation's message the agent last took in.
    through: Text = ""
    /// When that item was written, ISO-8601 UTC.
    through_at: Text = ""
}

/// Every ask and reply of the workspace, and what its agents keep of its long conversations.
type Agents {
    asks: Collection<Ask>
    replies: Collection<Reply>
    summaries: Collection<Summary>
}

root agents: Agents

default view asked for Agents

default view ask_page for Ask

row view ask_row for Ask

default view reply for Reply

/// Asks `agent` about the resource the ask is made from (`.made_from(r)`). Whoever makes it
/// decides first whether the agent may read that resource, and asks with nothing said when it
/// may not: the ask then says so, and holds nothing of the resource.
///
/// An ask made from a resource is named by it and the mention: every device that asks it so
/// makes the same ask, and asking again is that ask.
action ask(all: Agents, agent: Actor, said: RichText, mention: Text = "") -> Ask (
    icon: .sparkles,
    label: "Ask",
    did: "asked",
) {
    let a = if said.is_empty() {
        Ask(agent: agent, mention: mention, state: .cannot_read)
    } else {
        Ask(agent: agent, said: .some(said), mention: mention)
    }
    if let from = made_from {
        all.asks.insert(a, key: if mention == "" { from.id() } else { "{from.id()}|{mention}" })
    } else { all.asks.insert(a) }
}

/// Takes an ask back before a device took its turn. Only its asker may.
action withdraw(a: Ask) (icon: .undo_2, label: "Withdraw") {
    require created(a)?.who == invoker "Only who asked can withdraw it"
    require a.state == .waiting "The agent is already on it"
    a.state = .withdrawn
}

/// Asks again what was withdrawn: the mention that made it is back, as redo puts it back. Only
/// its asker may.
action reopen(a: Ask) (icon: .rotate_ccw, label: "Ask again") {
    require created(a)?.who == invoker "Only who asked can ask it again"
    require a.state == .withdrawn "It was not withdrawn"
    a.state = .waiting
}

/// Asks again for an answer that failed: the same ask, its next attempt, so the reply written
/// now stands beside the one that failed. Only its asker may.
action retry(a: Ask) (icon: .rotate_ccw, label: "Retry") {
    require created(a)?.who == invoker "Only who asked can ask it again"
    require a.state == .failed "It did not fail"
    a.state = .waiting
}

/// Stops a turn that is working; what the agent did until then stays.
action stop(a: Ask) (icon: .square, label: "Stop") {
    require a.state == .working "Nothing is working on it"
    a.state = .stopped
}

/// The agent starts its reply to `a`, the ask's next attempt: the one after its newest reply,
/// so a reply that failed and was asked again stands beside it.
action answer(a: Ask) -> Reply (icon: .sparkles, label: "Answer", did: "answered") {
    require invoker == a.agent "Only the agent asked may answer"
    require a.state == .waiting or a.state == .working "Nobody waits for an answer"
    let attempt = if let newest = latest_reply(a) { newest.attempt + 1 } else { 1 }
    a.state = .working
    agents.replies.insert(Reply(ask: a, body: RichText(""), attempt: attempt))
}

/// The agent writes `more` at the end of its answer.
action say(r: Reply, more: RichText) {
    require invoker == r.ask.agent "Only the agent asked may answer"
    require r.state == .working "The answer is finished"
    r.body = RichText(r.body, more)
}

/// The agent's model thought `more` on the way, as its provider shows it: at the end of the
/// reply's thinking, beside its answer.
action think(r: Reply, more: Text) {
    require invoker == r.ask.agent "Only the agent asked may answer"
    require r.state == .working "The answer is finished"
    r.thinking = "{r.thinking}{more}"
}

/// The agent called `called` on the way, and it answered `answered`.
action step(r: Reply, called: Text, answered: RichText, failed: Bool = false) {
    require invoker == r.ask.agent "Only the agent asked may answer"
    require r.state == .working "The answer is finished"
    r.steps.insert(Step(called: called, answered: answered, failed: failed))
}

/// The agent answers with a view: `definition`, called `title`.
action show_view(r: Reply, title: Text, definition: Text) {
    require invoker == r.ask.agent "Only the agent asked may answer"
    require r.state == .working "The answer is finished"
    require definition != "" "The view is empty"
    r.views.insert(SentView(title: title, definition: definition))
}

/// The agent shows the thing `card` projects in its answer, live.
action show(r: Reply, card: RichText) {
    require invoker == r.ask.agent "Only the agent asked may answer"
    require r.state == .working "The answer is finished"
    r.shown.insert(Shown(card: card))
}

/// The agent's turn ended as `state`: done, failed, or stopped when a person stopped it. `model`
/// of `provider` answered it, where a model did, its calls costing `cost` millionths of the
/// workspace's currency; a turn that ended before its model answered says nothing of one.
action finish(r: Reply, state: AskState, provider: Text = "", model: Text = "", cost: Int = 0) {
    require invoker == r.ask.agent "Only the agent asked may answer"
    require state == .done or state == .failed or state == .stopped "A turn ends done, failed or stopped"
    let ended = if r.ask.state == .stopped { AskState.stopped } else { state }
    r.state = ended
    r.ask.state = ended
    r.provider = provider
    r.model = model
    r.cost = cost
}

/// Another device's answer to the same ask stands instead of `r`.
action supersede(r: Reply) {
    require invoker == r.ask.agent "Only the agent asked may answer"
    r.superseded = true
}

/// The agent keeps what the conversation this is made from said before the part its turn was
/// shown: `text` says it in the agent's words, reaching `through`, the item it last took in. One
/// per conversation and agent: what it kept before is what it says now.
action keep_summary(agent: Actor, text: Text, through: Text, through_at: Text = "") -> Summary (
    icon: .file_text,
    label: "Keep a summary",
) {
    require invoker == agent "Only the agent may keep its summary"
    let wanted = Summary(agent: agent)
    let kept = if let from = made_from {
        agents.summaries.insert(wanted, key: "{from.id()}|{agent.id()}")
    } else { agents.summaries.insert(wanted) }
    kept.text = text
    kept.through = through
    kept.through_at = through_at
    kept
}

/// The asks made from `about`, oldest first: what a message, a note or a task shows under it.
fn asks_on(about: host.Resource) -> List<Ask> {
    agents.asks.filter(a -> created(a)?.from == about)
}

/// The newest reply to `a`, by its attempt: the one that says where the ask stands.
fn latest_reply(a: Ask) -> Option<Reply> {
    agents.replies.filter(r -> r.ask == a).sort_by(r -> r.attempt).last()
}

/// Every ask, newest last: who asked which agent, and where it stands.
view asked(all: Agents) {
    std.browse(all, all.asks, empty: "Nobody asked an agent yet", opens: false) { a ->
        ask_row(a)
    }
}

/// One ask as a row: the agent asked, and what about.
view ask_row(a: Ask) {
    ui.ListRow(
        a.agent.name(),
        secondary: if let about = created(a)?.from { meta(about).title } else { meta(a).title },
    )
}

/// An agent's answer as it writes it, and what it called on the way. Another device's answer to
/// the same ask stands instead of a superseded one, which shows nothing. Its asker can stop it
/// while it works and ask again when it failed.
view reply(r: Reply) {
    state open: Bool
    if not r.superseded {
        stack(gap: .tight) {
            row(gap: .tight) {
                avatar(r.ask.agent)
                text(r.ask.agent.name(), role: .label)
            }
            if r.thinking != "" {
                ui.DisclosureRow(
                    if r.state == .working { "Thinking" } else { "Thought" },
                    icon: .lightbulb,
                ) {
                    on toggle: set(open, not open)
                    if open {
                        ui.Text(r.thinking, role: .body, secondary: true)
                    }
                }
            }
            rich_text(r.body)
            for v in r.views {
                sent_view(title: v.title)
            }
            for s in r.shown {
                rich_text(s.card)
            }
            // What answered it and what the turn cost: a quiet line under the answer, kept with
            // the reply so an answer read later still names the model that wrote it.
            if r.model != "" {
                row(gap: .tight) {
                    text("{r.model}, {r.provider},", role: .caption, secondary: true)
                    ui.Money(r.cost, role: .caption, secondary: true)
                }
            }
            for step in r.steps {
                text(step.called, role: .caption, secondary: true)
            }
            // What its asker can do with it: stop the turn while the agent works, and ask again
            // when it failed. The reply and its ask are working together until the agent's
            // finish moves them on, so a stopped ask offers Stop no more. The newest reply alone
            // offers either, so one already pressed, or one a later attempt replaced, asks
            // nothing.
            if created(r.ask)?.who == workspace.me {
                if let newest = latest_reply(r.ask) {
                    if newest == r {
                        if r.state == .working and r.ask.state == .working {
                            row {
                                button(stop(r.ask))
                            }
                        } else if r.state == .failed and r.ask.state == .failed {
                            row {
                                button(retry(r.ask))
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A view sent with what someone wrote, as a Chat message or an agent's reply carries one: what
/// it is called, where it is called anything apart from the words, and that the reader can add
/// it to their own views.
view sent_view(title: Text = "") {
    if title != "" {
        text(title, role: .label)
    }
    ui.Text("A view, to add to your own views", role: .caption, secondary: true)
}

/// The replies to `a`, in the order it was asked.
view replies(a: Ask) {
    for r in agents.replies.filter(r -> r.ask == a) {
        reply(r)
    }
}

/// An ask's window: what it is about, which opens, what was asked, and the replies to it.
view ask_page(a: Ask) {
    std.document(a) {
        std.origin(a)
        if let said = a.said {
            rich_text(said)
        }
        if a.state == .cannot_read {
            text("The agent may not read what it was asked about.", secondary: true)
        }
        replies(a)
    }
}

/// A morning of asks: two people asking two agents, one answered, one at work, one waiting and
/// one the agent could not read.
scenario "a morning of asks" {
    let mia = person("Mia")
    let leo = person("Leo")
    let scout = agent("Scout")
    let ledger = agent("Ledger")
    let notes = invoke(
        ask(agents, scout, RichText("Summarise what we decided about the offsite.")),
        by: mia,
        at: 2026-06-24T08:02Z,
    )
    let summary = invoke(answer(notes), by: scout, at: 2026-06-24T08:02Z)
    invoke(
        say(summary, RichText("Two days, Thursday and Friday; Mia books the room.")),
        by: scout,
        at: 2026-06-24T08:03Z,
    )
    invoke(
        finish(summary, .done, "Google", "Gemini 2.5 Pro", 42000),
        by: scout,
        at: 2026-06-24T08:03Z,
    )
    let costs = invoke(
        ask(agents, ledger, RichText("What did the studio cost us in May?")),
        by: leo,
        at: 2026-06-24T09:15Z,
    )
    invoke(answer(costs), by: ledger, at: 2026-06-24T09:15Z)
    invoke(
        ask(agents, scout, RichText("Find a train that arrives before ten.")),
        by: leo,
        at: 2026-06-24T09:40Z,
    )
    invoke(ask(agents, ledger, RichText("")), by: mia, at: 2026-06-24T10:05Z)
}

/// An answer being written: what the agent thought, the steps it took and its words so far,
/// under what it was asked.
scenario "an answer being written" -> Ask {
    let mia = person("Mia")
    let scout = agent("Scout")
    let a = invoke(
        ask(agents, scout, RichText("Which of this week's tasks are still unassigned?")),
        by: mia,
        at: 2026-06-24T08:02Z,
    )
    let r = invoke(answer(a), by: scout, at: 2026-06-24T08:02Z)
    invoke(
        think(r, "Mia means the release board. I'll list its open cards, then check who has each."),
        by: scout,
        at: 2026-06-24T08:02Z,
    )
    invoke(
        step(r, "board/release#list", RichText("Twelve cards, five of them open.")),
        by: scout,
        at: 2026-06-24T08:03Z,
    )
    invoke(
        say(r, RichText("Three are unassigned: the release notes, the docs screenshots and")),
        by: scout,
        at: 2026-06-24T08:03Z,
    )
    a
}
