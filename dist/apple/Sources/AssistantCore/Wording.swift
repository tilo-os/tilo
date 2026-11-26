import Foundation

/// The lines the window puts together from values: a time's mark, how a sub-agent stands, what
/// an event's tags say, the lines of the assistant view. One place, so the Mac, the iPhone, a
/// notification and a test say the same.
public struct Wording: Sendable {
    public var locale: Locale
    public var calendar: Calendar
    public var now: Date

    public init(locale: Locale = .current, calendar: Calendar = .current, now: Date = Date()) {
        self.locale = locale
        self.calendar = calendar
        self.now = now
    }

    private typealias W = AssistantWords

    // MARK: Times

    /// "7:30"
    public func time(_ date: Date) -> String {
        var style = Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone)
        style = style.hour(.defaultDigits(amPM: .abbreviated)).minute(.twoDigits)
        return date.formatted(style)
    }

    /// "17 Nov"
    public func day(_ date: Date) -> String {
        let style = Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone)
        return date.formatted(style.day(.defaultDigits).month(.abbreviated))
    }

    /// The mark between pieces of a conversation: "Today 7:30".
    public func mark(_ date: Date) -> String {
        if calendar.isDate(date, inSameDayAs: now) { return W.todayAt.said(["time": time(date)]) }
        if isYesterday(date) { return W.yesterdayAt.said(["time": time(date)]) }
        return "\(day(date)) \(time(date))"
    }

    private func isYesterday(_ date: Date) -> Bool {
        guard let yesterday = calendar.date(byAdding: .day, value: -1, to: now) else { return false }
        return calendar.isDate(date, inSameDayAs: yesterday)
    }

    /// Whether a mark stands between two pieces: at the first, on a new day, and after a pause.
    public func marks(_ date: Date, after before: Date?) -> Bool {
        guard let before else { return true }
        return !calendar.isDate(date, inSameDayAs: before) || date.timeIntervalSince(before) >= 10 * 60
    }

    // MARK: The conversation

    /// What a task's line says after the task's name.
    public func line(_ state: TaskLine.State) -> String? {
        switch state {
        case .none: nil
        case .scheduled(let when): W.lineScheduled.said(["when": when])
        case .actedAlone: W.lineActedAlone.said
        case .replies(let count): W.counted(count, one: .replyOne, many: .replyMany)
        }
    }

    /// The line over what the main agent wrote to a sub-agent.
    public func caption(_ word: Message.MainAgentWord, at date: Date) -> String {
        switch word {
        case .job: W.jobFromMain.said
        case .instruction: W.mainWhileWorking.said(["time": time(date)])
        }
    }

    // MARK: Sub-agents

    /// "working", "needs you", "done yesterday"
    public func state(_ agent: SubAgent) -> String {
        switch agent.state {
        case .working: return W.working.said
        case .needsYou: return W.needsYou.said
        case .failed: return W.failed.said
        case .stopped: return W.stopped.said
        case .done:
            guard let finished = agent.finished else { return W.done.said }
            if calendar.isDate(finished, inSameDayAs: now) { return W.doneToday.said }
            if isYesterday(finished) { return W.doneYesterday.said }
            return W.doneOn.said(["date": day(finished)])
        }
    }

    /// The line under a sub-agent's name in the sidebar: "Coder · working".
    public func sidebarLine(_ agent: SubAgent) -> String {
        joined([agent.role, state(agent)])
    }

    /// The line under a sub-agent's name in its own window: who it is, where it runs, when it
    /// was started and by whom, and that it cannot read the main chat.
    public func header(_ agent: SubAgent) -> String {
        let started = (agent.startedBy == .mainAgent ? W.startedByMain : W.startedByYou).said(["time": time(agent.started)])
        return joined([agent.role, agent.runsOn, started, W.cannotReadMain.said])
    }

    // MARK: Scheduled things and events

    /// "Event · on"
    public func state(_ automation: Automation) -> String {
        switch (automation.sort, automation.isOn) {
        case (.event, true): W.eventOn.said
        case (.event, false): W.eventPaused.said
        case (.scheduled, true): W.scheduledOn.said
        case (.scheduled, false): W.scheduledPaused.said
        }
    }

    /// An event's tags in the panel: its filter, its threshold, its note, and that it acts alone.
    public func tags(_ automation: Automation) -> [String] {
        var tags: [String] = []
        if let filter = automation.filter { tags.append(filter) }
        if let threshold = automation.threshold { tags.append(W.tagThreshold.said(["count": String(threshold)])) }
        if let note = automation.note { tags.append(W.tagNote.said(["note": note])) }
        if automation.actsAlone { tags.append(W.tagActsAlone.said) }
        return tags
    }

    /// An event's tags on its card in the conversation: its filter and its note, as they are.
    public func cardTags(_ automation: Automation) -> [String] {
        [automation.filter, automation.note.map(capitalised)].compactMap { $0 }
    }

    // MARK: The assistant view

    /// One line of the assistant view: what it is about, the first thing to say, the rest.
    public struct Line: Equatable, Sendable {
        public var first: String
        public var second: String?

        public init(first: String, second: String? = nil) {
            self.first = first
            self.second = second
        }
    }

    /// "1.8M": thousands, millions and billions by the letters they go by everywhere.
    public func tokens(_ count: Int) -> String {
        let steps: [(Double, String)] = [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "K")]
        for (size, letter) in steps where Double(count) >= size {
            return (Double(count) / size).formatted(.number.precision(.fractionLength(0...1)).locale(locale)) + letter
        }
        return count.formatted(.number.locale(locale))
    }

    /// The day's messages as the view's figure: the count the person's server said, or that
    /// their server counts none. Never a nought that nobody counted.
    public func messages(_ usage: Supervision.Usage) -> String {
        usage.messagesToday.map { $0.formatted(.number.locale(locale)) } ?? W.notCounted.said
    }

    /// The day's tokens as the view's figure, said the same way.
    public func tokens(_ usage: Supervision.Usage) -> String {
        usage.tokensToday.map(tokens) ?? W.notCounted.said
    }

    /// "3.12 $", "10 $"
    public func money(micros: Int, currency: String) -> String {
        let whole = micros % 1_000_000 == 0
        let amount = (Double(micros) / 1_000_000).formatted(
            .number.precision(.fractionLength(whole ? 0 : 2)).grouping(.never).locale(Locale(identifier: "en_US_POSIX")))
        return "\(amount) \(currency)"
    }

    public func spent(_ usage: Supervision.Usage) -> Line {
        Line(first: money(micros: usage.spentTodayMicros, currency: usage.currency),
             second: usage.dailyLimitMicros.map { W.ofLimit.said(["limit": money(micros: $0, currency: usage.currency)]) }
                ?? W.spentToday.said)
    }

    public func model(_ facts: Supervision) -> Line {
        guard let model = facts.model else { return Line(first: W.noModel.said, second: W.noModelNote.said) }
        return Line(first: model, second: W.modelNote.said)
    }

    /// "Name · added 17 Nov · 23 commands", or "Name · added 17 Nov · sign-in needed": one that
    /// nobody is signed in to offers nothing until they are, and the line says that for a count.
    public func said(_ connection: Supervision.Connection) -> String {
        guard !connection.needsSignIn else {
            return W.connectionSignIn.said(["name": connection.name, "date": day(connection.added)])
        }
        return W.connectionLine.said(["name": connection.name, "date": day(connection.added), "count": String(connection.commands)])
    }

    /// "Name · typed 17 Nov · used today"
    public func said(_ login: Supervision.Login) -> String {
        guard let used = login.lastUsed else { return W.loginLine.said(["name": login.name, "date": day(login.typed)]) }
        let when = calendar.isDate(used, inSameDayAs: now) ? W.usedToday.said
            : (isYesterday(used) ? W.usedYesterday.said : W.usedOn.said(["date": day(used)]))
        return W.loginUsed.said(["name": login.name, "date": day(login.typed), "used": when])
    }

    /// "One and 31 more from the manager, fetched when needed"
    public func said(_ manager: Supervision.ManagerLogins) -> String {
        W.managerLine.said(["first": manager.first, "count": String(max(0, manager.count - 1)), "manager": manager.manager])
    }

    public func connections(_ facts: Supervision) -> Line {
        lines(facts.connections.map(said))
    }

    public func logins(_ facts: Supervision) -> Line {
        var all = facts.logins.map(said)
        if let manager = facts.manager { all.insert(said(manager), at: min(1, all.count)) }
        return lines(all)
    }

    public func learned(_ facts: Supervision) -> Line {
        guard let first = facts.learned.first else { return Line(first: W.none.said, second: nil) }
        var rest = facts.learned.dropFirst().prefix(1).map(\.text)
        let more = facts.learnedCount - 1 - rest.count
        if more > 0 { rest.append(W.moreCount.said(["count": String(more)])) }
        return Line(first: first.text, second: rest.isEmpty ? nil : joined(rest))
    }

    public func places(_ facts: Supervision) -> Line {
        lines(facts.places.map { "\($0.name), \($0.detail)" })
    }

    /// The workspaces the source says the assistant works in. Under one stands that it works
    /// there. Nothing is said of any other workspace, and of several nothing is promised.
    public func workspaces(_ facts: Supervision) -> Line {
        guard !facts.workspaces.isEmpty else { return Line(first: W.none.said, second: nil) }
        return Line(first: joined(facts.workspaces), second: facts.workspaces.count == 1 ? W.worksInThis.said : nil)
    }

    /// The first thing on the line, the second under it, and how many more there are.
    private func lines(_ said: [String]) -> Line {
        guard let first = said.first else { return Line(first: W.none.said, second: nil) }
        var rest = Array(said.dropFirst().prefix(1))
        if said.count > 2 { rest.append(W.moreCount.said(["count": String(said.count - 2)])) }
        return Line(first: first, second: rest.isEmpty ? nil : joined(rest))
    }

    private func joined(_ parts: [String]) -> String {
        parts.filter { !$0.isEmpty }.joined(separator: " · ")
    }

    private func capitalised(_ words: String) -> String {
        guard let first = words.first else { return words }
        return first.uppercased() + words.dropFirst()
    }
}
