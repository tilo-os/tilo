import AssistantClosed
import AssistantCore
import SwiftUI
import UserNotifications

/// Tells the person outside the window: a piece of a conversation as a notification of the
/// system, with the answer it offers as the system's own button, and their answer back.
///
/// It tells while the app runs. Telling a person whose app is closed is a push from their host
/// through Apple's service. A push names its piece with this device's opaque name.
@MainActor
public final class Teller: NSObject {
    /// Whether the person is looking at this conversation right now: what they see is not told again.
    public var looksAt: (Chat) -> Bool = { _ in false }

    private let center = UNUserNotificationCenter.current()
    private let model: AssistantModel
    private var categories: [String: UNNotificationCategory] = [:]

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
        categories[identifier] = UNNotificationCategory(identifier: identifier, actions: actions, intentIdentifiers: [],
                                                        options: [])
        center.setNotificationCategories(Set(categories.values))
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

    nonisolated static func chat(_ name: String?) -> Chat? {
        guard let name else { return nil }
        if name == "main" { return .main }
        guard name.hasPrefix("sub-agent:") else { return nil }
        return .subAgent(SubAgent.ID(String(name.dropFirst("sub-agent:".count))))
    }
}

/// What the person chose in a system notification. A remote notification can only open
/// a piece whose device-specific name the signed-in workspace supplies.
enum NoticeTap: Sendable {
    case push(String)
    case open(Item.ID, Chat)
    case offer(Offer.ID, Item.ID, Chat)
    case answer(Question.ID, Choice.ID)

    init?(_ action: String, fields: [AnyHashable: Any], pushed: Bool) {
        let opens = action == NoticePlan.openAction || action == UNNotificationDefaultActionIdentifier
        if pushed {
            guard opens, let push = ClosedPush(fields), !push.isGone else { return nil }
            self = .push(push.name)
            return
        }
        guard let item = fields["item"] as? String, let chat = Teller.chat(fields["chat"] as? String) else { return nil }
        if action.hasPrefix(NoticePlan.offerAction) {
            self = .offer(Offer.ID(String(action.dropFirst(NoticePlan.offerAction.count))), Item.ID(item), chat)
        } else if action == NoticePlan.answerAction, let question = fields["question"] as? String, let choice = fields["choice"] as? String {
            self = .answer(Question.ID(question), Choice.ID(choice))
        } else if opens {
            self = .open(Item.ID(item), chat)
        } else {
            return nil
        }
    }

    @MainActor
    func take(in model: AssistantModel) {
        switch self {
        case .push(let name): model.openNotice(named: name)
        case .open(let item, let chat):
            model.show(item, in: chat)
            model.saw(item, in: chat)
        case .offer(let offer, let item, let chat): model.press(offer, of: item, in: chat)
        case .answer(let question, let choice): model.answer(question, with: choice)
        }
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
        let request = response.notification.request
        let tap = NoticeTap(response.actionIdentifier, fields: request.content.userInfo, pushed: request.trigger is UNPushNotificationTrigger)
        DispatchQueue.main.async {
            MainActor.assumeIsolated { tap?.take(in: self.model) }
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
/// It can begin one only while the app is in front, and bring one up to date only while the app
/// runs: with the app closed, both are the host's to do through Apple's service.
@MainActor
public final class LiveActivities {
    private let model: AssistantModel

    public init(model: AssistantModel) {
        self.model = model
    }

    public func bring(now: Date = Date()) {
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { return }
        let wanted = LiveActivityPlan.activities(tasks: model.tasks, subAgents: model.subAgents, automations: model.automations,
                                                 items: model.conversation(.main).items, now: now)
        let running = Activity<AssistantActivity>.activities
        for activity in running {
            // The system's own handle on an activity: it is made to be told from anywhere.
            nonisolated(unsafe) let activity = activity
            if let facts = wanted[activity.attributes.name] {
                guard activity.content.state != facts else { continue }
                Task { await activity.update(ActivityContent(state: facts, staleDate: facts.until)) }
            } else {
                var last = activity.content.state
                last.state = .done
                let ended = last
                Task { await activity.end(ActivityContent(state: ended, staleDate: nil), dismissalPolicy: .immediate) }
            }
        }
        let known = Set(running.map(\.attributes.name))
        for (name, facts) in wanted where !known.contains(name) && LiveActivityPlan.mayBegin(facts) {
            _ = try? Activity.request(attributes: AssistantActivity(name: name),
                                      content: ActivityContent(state: facts, staleDate: facts.until), pushType: nil)
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
