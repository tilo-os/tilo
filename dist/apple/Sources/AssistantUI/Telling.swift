import AssistantClosed
import AssistantCore
import SwiftUI
import UserNotifications

/// Tells the person outside the window: a piece of a conversation as a notification of the
/// system, with the answer it offers as the system's own button, and their answer back.
///
/// It tells while the app runs. Telling a person whose app is closed is a push from their host
/// through Apple's service, under the same categories and with the same fields.
@MainActor
public final class Teller: NSObject {
    /// Whether the person is looking at this conversation right now: what they see is not told again.
    public var looksAt: (Chat) -> Bool = { _ in false }

    private let center = UNUserNotificationCenter.current()
    private let model: AssistantModel
    private var categories: [String: UNNotificationCategory] = [:]
    private var pendingRoute: ClosedRoute?
    private var watchingPending = false

    public init(model: AssistantModel) {
        self.model = model
        super.init()
        center.delegate = self
        // Every kind has its category from the start, so a push finds its buttons too.
        for kind in Notice.Kind.allCases { register("assistant.\(kind.rawValue)", answer: nil) }
        model.told = { [weak self] item, chat in self?.tell(item, in: chat) }
        model.offered = { [weak self] item, chat in self?.bring(item, in: chat) }
        model.settled = { [weak self] item, _ in
            // Answered here or on another device: nothing is left to answer under the notification.
            if !item.waits { self?.withdraw(item) }
        }
    }

