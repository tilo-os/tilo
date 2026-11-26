import AssistantCore
import Foundation

/// The content of the window's drawing, the one the owner accepted as the specification. It is
/// here for tests and pictures only: no app links this module, so the app starts empty. Its
/// people and firms are the drawing's examples and nothing the product knows about.
public enum Drawing {
    public static let locale = Locale(identifier: "en_GB")

    public static let calendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC") ?? .gmt
        calendar.locale = locale
        return calendar
    }()

    /// A time on the drawing's day, 17 November 2026.
    public static func at(_ hour: Int, _ minute: Int, day: Int = 17) -> Date {
        calendar.date(from: DateComponents(year: 2026, month: 11, day: day, hour: hour, minute: minute)) ?? .distantPast
    }

    /// The moment the main chat is drawn at.
    public static let now = at(9, 44)

    public static var wording: Wording { Wording(locale: locale, calendar: calendar, now: now) }

    public static let person = Person(id: "person", name: "Xaver E", workspace: "Personal")

    /// Everything the drawing shows, signed in.
    public static func content() -> MemorySource.Content {
        var content = MemorySource.Content()
        content.session = .signedIn(person)
        content.pairsAs = person
        content.main = mainChat
        content.subAgents = subAgents
        content.subAgentChats = ["usage-drop": usageDropChat]
        content.replies = ["maya": mayaReplies]
        content.tasks = tasks
        content.automations = automations
        content.supervision = supervision
        content.models = models
        return content
    }

    public static func source() -> MemorySource {
        MemorySource(content(), now: { now })
    }

    // MARK: The main chat

    public static let mayaQuestion = Question(
        id: "maya-reply",
        text: "Maya, who invested in your seed, asks for the October numbers. I drafted a reply with the figures from the metrics sheet.",
        choices: [Choice(id: "send", title: "Send it", isMain: true), Choice(id: "show", title: "Show me the draft"),
                  Choice(id: "later", title: "Not now")])

    public static let quietReason = "Dima's mail did not notify you: it is news, so it waits in the conversation."

    public static var mainChat: [Item] {
        let rundown = TaskLine(task: "morning-rundown", title: "Morning rundown", state: .scheduled("every day"))
        let inbound = TaskLine(task: "investor-inbound", title: "Answer investor inbound", state: .actedAlone)
        let maya = TaskLine(task: "maya", title: "Reply to Maya at Northwind", state: .replies(2))
        return [
            Item(id: "rundown-1", at: at(7, 30), .message(Message(.assistant, """
                Good morning. Today:
                - **10:30** Northwind, with a rundown on the fund 15 minutes before.
                - **14:00** Atlas, moved from Wednesday.
                - **Open** two things wait for your yes.
                """)), task: rundown),
            Item(id: "rundown-2", at: at(7, 30),
                 .message(Message(.assistant, "Nine mails sorted since seven. None of them needs you.")), task: rundown),
            Item(id: "dima", at: at(9, 12), .report("Dima at Mento reached out. I responded in the usual way."),
                 task: inbound, keptQuiet: quietReason),
            Item(id: "maya", at: at(9, 12), .question(mayaQuestion), task: maya, isImportant: true,
                 notice: "Maya at Northwind asks for the October numbers. A reply is drafted."),
            Item(id: "remind-ask", at: at(9, 44), .message(Message(.person, "remind me in 30 minutes to call Jonas"))),
            Item(id: "remind-answer", at: at(9, 44), .message(Message(.assistant, "I will remind you at 10:14."))),
        ]
    }

    /// The two replies the line "Reply to Maya at Northwind" counts and the conversation leaves
    /// out. The drawing only counts them; these words are made up in its manner.
    public static var mayaReplies: [Item] {
        [
            Item(id: "maya-figures", at: at(9, 10), .message(Message(
                .assistant, "The October figures are in the metrics sheet: revenue, burn and runway."))),
            Item(id: "maya-draft", at: at(9, 11), .message(Message(
                .assistant, "The reply is drafted in your words, with the three figures and nothing else."))),
        ]
    }

    /// The reminder when its time has come, as the phone's lock screen shows it.
    public static let reminder = Item(id: "remind-jonas", at: at(10, 14), .reminder("Call Jonas."))

    // MARK: What the person was told

    /// What came while the person was away, had they left at 8:05 and come back now. The
    /// drawing shows no such message; its lines are the drawing's own pieces and tasks.
    public static var sinceLeft: SinceLeft {
        SinceLeft(left: at(8, 5),
                  waits: [SinceLeft.Line(id: "maya", words: "Maya at Northwind asks for the October numbers. A reply is drafted.",
                                         item: "maya", isImportant: true)],
                  didAlone: [SinceLeft.Line(id: "dima", words: "Dima at Mento reached out. I responded in the usual way.", item: "dima")],
                  cameIn: [SinceLeft.Line(id: "deck", words: "The board deck for Friday has a first draft.")])
    }

    /// Pieces as the app marks them from what a person's server told: a question with what
    /// the server offers under its notice, news an agent of theirs offers a button with, and
    /// the reminder, raised once more because it insists.
    public static var toldPieces: [Item] {
        [
            Item(id: "maya", at: at(9, 12), .question(mayaQuestion), isImportant: true,
                 notice: "Maya at Northwind asks for the October numbers. A reply is drafted.",
                 offers: [Offer(id: "send", title: "Send it", isMain: true), Offer(id: "later", title: "Not now")]),
            Item(id: "deck", at: at(9, 13), .message(Message(.assistant, "The board deck for Friday has a first draft.")),
                 isImportant: true, offers: [Offer(id: "show-deck", title: "Show the draft", isMain: true, by: "Researcher")]),
            Item(id: "remind-jonas", at: at(10, 14), .reminder("Call Jonas."), raised: 1),
        ]
    }

    // MARK: Sub-agents

    public static var subAgents: [SubAgent] {
        [
            SubAgent(id: "usage-drop", title: "Usage drop: investigate", role: "Coder", state: .working,
                     runsOn: "The cloud machine", started: at(9, 20)),
            SubAgent(id: "board-deck", title: "Find the Q3 board deck", role: "Researcher", state: .needsYou,
                     runsOn: "The cloud machine", started: at(8, 50)),
            SubAgent(id: "yc-batch", title: "YC batch against the thesis", role: "Researcher", state: .working,
                     runsOn: "The cloud machine", started: at(8, 5)),
            SubAgent(id: "data-room", title: "Data room: open requests", role: "Researcher", state: .done,
                     runsOn: "The cloud machine", started: at(15, 10, day: 16), finished: at(16, 2, day: 16)),
        ]
    }

    public static var usageDropChat: [Item] {
        [
            Item(id: "job", at: at(9, 20), .message(Message(.mainAgent(.job),
                "Daily use fell 18% since Tuesday. Find out why. Look at the deploys since Monday and at the event counts per build."))),
            Item(id: "deploys", at: at(9, 24),
                 .message(Message(.assistant, "Three deploys since Monday. Comparing event counts per build."))),
            Item(id: "also", at: at(9, 31),
                 .message(Message(.mainAgent(.instruction), "Also check whether Tuesday's sign-in change is involved."))),
            Item(id: "build", at: at(9, 36), .message(Message(.assistant,
                "The drop starts with build 4812, which changed sign-in. Returning people are sent to the first screen again."))),
            Item(id: "safari-ask", at: at(9, 40), .message(Message(.person, "is it only Safari?"))),
            Item(id: "safari", at: at(9, 41), .question(Question(
                id: "safari-fix",
                text: "Yes. 96% of the lost sessions are Safari. A fix is on a branch, with a test that fails without it.",
                choices: [Choice(id: "pull", title: "Open the pull request", isMain: true),
                          Choice(id: "change", title: "Show the change"), Choice(id: "stop", title: "Stop")]))),
        ]
    }

    // MARK: Tasks, scheduled things and events

    public static var tasks: [AssistantTask] {
        [
            AssistantTask(id: "maya", title: "Reply to Maya at Northwind", state: .readyForReview, isImportant: true, item: "maya"),
            AssistantTask(id: "deck-draft", title: "Board deck for Friday: first draft", state: .readyForReview),
            AssistantTask(id: "deck", title: "Find the Q3 board deck", state: .needsInput, note: "Two versions exist. Which one?"),
            AssistantTask(id: "usage", title: "Find out why usage dropped", state: .inProgress),
            AssistantTask(id: "yc", title: "Shortlist the YC batch for the thesis", state: .inProgress),
        ]
    }

    public static let investorEvent = Automation(
        id: "investor-writes", sort: .event, title: "When an investor writes",
        summary: "Answer in my words that we raise in three to six months, then tell me.",
        filter: "Sender is an investor", note: "your style, from 14 replies", actsAlone: true)

    public static var automations: [Automation] {
        [
            Automation(id: "remind-jonas", sort: .scheduled, title: "Remind: call Jonas", summary: "Today at 10:14, once.",
                       next: at(10, 14), once: true),
            Automation(id: "morning-rundown", sort: .scheduled, title: "Morning rundown", summary: "Every day at 7:30.",
                       next: at(7, 30, day: 18)),
            investorEvent,
            Automation(id: "ten-mails", sort: .event, title: "Every ten new mails", summary: "One summary.", threshold: 10),
            Automation(id: "investor-meeting", sort: .event, title: "15 minutes before an investor meeting",
                       summary: "A rundown on the fund.", filter: "Calendar entries flagged investor"),
            Automation(id: "mail-date", sort: .event, title: "When a mail contains a date", summary: "Put it in my calendar."),
        ]
    }

    // MARK: The assistant view

    public static var supervision: Supervision {
        Supervision(
            isOn: true, model: "Claude Opus 5.5",
            usage: Supervision.Usage(messagesToday: 42, tokensToday: 1_800_000, spentTodayMicros: 3_120_000,
                                     dailyLimitMicros: 10_000_000),
            connections: [
                Supervision.Connection(id: "linear", name: "Linear", added: at(8, 0), commands: 23),
                Supervision.Connection(id: "affinity", name: "Affinity", added: at(8, 0, day: 12), commands: 14),
            ],
            logins: [Supervision.Login(id: "linkedin", name: "LinkedIn", typed: at(8, 30), lastUsed: at(9, 0))],
            manager: Supervision.ManagerLogins(manager: "Bitwarden", first: "GitHub", count: 32),
            learned: [Supervision.Learned(id: "sign", text: "You sign mails with Best, Xaver"),
                      Supervision.Learned(id: "dima", text: "Dima at Mento is an investor")],
            learnedCount: 48,
            places: [Supervision.Place(name: "The cloud machine", detail: "always"),
                     Supervision.Place(name: "This Mac", detail: "in ~/dev/tilo and ~/Documents/Fundraising")],
            workspaces: ["Personal", "Tilo team", "Northwind data room"])
    }

    public static var models: [ModelChoice] {
        [ModelChoice(id: "anthropic/claude-opus-5-5", name: "Claude Opus 5.5", provider: "Anthropic"),
         ModelChoice(id: "google/gemini-3-pro", name: "Gemini 3 Pro", provider: "Google")]
    }

    // MARK: The first run

    /// What Tilo's page asks before a device is paired, as it says it for its own server.
    public static let pairingAsk = PairingAsk(
        heading: "Add this device?",
        said: ["You are adding this device to Xaver E’s devices. If that is not you, stop.",
               "The server that adds it is this site’s own, tilo.example.",
               "Whoever made this link approves it on a device of theirs. From then on this device acts as them, and what you do here is theirs."],
        go: "Add this device", stop: "Stop")

    public static let providers = [ProviderChoice(id: "anthropic", name: "Anthropic"), ProviderChoice(id: "gemini", name: "Google Gemini")]

    // MARK: Pieces of the conversation

    public static var secretPiece: [Item] {
        [Item(id: "linkedin", at: now, .secret(SecretRequest(
            id: "linkedin", text: "LinkedIn asks for your password. Type it below. I can use it to sign in, and I cannot read it.",
            name: "LinkedIn", manager: "Bitwarden")))]
    }

    public static var connectionPiece: [Item] {
        [
            Item(id: "address", at: now, .message(Message(.person, "https://mcp.linear.example/mcp"))),
            Item(id: "linear", at: now, .connection(ConnectionOffer(
                id: "linear", text: "That is Linear. It needs you to sign in.", name: "Linear", commands: 23))),
        ]
    }

    public static var eventPiece: [Item] {
        [
            Item(id: "ask", at: now, .message(Message(
                .person, "answer all investor inbound: we are not raising, we will in 3 to 6 months"))),
            Item(id: "event", at: now, .event(EventCard(
                text: "Done. I read your last fourteen replies to investors and wrote the answer in your words. From now on I answer and tell you afterwards.",
                automation: investorEvent, question: "investor-event",
                choices: [Choice(id: "show", title: "Show the answer"), Choice(id: "ask", title: "Ask me each time"),
                          Choice(id: "undo", title: "Undo")]))),
        ]
    }

    public static var briefPiece: [Item] {
        [Item(id: "brief", at: at(10, 15), .brief(Brief(title: "Northwind call in 15 minutes", begins: at(10, 30), text: """
            Rundown on the fund:
            - **Northwind** led your seed with 1.2 million.
            - **Maya** asked for the October numbers on Monday. The reply waits for you.
            - **Last call** was 3 October. You promised the hiring plan.
            """)), isImportant: true)]
    }

    /// What the message field holds in the drawing of it being typed in.
    public static let typed = "**Tomorrow** before the Atlas call:\n1. pull the churn numbers\n2. check `billing#retry` is live"
}
