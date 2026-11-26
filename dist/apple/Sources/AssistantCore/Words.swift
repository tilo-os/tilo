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
    case noSubAgents = "no-sub-agents", showSidebar = "show-sidebar", hideSidebar = "hide-sidebar", sidebar
    case working, needsYou = "needs-you", done, doneToday = "done-today", doneYesterday = "done-yesterday"
    case doneOn = "done-on", failed, stopped
    // The conversation.
    case todayAt = "today-at", yesterdayAt = "yesterday-at"
    case lineScheduled = "line-scheduled", lineActedAlone = "line-acted-alone"
    case replyOne = "reply-one", replyMany = "reply-many", important
    case showReplyOne = "show-reply-one", showReplyMany = "show-reply-many", hideReplies = "hide-replies"
    case noReplies = "no-replies", conversation, you, waitsForAnswer = "waits-for-answer"
    case readyOne = "ready-one", readyMany = "ready-many", seeAllTasks = "see-all-tasks"
    case youChose = "you-chose", answering, moreBelow = "more-below", notTakenUp = "not-taken-up", earlier
    case thinking, atWork = "at-work", stepOne = "step-one", stepMany = "step-many", stepFailed = "step-failed"
    case showSteps = "show-steps", hideSteps = "hide-steps"
    case nothingYet = "nothing-yet", nothingYetBody = "nothing-yet-body"
    case liveThing = "live-thing", attachment, selectText = "select-text"
    case reply, replyingTo = "replying-to", dropReply = "drop-reply", earlierMessage = "earlier-message", quote, selectToQuote = "select-to-quote"
    case viewNotOffered = "view-not-offered", liveNotShown = "live-not-shown", stopSharing = "stop-sharing"
    // The first run, in the main chat.
    case makingAssistant = "making-assistant", makingWorkspace = "making-workspace"
    case needsProvider = "needs-provider", providerKey = "provider-key"
    case linkProvider = "link-provider", needsModel = "needs-model", chooseModel = "choose-model", answersOn = "answers-on"
    // The way back to the provider step, from the assistant view's Model line.
    case anotherKey = "another-key", linkAnother = "link-another", anotherKeyStep = "another-key-step"
    case anotherProviderStep = "another-provider-step", useKey = "use-key", keyTaken = "key-taken"
    // A sub-agent's window.
    case startedByMain = "started-by-main", startedByYou = "started-by-you", cannotReadMain = "cannot-read-main"
    case jobFromMain = "job-from-main", mainWhileWorking = "main-while-working", stop
    case delegateJob = "delegate-job"
    case subAgentJob = "sub-agent-job", subAgentJobBody = "sub-agent-job-body", start, cancel
    // The message field.
    case messageAssistant = "message-assistant", messageSubAgent = "message-sub-agent"
    case askAboutLookRound = "ask-about-look-round", askLookRoundSettings = "ask-look-round-settings"
    case askLookRoundOn = "ask-look-round-on", askLookRoundSchedule = "ask-look-round-schedule", askLookRoundOff = "ask-look-round-off"
    case fieldHint = "field-hint", attach, dictate, stopDictating = "stop-dictating", send, removeFile = "remove-file"
    case dictationUnavailable = "dictation-unavailable", dictationNotAllowed = "dictation-not-allowed"
    // The cards.
    case keptInVault = "kept-in-vault", save, getFromManager = "get-from-manager", secretFine = "secret-fine"
    case secretField = "secret-field", secretKept = "secret-kept", secretDeclined = "secret-declined"
    case commandsAfter = "commands-after", authenticate, notNow = "not-now", connected, notConnected = "not-connected"
    case commandsCount = "commands-count", signInAt = "sign-in-at"
    case eventOn = "event-on", eventPaused = "event-paused", scheduledOn = "scheduled-on", scheduledPaused = "scheduled-paused"
    case showAnswer = "show-answer", askEachTime = "ask-each-time", asksEachTime = "asks-each-time"
    case eventUndone = "event-undone", eventRemoved = "event-removed"
    case keptTold = "kept-told", keptStyle = "kept-style", keptGuidance = "kept-guidance", keptAccepted = "kept-accepted"
    case keptNoAnswer = "kept-no-answer"
    // The Tasks panel.
    case tasks
    case proposedWhen = "proposed-when", proposedCondition = "proposed-condition", proposedInstruction = "proposed-instruction"
    case refreshProposals = "refresh-proposals", suggestionsSignedOut = "suggestions-signed-out"
    case proposals, startTask = "start-task", setUpEvent = "set-up-event", declineProposal = "decline-proposal"
    case lookNow = "look-now", lookRequested = "look-requested", proposalUnconfirmed = "proposal-unconfirmed"
    case alwaysAllow = "always-allow", alwaysAllowDetail = "always-allow-detail", confirmAlwaysAllow = "confirm-always-allow"
    case kindQuiet = "kind-quiet", kindAlwaysAllowed = "kind-always-allowed", suggestAgain = "suggest-again", askMeFirst = "ask-me-first"
    case active, upNext = "up-next", scheduled, resolved
    case readyForReview = "ready-for-review", needsInput = "needs-input", inProgress = "in-progress", events
    case tagThreshold = "tag-threshold", tagNote = "tag-note", tagActsAlone = "tag-acts-alone"
    case noteStyle = "note-style", noteStyleFromOne = "note-style-from-one", noteStyleFromMany = "note-style-from-many"
    case noteAcceptedOne = "note-accepted-one", noteAcceptedMany = "note-accepted-many"
    case noteGuidance = "note-guidance", noteKept = "note-kept"
    case addTask = "add-task", addTaskField = "add-task-field", add, close
    case nothingActive = "nothing-active", nothingUpNext = "nothing-up-next"
    case nothingScheduled = "nothing-scheduled", nothingResolved = "nothing-resolved"
    case isOn = "is-on", showTasks = "show-tasks", hideTasks = "hide-tasks", changeByTelling = "change-by-telling"
    // The assistant view.
    case assistantView = "assistant-view", assistantActive = "assistant-active", assistantRetired = "assistant-retired"
    case model, modelNote = "model-note", noModel = "no-model", noModelNote = "no-model-note", change
    case messagesToday = "messages-today", tokensToday = "tokens-today", ofLimit = "of-limit", spentToday = "spent-today"
    case notCounted = "not-counted"
    case connections, connectionLine = "connection-line", logins, loginLine = "login-line", loginUsed = "login-used"
    case connectionSignIn = "connection-sign-in"
    case usedToday = "used-today", usedYesterday = "used-yesterday", usedOn = "used-on"
    case managerLine = "manager-line", moreCount = "more-count", open
    case hasLearned = "has-learned", whereItWorks = "where-it-works", workspaces, worksInThis = "works-in-this", ownWorkspace = "own-workspace"
    // Where it works: a place, and what is to say of it.
    case placeWorkspace = "place-workspace", placeAvailable = "place-available"
    case none, forget, signOut = "sign-out", addConnectionDraft = "add-connection-draft"
    // The way in.
    case wayInTitle = "way-in-title", wayInLead = "way-in-lead", wayInReset = "way-in-reset", scanCode = "scan-code", pasteLink = "paste-link"
    case getPairingLink = "get-pairing-link"
    case wayInHelp = "way-in-help", wayInHelpOne = "way-in-help-one", pairing, noLink = "no-link", codeNoLink = "code-no-link"
    case codeFromElsewhere = "code-from-elsewhere", pairsNothing = "pairs-nothing"
    case compareCode = "compare-code", pairingSite = "pairing-site"
    case unreachable, cameraNotAllowed = "camera-not-allowed", tryAgain = "try-again", notReachable = "not-reachable"
    case joinStops = "join-stops", signOutToPair = "sign-out-to-pair"
    case technicalDetails = "technical-details", copyDetails = "copy-details"
    case wayInApproved = "way-in-approved", wayInGoesOn = "way-in-goes-on", approvedWhen = "approved-when"
    case wayInApprovedHelp = "way-in-approved-help"
    // The menu bar and the menus.
    case openApp = "open-app", waitsOne = "waits-one", waitsMany = "waits-many", waitsNone = "waits-none"
    case quit, viewMenu = "view-menu"
    case about, hideApp = "hide-app", hideOthers = "hide-others", showAll = "show-all"
    case fileMenu = "file-menu", closeWindow = "close-window", editMenu = "edit-menu", undo, redo, cut, copy, paste
    case selectAll = "select-all", windowMenu = "window-menu", minimize, zoom
    // Notifications and the live activity.
    case reminder, brief, inProgressCount = "in-progress-count"
    // What the person's Tilo told them: why something did not notify, what it offers, what came while they were away.
    case quietLine = "quiet-line", quietLinePlain = "quiet-line-plain", quietNoRule = "quiet-no-rule"
    case quietWhile = "quiet-while", quietByRule = "quiet-by-rule", quietUnjudged = "quiet-unjudged"
    case offeredBy = "offered-by", allow, deny
    case sinceLeft = "since-left", sinceWaits = "since-waits", sinceDidAlone = "since-did-alone"
    case sinceCameIn = "since-came-in", sinceMore = "since-more"
    // What a push says in the open while the app is closed. The person's Tilo names it by this
    // key, and the system reads it from the bundle, so it takes no value.
    case toldWaits = "TOLD_WAITS"
    // Which build the app is, where the app says so itself.
    case versionAndBuild = "version-and-build", versionAlone = "version-alone"

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
        case .sidebar: "Sidebar"
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
        case .conversation: "Conversation"
        case .you: "You"
        case .waitsForAnswer: "Waiting for your answer"
        case .readyOne: "1 ready for review"
        case .readyMany: "{count} ready for review"
        case .seeAllTasks: "See all tasks"
        case .youChose: "You chose {answer}"
        case .answering: "Writing"
        case .moreBelow: "More below"
        case .notTakenUp: "Tilo has not taken this up yet."
        case .earlier: "Show earlier"
        case .thinking: "Thinking"
        case .atWork: "Working"
        case .stepOne: "1 step"
        case .stepMany: "{count} steps"
        case .stepFailed: "failed"
        case .showSteps: "Shows the steps so far"
        case .hideSteps: "Hides the steps"
        case .nothingYet: "Nothing here yet"
        case .nothingYetBody: "Write to your assistant below."
        case .liveThing: "Live view"
        case .attachment: "Attachment"
        case .selectText: "Select text"
        case .reply: "Reply"
        case .replyingTo: "Replying to"
        case .dropReply: "Do not reply to this message"
        case .earlierMessage: "An earlier message"
        case .quote: "Quote"
        case .selectToQuote: "Select the words to quote"
        case .viewNotOffered: "This Tilo cannot show views in messages yet"
        case .liveNotShown: "Tilo could not show this"
        case .stopSharing: "Stop sharing"
        case .makingAssistant: "Setting up your assistant"
        case .makingWorkspace: "Tilo is making your workspace. The first time, this can take a few minutes."
        case .needsProvider: "No model provider is linked yet, so your assistant cannot answer. Choose a provider and type your own key for it below."
        case .providerKey: "Your key for {name}"
        case .linkProvider: "Link {name}"
        case .needsModel: "A provider is linked. Choose the model your assistant answers on."
        case .chooseModel: "Choose a model"
        case .answersOn: "Your assistant answers on {model}"
        case .anotherKey: "Use another key for {name}"
        case .linkAnother: "Link another provider"
        case .anotherKeyStep: "Type another key of your own for {name} below. It takes the place of the key your vault keeps for {name}."
        case .anotherProviderStep: "Choose a provider and type your own key for it below."
        case .useKey: "Use this key for {name}"
        case .keyTaken: "The key is in your vault. Your assistant answers on the model you chose, and you can change it here."
        case .startedByMain: "started {time} by the main agent"
        case .startedByYou: "started {time} by you"
        case .cannotReadMain: "it cannot read the main chat"
        case .jobFromMain: "The job, from the main agent"
        case .mainWhileWorking: "Main agent, {time}, while it worked"
        case .stop: "Stop"
        case .delegateJob: "Start a sub-agent for this job: {job}"
        case .subAgentJob: "What is the job?"
        case .subAgentJobBody: "The main agent picks who does it and with which model."
        case .start: "Start"
        case .cancel: "Cancel"
        case .messageAssistant: "Message your assistant"
        case .askAboutLookRound: "Your workspace checks"
        case .askLookRoundSettings: "Show the schedule and daily spending limit for my checks in this workspace."
        case .askLookRoundOn: "Turn on my checks in this workspace to look for work you could do."
        case .askLookRoundSchedule: "Change my checks in this workspace to once an hour."
        case .askLookRoundOff: "Turn off my checks in this workspace."
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
        case .commandsCount: "{count} commands"
        case .signInAt: "Sign in at {site}"
        case .authenticate: "Authenticate"
        case .notNow: "Not now"
        case .connected: "Connected"
        case .notConnected: "Not connected"
        case .eventOn: "Event · on"
        case .eventPaused: "Event · paused"
        case .scheduledOn: "Scheduled · on"
        case .scheduledPaused: "Scheduled · paused"
        case .showAnswer: "Show the answer"
        case .askEachTime: "Ask me each time"
        case .asksEachTime: "It asks you each time now."
        case .eventUndone: "Undone. It is removed and does nothing more."
        case .eventRemoved: "Removed"
        case .keptTold: "What it is told to do"
        case .keptStyle: "How it writes"
        case .keptGuidance: "What it keeps in mind"
        case .keptAccepted: "An answer you accepted"
        case .keptNoAnswer: "It keeps no answer of its own yet. It writes one each time from what it is told."
        case .tasks: "Tasks"
        case .proposedWhen: "When"
        case .proposedCondition: "Only if"
        case .proposedInstruction: "Instruction"
        case .refreshProposals: "Refresh suggestions"
        case .suggestionsSignedOut: "Sign in to review suggestions."
        case .proposals: "Suggestions"
        case .startTask: "Start task"
        case .setUpEvent: "Set up event"
        case .declineProposal: "Decline"
        case .lookNow: "Look for work now"
        case .lookRequested: "Look requested."
        case .proposalUnconfirmed: "Tilo could not confirm the change. Check the suggestions before trying again."
        case .alwaysAllow: "Always allow {kind}…"
        case .alwaysAllowDetail: "Start this task and allow future tasks of kind “{kind}” to start without asking?"
        case .confirmAlwaysAllow: "Start and always allow"
        case .kindQuiet: "Suggestions for “{kind}” are paused."
        case .kindAlwaysAllowed: "Tasks of kind “{kind}” start automatically."
        case .suggestAgain: "Suggest again"
        case .askMeFirst: "Ask me first"
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
        case .noteStyle: "your style"
        case .noteStyleFromOne: "your style, from 1 answer you accepted"
        case .noteStyleFromMany: "your style, from {count} answers you accepted"
        case .noteAcceptedOne: "1 answer you accepted"
        case .noteAcceptedMany: "{count} answers you accepted"
        case .noteGuidance: "what you care about"
        case .noteKept: "what it saw last"
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
        case .changeByTelling: "Opens the chat, where you change it by telling your assistant"
        case .assistantView: "Assistant view"
        case .assistantActive: "Assistant is active"
        case .assistantRetired: "Assistant is retired"
        case .model: "Model"
        case .modelNote: "From your Tilo providers · sub-agents may use a smaller one"
        case .noModel: "No model yet"
        case .noModelNote: "Link a provider in the main chat, then choose a model here"
        case .change: "Change"
        case .messagesToday: "Messages today"
        case .tokensToday: "Tokens today"
        case .ofLimit: "Of {limit} a day"
        case .spentToday: "Spent today"
        case .notCounted: "Not counted yet"
        case .connections: "Connections"
        case .connectionLine: "{name} · added {date} · {count} commands"
        case .logins: "Logins"
        case .loginLine: "{name} · typed {date}"
        case .loginUsed: "{name} · typed {date} · {used}"
        case .connectionSignIn: "{name} · added {date} · sign-in needed"
        case .usedToday: "used today"
        case .usedYesterday: "used yesterday"
        case .usedOn: "used {date}"
        case .managerLine: "{first} and {count} more from {manager}, fetched when needed"
        case .moreCount: "{count} more"
        case .open: "Open"
        case .hasLearned: "It has learned"
        case .whereItWorks: "Where it works"
        case .workspaces: "Workspaces"
        case .worksInThis: "Your assistant works in this workspace."
        case .ownWorkspace: "Your own workspace"
        case .placeWorkspace: "Your workspace"
        case .placeAvailable: "on an available device"
        case .none: "Nothing yet"
        case .forget: "Forget"
        case .signOut: "Sign out of this device"
        case .addConnectionDraft: "Connect this service: "
        case .wayInTitle: "Your assistant, on this device"
        case .wayInLead: "Pair this device with your Tilo. Your assistant, its tasks and everything it set up come with you."
        case .wayInReset: "Your Tilo was reset since this device was paired, so it has to be paired again."
        case .scanCode: "Scan code"
        case .pasteLink: "Paste link"
        case .getPairingLink: "Get a pairing link"
        case .wayInHelp: "On your first visit, “Skip for now” opens your workspace. Choose “Pair a new device” there, then scan the code or choose “Open in Tilo Assistant”."
        case .wayInHelpOne: "On your first visit, “Skip for now” opens your workspace. Choose “Pair a new device” there, then choose “Open in Tilo Assistant” or copy the link."
        case .pairing: "Waiting for Tilo"
        case .compareCode: "A device where you are signed in shows a code and asks about it. Approve there only if it shows this one."
        case .pairingSite: "This app is set up for {site}."
        case .noLink: "The clipboard holds no link."
        case .codeNoLink: "This code holds no link."
        case .codeFromElsewhere: "This code is from another Tilo site, {from}. This app is set up for {site}. Open “Pair a new device” there."
        case .pairsNothing: "This link pairs no device. This app is set up for {site}. Open “Pair a new device” there."
        case .unreachable: "Tilo cannot be reached. Check the connection, then try again."
        case .cameraNotAllowed: "Allow the camera for {app} in the system's settings to scan a code."
        case .tryAgain: "Try again"
        case .notReachable: "Tilo cannot be reached right now"
        case .joinStops: "Tilo answers but does not let this device in: signing out here lets you pair it again."
        case .signOutToPair: "Sign out and pair again"
        case .technicalDetails: "Technical details"
        case .copyDetails: "Copy details"
        case .wayInApproved: "This device was approved, but Tilo stopped before it opened here. Try again goes on as that device: nothing is paired twice."
        case .wayInGoesOn: "This device was approved. Tilo is opening on it, and nothing has to be paired again."
        case .approvedWhen: "Approved: {when}"
        case .wayInApprovedHelp: "Or pair it afresh with “Pair a new device” on a device where you are signed in."
        case .openApp: "Open {app}"
        case .waitsOne: "1 thing waits for you"
        case .waitsMany: "{count} things wait for you"
        case .waitsNone: "Nothing waits for you"
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
        case .quietLine: "{what} did not notify you: {why}"
        case .quietLinePlain: "This did not notify you: {why}"
        case .quietNoRule: "you have not said what is important to you"
        case .quietWhile: "not important while {fact} is {value}"
        case .quietByRule: "not important by what you said"
        case .quietUnjudged: "nobody could judge whether it is important"
        case .offeredBy: "{name} offers this"
        case .allow: "Allow"
        case .deny: "Deny"
        case .toldWaits: "Something from your assistant waits for you"
        case .versionAndBuild: "Version {version} ({build})"
        case .versionAlone: "Version {version}"
        case .sinceLeft: "Since you left"
        case .sinceWaits: "Waits for you"
        case .sinceDidAlone: "Done without asking"
        case .sinceCameIn: "Came in"
        case .sinceMore: "{count} more"
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

    /// The language these sentences are said in, as what a time or a number that stands among
    /// them is written by: an English line says "2.0s" on a device set to a German region too.
    public static var language: Locale {
        language(spoken: spoken, on: .current)
    }

    /// The same for sentences held in the language `spoken`, on a device set to `device`. Where
    /// the device speaks that language itself, its region's ways stay; where it does not, the
    /// language's own.
    public static func language(spoken: String?, on device: Locale) -> Locale {
        let said = Locale(identifier: Locale.canonicalLanguageIdentifier(from: spoken ?? "en"))
        return said.language.languageCode == device.language.languageCode ? device : said
    }

    /// The language the bundle holds its sentences in for this person. It changes only with a new start.
    private static let spoken = Bundle.main.preferredLocalizations.first

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
