import Foundation

/// Every sentence the assistant's window shows of its own, by its key in the bundle's
/// `Localizable.strings`, with its English. What the assistant itself says (a message, a
/// task's name, an answer's title) is not here: those are its words, in the person's language.
///
/// A sentence has `{app}` where the app's own name belongs, and other names in braces where a
/// value goes in.
public enum AssistantWords: String, CaseIterable, Sendable {
    // The sidebar.
    case assistant, mainChat = "main-chat", subAgents = "sub-agents", newSubAgent = "new-sub-agent"
    case search, searchField = "search-field", nothingFound = "nothing-found"
    case noSubAgents = "no-sub-agents", showSidebar = "show-sidebar", hideSidebar = "hide-sidebar"
    case working, needsYou = "needs-you", done, doneToday = "done-today", doneYesterday = "done-yesterday"
    case doneOn = "done-on", failed, stopped
    // The conversation.
    case todayAt = "today-at", yesterdayAt = "yesterday-at"
    case lineScheduled = "line-scheduled", lineActedAlone = "line-acted-alone"
    case replyOne = "reply-one", replyMany = "reply-many", important
    case showReplyOne = "show-reply-one", showReplyMany = "show-reply-many", hideReplies = "hide-replies"
    case noReplies = "no-replies"
    case readyOne = "ready-one", readyMany = "ready-many", seeAllTasks = "see-all-tasks"
    case youChose = "you-chose", answering, earlier
    case nothingYet = "nothing-yet", nothingYetBody = "nothing-yet-body"
    case liveThing = "live-thing", attachment
    // A sub-agent's window.
    case startedByMain = "started-by-main", startedByYou = "started-by-you", cannotReadMain = "cannot-read-main"
    case jobFromMain = "job-from-main", mainWhileWorking = "main-while-working", stop
    case subAgentJob = "sub-agent-job", subAgentJobBody = "sub-agent-job-body", start, cancel
    // The message field.
    case messageAssistant = "message-assistant", messageSubAgent = "message-sub-agent"
    case fieldHint = "field-hint", attach, dictate, stopDictating = "stop-dictating", send, removeFile = "remove-file"
    case dictationUnavailable = "dictation-unavailable", dictationNotAllowed = "dictation-not-allowed"
    // The cards.
    case keptInVault = "kept-in-vault", save, getFromManager = "get-from-manager", secretFine = "secret-fine"
    case secretField = "secret-field", secretKept = "secret-kept", secretDeclined = "secret-declined"
    case commandsAfter = "commands-after", authenticate, notNow = "not-now", connected, notConnected = "not-connected"
    case eventOn = "event-on", eventPaused = "event-paused", scheduledOn = "scheduled-on", scheduledPaused = "scheduled-paused"
    // The Tasks panel.
    case tasks, active, upNext = "up-next", scheduled, resolved
    case readyForReview = "ready-for-review", needsInput = "needs-input", inProgress = "in-progress", events
    case tagThreshold = "tag-threshold", tagNote = "tag-note", tagActsAlone = "tag-acts-alone"
    case addTask = "add-task", addTaskField = "add-task-field", add, close
    case nothingActive = "nothing-active", nothingUpNext = "nothing-up-next"
    case nothingScheduled = "nothing-scheduled", nothingResolved = "nothing-resolved"
    case isOn = "is-on", showTasks = "show-tasks", hideTasks = "hide-tasks"
    // The assistant view.
    case assistantView = "assistant-view", assistantOn = "assistant-on", assistantPaused = "assistant-paused"
    case pause, resume, model, modelNote = "model-note", noModel = "no-model", noModelNote = "no-model-note", change
    case messagesToday = "messages-today", tokensToday = "tokens-today", ofLimit = "of-limit", spentToday = "spent-today"
    case connections, connectionLine = "connection-line", logins, loginLine = "login-line", loginUsed = "login-used"
    case usedToday = "used-today", usedYesterday = "used-yesterday", usedOn = "used-on"
    case managerLine = "manager-line", moreCount = "more-count", open
    case hasLearned = "has-learned", whereItWorks = "where-it-works", workspaces, yoursInEach = "yours-in-each"
    case none, forget, signOut = "sign-out", addConnectionDraft = "add-connection-draft"
    // The way in.
    case wayInTitle = "way-in-title", wayInLead = "way-in-lead", scanCode = "scan-code", pasteLink = "paste-link"
    case wayInHelp = "way-in-help", wayInHelpOne = "way-in-help-one", pairing, noLink = "no-link", codeNoLink = "code-no-link"
    case unreachable, cameraNotAllowed = "camera-not-allowed", tryAgain = "try-again", notReachable = "not-reachable"
    // The menu bar and the menus.
    case openApp = "open-app", waitsOne = "waits-one", waitsMany = "waits-many", waitsNone = "waits-none"
    case pauseAssistant = "pause-assistant", resumeAssistant = "resume-assistant", quit, viewMenu = "view-menu"
    case about, hideApp = "hide-app", hideOthers = "hide-others", showAll = "show-all"
    case fileMenu = "file-menu", closeWindow = "close-window", editMenu = "edit-menu", undo, redo, cut, copy, paste
    case selectAll = "select-all", windowMenu = "window-menu", minimize, zoom
    // Notifications and the live activity.
    case reminder, brief, inProgressCount = "in-progress-count"

