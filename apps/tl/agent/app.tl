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

/// An agent's answer to an ask, as it writes it. Asking again makes a new reply.
type Reply {
    ask: Ask
    /// The answer, growing while the agent writes.
    body: RichText
    state: AskState = .working
    /// Which time it was asked: the first reply is 1.
    attempt: Int = 1
    /// What the agent called on the way, in order.
    steps: Collection<Step>
    /// Whether another device's answer to the same ask stands instead: two devices offline at
    /// once may both have answered.
    superseded: Bool = false
}

/// Every ask and reply of the workspace.
type Agents {
    asks: Collection<Ask>
    replies: Collection<Reply>
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

/// Stops a turn that is working; what the agent did until then stays.
action stop(a: Ask) (icon: .square, label: "Stop") {
    require a.state == .working "Nothing is working on it"
    a.state = .stopped
}

/// The agent starts its reply to `a`, the ask's next attempt.
action answer(a: Ask) -> Reply (icon: .sparkles, label: "Answer", did: "answered") {
    require invoker == a.agent "Only the agent asked may answer"
    require a.state == .waiting or a.state == .working "Nobody waits for an answer"
    let attempt = agents.replies.filter(r -> r.ask == a).len() + 1
    a.state = .working
    agents.replies.insert(Reply(ask: a, body: RichText(""), attempt: attempt))
}

/// The agent writes `more` at the end of its answer.
action say(r: Reply, more: RichText) {
    require invoker == r.ask.agent "Only the agent asked may answer"
    require r.state == .working "The answer is finished"
    r.body = RichText(r.body, more)
}

/// The agent called `called` on the way, and it answered `answered`.
action step(r: Reply, called: Text, answered: RichText, failed: Bool = false) {
    require invoker == r.ask.agent "Only the agent asked may answer"
    require r.state == .working "The answer is finished"
    r.steps.insert(Step(called: called, answered: answered, failed: failed))
}

/// The agent's turn ended as `state`: done, failed, or stopped when a person stopped it.
action finish(r: Reply, state: AskState) {
    require invoker == r.ask.agent "Only the agent asked may answer"
    require state == .done or state == .failed or state == .stopped "A turn ends done, failed or stopped"
    let ended = if r.ask.state == .stopped { AskState.stopped } else { state }
    r.state = ended
    r.ask.state = ended
}

/// Another device's answer to the same ask stands instead of `r`.
action supersede(r: Reply) {
    require invoker == r.ask.agent "Only the agent asked may answer"
    r.superseded = true
}

/// The asks made from `about`, oldest first: what a message, a note or a task shows under it.
fn asks_on(about: host.Resource) -> List<Ask> {
    agents.asks.filter(a -> created(a)?.from == about)
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
/// the same ask stands instead of a superseded one, which shows nothing.
view reply(r: Reply) {
    if not r.superseded {
        stack(gap: .tight) {
            row(gap: .tight) {
                avatar(r.ask.agent)
                text(r.ask.agent.name(), role: .label)
            }
            rich_text(r.body)
            for step in r.steps {
                text(step.called, role: .caption, secondary: true)
            }
        }
    }
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