    /// Ask the person whether the app may notify them, with the system's own question.
    @discardableResult
    public func ask() async -> Bool {
        (try? await center.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
    }

    public func tell(_ item: Item, in chat: Chat) {
        guard let notice = NoticePlan.notice(for: item, in: chat) else { return }
        // What the person sees arrive is not told again, and their Tilo hears that they saw it.
        // Why something did not notify is not listed for what they are looking at either.
        if looksAt(chat) {
            if notice.interrupts { model.saw(item.id, in: chat) }
            return
        }
        hand(notice, of: item, quietly: false)
    }

    /// What is offered with a piece changed after it was told: the notice the system still
    /// shows of it is brought up to date, without a sound and without lighting the screen.
    /// A notice the person put away is not brought back.
    public func bring(_ item: Item, in chat: Chat) {
        guard let notice = NoticePlan.notice(for: item, in: chat), notice.interrupts else { return }
        let request = Teller.request(item)
        center.getDeliveredNotifications { [weak self] delivered in
            guard delivered.contains(where: { $0.request.identifier == request }) else { return }
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.hand(notice, of: item, quietly: true) }
            }
        }
    }

    private func hand(_ notice: Notice, of item: Item, quietly: Bool) {
        let content = UNMutableNotificationContent()
        content.title = notice.title
        if let subtitle = notice.subtitle { content.subtitle = subtitle }
        content.body = notice.words
        // What did not interrupt is listed without a sound and without lighting the screen.
        let sounds = notice.interrupts && !quietly
        content.interruptionLevel = sounds ? .active : .passive
        content.sound = sounds ? .default : nil
        content.threadIdentifier = Teller.name(notice.chat)
        content.categoryIdentifier = NoticePlan.category(of: notice)
        var fields = ["item": item.id.value, "chat": Teller.name(notice.chat)]
        if let toldAs = item.toldAs { fields["told"] = toldAs }
        if let task = item.task { fields["task"] = task.task.value }
        if !notice.offers.isEmpty {
            // The buttons are the ones the person's Tilo offers with it, the expected one first.
            register(content.categoryIdentifier,
                     buttons: notice.offers.map { (NoticePlan.offerAction + $0.id.value, $0.title) })
        } else if let answer = notice.answer {
            fields["question"] = answer.question.value
            fields["choice"] = answer.choice.value
            register(content.categoryIdentifier, answer: answer.title)
        }
        content.userInfo = fields
        center.add(UNNotificationRequest(identifier: Teller.request(item), content: content, trigger: nil),
                   withCompletionHandler: nil)
    }

    public func withdraw(_ item: Item) {
        center.removeDeliveredNotifications(withIdentifiers: [Teller.request(item)])
    }

    /// The number on the app's icon: how many things wait for the person.
    public func setBadge(_ count: Int) {
        center.setBadgeCount(max(0, count), withCompletionHandler: nil)
    }

    private func register(_ identifier: String, answer: String?) {
        register(identifier, buttons: answer.map { [(NoticePlan.answerAction, $0)] } ?? [])
    }

    private func register(_ identifier: String, buttons: [(name: String, title: String)]) {
        guard categories[identifier] == nil else { return }
        // An answer acts for the person: the phone is unlocked first, and the app need not come forward.
        var actions = buttons.map {
            UNNotificationAction(identifier: $0.name, title: $0.title, options: [.authenticationRequired])
        }
        actions.append(UNNotificationAction(identifier: NoticePlan.openAction, title: AssistantWords.open.said,
                                            options: [.foreground]))
        categories[identifier] = UNNotificationCategory(
            identifier: identifier,
            actions: actions,
            intentIdentifiers: [],
            hiddenPreviewsBodyPlaceholder: AssistantWords.toldWaits.said,
            options: []
        )
        center.setNotificationCategories(Set(categories.values))
    }

    func took(_ action: String, _ route: ClosedRoute) {
        if action.hasPrefix(NoticePlan.offerAction) {
            let offer = Offer.ID(String(action.dropFirst(NoticePlan.offerAction.count)))
            if let (item, chat) = resolvedItem(for: route) {
                model.press(offer, of: item, in: chat)
            }
        } else if action == NoticePlan.answerAction, let question = route.question, let choice = route.choice {
            model.answer(question, with: choice)
        } else if action == NoticePlan.openAction || action == UNNotificationDefaultActionIdentifier {
            routeTo(route)
        }
    }

    private func resolvedItem(for route: ClosedRoute) -> (Item.ID, Chat)? {
        if case .item(let item, let chat) = route.resolve(conversations: model.conversations.mapValues(\.items), tasks: model.tasks) {
            return (item, chat)
        }
        if case .item(let item, let chat) = route.target {
            return (item, chat)
        }
        return nil
    }

    private func routeTo(_ route: ClosedRoute) {
        if applyResolved(route) {
            pendingRoute = nil
            return
        }
        // Cold launch or pending sync: open the closest surface immediately and finish routing
        // to the exact waiting decision or task as soon as it arrives.
        switch route.target {
        case .item(let item, let chat):
            model.show(item, in: chat)
            model.saw(item, in: chat)
            if model.conversations[chat]?.isLoaded != true {
                pendingRoute = route
                watchPendingRoute()
            } else {
                pendingRoute = nil
            }
        case .decision(_, let chat):
            model.open(chat)
            pendingRoute = route
            watchPendingRoute()
        case .task:
            model.open(.main)
            model.showTasks(.active)
            pendingRoute = route
            watchPendingRoute()
        case .told(_, let kind):
            model.open(.main)
            if kind == "reminder" {
                model.showTasks(.scheduled)
            } else if kind == "task" {
                model.showTasks(.active)
            }
            pendingRoute = route
            watchPendingRoute()
        }
    }

    private func applyResolved(_ route: ClosedRoute) -> Bool {
        guard let resolved = route.resolve(conversations: model.conversations.mapValues(\.items), tasks: model.tasks) else {
            return false
        }
        switch resolved {
        case .item(let item, let chat):
            model.show(item, in: chat)
            model.saw(item, in: chat)
        case .task(_, let item, let state):
            let tab: AssistantModel.PanelTab = switch state {
            case .upNext: .upNext
            case .resolved: .resolved
            default: .active
            }
            if let item {
                model.show(item, in: .main)
                model.saw(item, in: .main)
            } else {
                model.open(.main)
            }
            model.showTasks(tab)
        }
        return true
    }

    private func watchPendingRoute() {
        guard !watchingPending, pendingRoute != nil else { return }
        watchingPending = true
        withObservationTracking {
            _ = model.conversations
            _ = model.tasks
            _ = model.person
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.watchingPending = false
                guard let pending = self.pendingRoute else { return }
                if self.model.person == nil {
                    self.pendingRoute = nil
                    return
                }
                if self.applyResolved(pending) {
                    self.pendingRoute = nil
                } else {
                    self.watchPendingRoute()
                }
            }
        }
    }

    /// The name the system keeps a piece's notice under. Where a push may tell of the piece
    /// while the app is closed, it is the name the push gives it: the two are then one notice,
    /// each takes the other's place, and nothing is shown twice.
    static func request(_ item: Item) -> String { item.toldAs ?? "assistant-\(item.id.value)" }

    static func name(_ chat: Chat) -> String {
        switch chat {
        case .main: "main"
        case .subAgent(let id): "sub-agent:\(id.value)"
        }
    }

    static func chat(_ name: String?) -> Chat? {
        ClosedRoute.parseChat(name)
    }
}

// The system calls these on a thread of its choosing and wants to be told on the main thread
// that an answer was taken. So they are tied to no actor, read what they need where they are
// called, and go to the main thread themselves.
extension Teller: UNUserNotificationCenterDelegate {
    public nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        // A push that comes while the app is in front shows nothing: the app tells of the same
        // thing itself, under the same name, unless the person is looking at it.
        if notification.request.trigger is UNPushNotificationTrigger {
            completionHandler([])
            return
        }
        // What did not interrupt is listed and nothing more.
        let listed = notification.request.content.interruptionLevel == .passive
        completionHandler(listed ? [.list] : [.banner, .list, .sound])
    }

    public nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let taken = Taken(completionHandler)
        let action = response.actionIdentifier
        let route = ClosedRoute(userInfo: response.notification.request.content.userInfo,
                                requestIdentifier: response.notification.request.identifier)
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                if let route { self.took(action, route) }
            }
            taken.tell()
        }
    }
}