    public var english: String {
        switch self {
        case .assistant: "Assistant"
        case .mainChat: "Main chat"
        case .subAgents: "Sub-agents"
        case .newSubAgent: "New sub-agent"
        case .search: "Search"
        case .searchField: "Search this conversation"
        case .nothingFound: "Nothing found"
        case .noSubAgents: "None yet"
        case .showSidebar: "Show sidebar"
        case .hideSidebar: "Hide sidebar"
        case .working: "working"
        case .needsYou: "needs you"
        case .done: "done"
        case .doneToday: "done today"
        case .doneYesterday: "done yesterday"
        case .doneOn: "done {date}"
        case .failed: "failed"
        case .stopped: "stopped"
        case .todayAt: "Today {time}"
        case .yesterdayAt: "Yesterday {time}"
        case .lineScheduled: "scheduled, {when}"
        case .lineActedAlone: "done, by your lasting instruction"
        case .replyOne: "1 reply"
        case .replyMany: "{count} replies"
        case .important: "Important"
        case .showReplyOne: "Show 1 reply"
        case .showReplyMany: "Show {count} replies"
        case .hideReplies: "Hide replies"
        case .noReplies: "Nothing more to show"
        case .readyOne: "1 ready for review"
        case .readyMany: "{count} ready for review"
        case .seeAllTasks: "See all tasks"
        case .youChose: "You chose {answer}"
        case .answering: "Writing"
        case .earlier: "Show earlier"
        case .nothingYet: "Nothing here yet"
        case .nothingYetBody: "Write to your assistant below."
        case .liveThing: "Live view"
        case .attachment: "Attachment"
        case .startedByMain: "started {time} by the main agent"
        case .startedByYou: "started {time} by you"
        case .cannotReadMain: "it cannot read the main chat"
        case .jobFromMain: "The job, from the main agent"
        case .mainWhileWorking: "Main agent, {time}, while it worked"
        case .stop: "Stop"
        case .subAgentJob: "What is the job?"
        case .subAgentJobBody: "The main agent picks who does it and with which model."
        case .start: "Start"
        case .cancel: "Cancel"
        case .messageAssistant: "Message your assistant"
        case .messageSubAgent: "Message this sub-agent"
        case .fieldHint: "Return sends · Shift Return makes a line"
        case .attach: "Attach a file"
        case .dictate: "Dictate"
        case .stopDictating: "Stop dictating"
        case .send: "Send"
        case .removeFile: "Remove {name}"
        case .dictationUnavailable: "Dictation is not available in this language on this device."
        case .dictationNotAllowed: "Allow the microphone and speech recognition for {app} in the system's settings, then try again."
        case .keptInVault: "Kept in your vault"
        case .save: "Save"
        case .getFromManager: "Get it from {manager}"
        case .secretFine: "This field is not part of the chat. What you type goes to the vault only."
        case .secretField: "The secret for {name}"
        case .secretKept: "Saved in your vault"
        case .secretDeclined: "Not given"
        case .commandsAfter: "{count} commands after sign-in"
        case .authenticate: "Authenticate"
        case .notNow: "Not now"
        case .connected: "Connected"
        case .notConnected: "Not connected"
        case .eventOn: "Event · on"
        case .eventPaused: "Event · paused"
        case .scheduledOn: "Scheduled · on"
        case .scheduledPaused: "Scheduled · paused"
        case .tasks: "Tasks"
        case .active: "Active"
        case .upNext: "Up next"
        case .scheduled: "Scheduled"
        case .resolved: "Resolved"
        case .readyForReview: "Ready for review"
        case .needsInput: "Needs input"
        case .inProgress: "In progress"
        case .events: "Events"
        case .tagThreshold: "Threshold: {count}"
        case .tagNote: "Note: {note}"
        case .tagActsAlone: "Acts alone"
        case .addTask: "Add a task"
        case .addTaskField: "What should it do?"
        case .add: "Add"
        case .close: "Close"
        case .nothingActive: "Nothing waits for you"
        case .nothingUpNext: "Nothing is up next"
        case .nothingScheduled: "Nothing is scheduled"
        case .nothingResolved: "Nothing is resolved yet"
        case .isOn: "On"
        case .showTasks: "Show tasks"
        case .hideTasks: "Hide tasks"
        case .assistantView: "Assistant view"
        case .assistantOn: "Assistant is on"
        case .assistantPaused: "Assistant is paused"
        case .pause: "Pause"
        case .resume: "Resume"
        case .model: "Model"
        case .modelNote: "From your Tilo providers · sub-agents may use a smaller one"
        case .noModel: "No model yet"
        case .noModelNote: "Link a provider in Tilo, then choose a model here"
        case .change: "Change"
        case .messagesToday: "Messages today"
        case .tokensToday: "Tokens today"
        case .ofLimit: "Of {limit} a day"
        case .spentToday: "Spent today"
        case .connections: "Connections"
        case .connectionLine: "{name} · added {date} · {count} commands"
        case .logins: "Logins"
        case .loginLine: "{name} · typed {date}"
        case .loginUsed: "{name} · typed {date} · {used}"
        case .usedToday: "used today"
        case .usedYesterday: "used yesterday"
        case .usedOn: "used {date}"
        case .managerLine: "{first} and {count} more from {manager}, fetched when needed"
        case .moreCount: "{count} more"
        case .open: "Open"
        case .hasLearned: "It has learned"
        case .whereItWorks: "Where it works"
        case .workspaces: "Workspaces"
        case .yoursInEach: "It is yours in each. Nobody else can instruct it."
        case .none: "Nothing yet"
        case .forget: "Forget"
        case .signOut: "Sign out of this device"
        case .addConnectionDraft: "Connect this service: "
        case .wayInTitle: "Your assistant, on this device"
        case .wayInLead: "Pair this device with your Tilo. Your assistant, its tasks and everything it set up come with you."
        case .scanCode: "Scan code"
        case .pasteLink: "Paste link"
        case .wayInHelp: "Both come from “Pair a new device” on a device where you are signed in."
        case .wayInHelpOne: "It comes from “Pair a new device” on a device where you are signed in."
        case .pairing: "Waiting for Tilo"
        case .noLink: "The clipboard holds no link."
        case .codeNoLink: "This code holds no link."
        case .unreachable: "Tilo cannot be reached. Check the connection, then try again."
        case .cameraNotAllowed: "Allow the camera for {app} in the system's settings to scan a code."
        case .tryAgain: "Try again"
        case .notReachable: "Tilo cannot be reached right now"
        case .openApp: "Open {app}"
        case .waitsOne: "1 thing waits for you"
        case .waitsMany: "{count} things wait for you"
        case .waitsNone: "Nothing waits for you"
        case .pauseAssistant: "Pause the assistant"
        case .resumeAssistant: "Let the assistant go on"
        case .quit: "Quit {app}"
        case .viewMenu: "View"
        case .about: "About {app}"
        case .hideApp: "Hide {app}"
        case .hideOthers: "Hide others"
        case .showAll: "Show all"
        case .fileMenu: "File"
        case .closeWindow: "Close window"
        case .editMenu: "Edit"
        case .undo: "Undo"
        case .redo: "Redo"
        case .cut: "Cut"
        case .copy: "Copy"
        case .paste: "Paste"
        case .selectAll: "Select all"
        case .windowMenu: "Window"
        case .minimize: "Minimize"
        case .zoom: "Zoom"
        case .reminder: "Reminder"
        case .brief: "Brief"
        case .inProgressCount: "{count} in progress"
        }
    }

    /// The sentence as the app's bundle says it in the person's language, or in English.
    public var said: String {
        said([:])
    }

    /// The sentence with its values put in.
    public func said(_ values: [String: String]) -> String {
        var sentence = Bundle.main.localizedString(forKey: rawValue, value: english, table: nil)
        for (name, value) in values.merging(["app": AssistantWords.appName], uniquingKeysWith: { given, _ in given }) {
            sentence = sentence.replacingOccurrences(of: "{\(name)}", with: value)
        }
        return sentence
    }

    /// One of two sentences by a count: the first for exactly one.
    public static func counted(_ count: Int, one: AssistantWords, many: AssistantWords) -> String {
        count == 1 ? one.said : many.said(["count": String(count)])
    }

    /// The app's name, as its bundle gives it.
    public static var appName: String {
        let info = Bundle.main.infoDictionary ?? [:]
        for key in ["CFBundleDisplayName", "CFBundleName"] {
            if let name = info[key] as? String, !name.isEmpty { return name }
        }
        return "Tilo Assistant"
    }

    /// The whole table as a `Localizable.strings` file, which the bundle carries.
    public static var catalogue: String {
        let head = """
        /* Tilo Assistant's own sentences in English. Sources/AssistantCore/Words.swift holds the
           same table; a test fails when the two differ. {app} is the app's own name, which its
           bundle gives; the other names in braces are values put in when the sentence is said. */

        """
        let lines = allCases.map { words -> String in
            let english = words.english.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
            return "\"\(words.rawValue)\" = \"\(english)\";"
        }
        return head + lines.joined(separator: "\n") + "\n"
    }
}