/// The system's own block, carried to the main thread to be called there once.
private final class Taken: @unchecked Sendable {
    let tell: () -> Void

    init(_ tell: @escaping () -> Void) {
        self.tell = tell
    }
}

#if os(iOS)
import ActivityKit

/// Keeps the iPhone's live activities in line with what `LiveActivityPlan` says earns one.
///
/// When APNs push capability is provisioned (`ClosedBuild`), new activities request
/// `pushType: .token` and forward push-to-start and per-activity token updates to
/// `model.source.register(_:)`, falling back cleanly to `pushType: nil` when unprovisioned.
@MainActor
public final class LiveActivities {
    private let model: AssistantModel
    private let closed: ClosedBuild?
    private var tokenTasks: [String: Task<Void, Never>] = [:]
    private var startTokenTask: Task<Void, Never>?

    public init(model: AssistantModel, closed: ClosedBuild? = ClosedBuild(info: Bundle.main.infoDictionary)) {
        self.model = model
        self.closed = closed
    }

    isolated deinit {
        startTokenTask?.cancel()
        for task in tokenTasks.values { task.cancel() }
    }

    public func bring(now: Date = Date()) {
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { return }
        if let closed { watchStartTokens(closed: closed) }
        let wanted = LiveActivityPlan.activities(tasks: model.tasks, subAgents: model.subAgents, automations: model.automations,
                                                 items: model.conversation(.main).items, now: now)
        let active = Activity<AssistantActivity>.activities
        for activity in active {
            // The system's own handle on an activity: it is made to be told from anywhere.
            nonisolated(unsafe) let activity = activity
            let name = activity.attributes.name
            if let facts = wanted[name] {
                if let closed { watchPushTokens(for: activity, name: name, closed: closed) }
                guard activity.content.state != facts else { continue }
                Task { await activity.update(ActivityContent(state: facts, staleDate: facts.until)) }
            } else {
                tokenTasks.removeValue(forKey: name)?.cancel()
                var last = activity.content.state
                last.state = .done
                let ended = last
                Task { await activity.end(ActivityContent(state: ended, staleDate: nil), dismissalPolicy: .immediate) }
            }
        }
        let known = Set(active.map(\.attributes.name))
        for (name, facts) in wanted where !known.contains(name) && LiveActivityPlan.mayBegin(facts) {
            let content = ActivityContent(state: facts, staleDate: facts.until)
            if let activity = startActivity(name: name, content: content), let closed {
                watchPushTokens(for: activity, name: name, closed: closed)
            }
        }
    }

    /// Start a live activity with `pushType: .token` when APNs push is enabled, falling back
    /// cleanly to `pushType: nil` when unprovisioned or rejected by ActivityKit.
    private func startActivity(name: String, content: ActivityContent<LiveFacts>) -> Activity<AssistantActivity>? {
        let attributes = AssistantActivity(name: name)
        if closed != nil,
           let activity = try? Activity.request(attributes: attributes, content: content, pushType: .token) {
            return activity
        }
        return try? Activity.request(attributes: attributes, content: content, pushType: nil)
    }

    /// Forward push-to-start token updates to the source registration path when provisioned.
    private func watchStartTokens(closed: ClosedBuild) {
        guard startTokenTask == nil else { return }
        let source = model.source
        let bundleID = Bundle.main.bundleIdentifier ?? ""
        startTokenTask = Task {
            for await token in Activity<AssistantActivity>.pushToStartTokenUpdates {
                guard !Task.isCancelled else { return }
                await source.register(closed.address(for: .activityStart, token: token, bundleID: bundleID))
            }
        }
    }

    /// Forward per-activity APNs push token updates to the source registration path.
    private func watchPushTokens(for activity: Activity<AssistantActivity>, name: String, closed: ClosedBuild) {
        guard tokenTasks[name] == nil else { return }
        nonisolated(unsafe) let activity = activity
        let source = model.source
        let bundleID = Bundle.main.bundleIdentifier ?? ""
        if let initial = activity.pushToken {
            let address = closed.address(for: .activity(name), token: initial, bundleID: bundleID)
            Task { await source.register(address) }
        }
        tokenTasks[name] = Task {
            for await token in activity.pushTokenUpdates {
                guard !Task.isCancelled else { return }
                await source.register(closed.address(for: .activity(name), token: token, bundleID: bundleID))
            }
        }
    }

    /// The activities the system holds for the app now, one line each: for a check.
    public func running() -> [String] {
        Activity<AssistantActivity>.activities.map { activity in
            let state = activity.content.state
            return "\(activity.attributes.name): \(state.kind.rawValue), \(state.state.rawValue), \(state.title)"
        }
        .sorted()
    }
}
#endif
