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

    public init(model: AssistantModel) {
        self.model = model
        super.init()
        center.delegate = self
        // Every kind has its category from the start, so a push finds its buttons too.
        for kind in Notice.Kind.allCases { register("assistant.\(kind.rawValue)", answer: nil) }
        model.told = { [weak self] item, chat in self?.tell(item, in: chat) }
        model.settled = { [weak self] item, _ in
            // Answered here or on another device: nothing is left to answer under the notification.
            if !item.waits { self?.withdraw(item.id) }
        }
    }

    /// Ask the person whether the app may notify them, with the system's own question.
    @discardableResult
    public func ask() async -> Bool {
        (try? await center.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
    }

    public func tell(_ item: Item, in chat: Chat) {
        guard let notice = NoticePlan.notice(for: item, in: chat), !(notice.interrupts && looksAt(chat)) else { return }
        let content = UNMutableNotificationContent()
        content.title = notice.title
        if let subtitle = notice.subtitle { content.subtitle = subtitle }
        content.body = notice.body
        // What did not interrupt is listed without a sound and without lighting the screen.
        content.interruptionLevel = notice.interrupts ? .active : .passive
        content.sound = notice.interrupts ? .default : nil
        content.threadIdentifier = Teller.name(chat)
        content.categoryIdentifier = NoticePlan.category(of: notice)
        var fields = ["item": item.id.value, "chat": Teller.name(chat)]
        if let answer = notice.answer {
            fields["question"] = answer.question.value
            fields["choice"] = answer.choice.value
            register(content.categoryIdentifier, answer: answer.title)
        }
        content.userInfo = fields
        center.add(UNNotificationRequest(identifier: Teller.request(item.id), content: content, trigger: nil),
                   withCompletionHandler: nil)
    }

    public func withdraw(_ item: Item.ID) {
        center.removeDeliveredNotifications(withIdentifiers: [Teller.request(item)])
    }

    /// The number on the app's icon: how many things wait for the person.
    public func setBadge(_ count: Int) {
        center.setBadgeCount(max(0, count), withCompletionHandler: nil)
    }

    private func register(_ identifier: String, answer: String?) {
        guard categories[identifier] == nil else { return }
        var actions: [UNNotificationAction] = []
        if let answer {
            // An answer acts for the person: the phone is unlocked first, and the app need not come forward.
            actions.append(UNNotificationAction(identifier: NoticePlan.answerAction, title: answer,
                                                options: [.authenticationRequired]))
        }
        actions.append(UNNotificationAction(identifier: NoticePlan.openAction, title: AssistantWords.open.said,
                                            options: [.foreground]))
        categories[identifier] = UNNotificationCategory(identifier: identifier, actions: actions, intentIdentifiers: [],
                                                        options: [])
        center.setNotificationCategories(Set(categories.values))
    }

    private func took(_ action: String, _ fields: [String: String]) {
        guard let item = fields["item"], let chat = Teller.chat(fields["chat"]) else { return }
        if action == NoticePlan.answerAction, let question = fields["question"], let choice = fields["choice"] {
            model.answer(Question.ID(question), with: Choice.ID(choice))
        } else if action == NoticePlan.openAction || action == UNNotificationDefaultActionIdentifier {
            model.show(Item.ID(item), in: chat)
        }
    }

    private static func request(_ item: Item.ID) -> String { "assistant-\(item.value)" }

    static func name(_ chat: Chat) -> String {
        switch chat {
        case .main: "main"
        case .subAgent(let id): "sub-agent:\(id.value)"
        }
    }

    static func chat(_ name: String?) -> Chat? {
        guard let name else { return nil }
        if name == "main" { return .main }
        guard name.hasPrefix("sub-agent:") else { return nil }
        return .subAgent(SubAgent.ID(String(name.dropFirst("sub-agent:".count))))
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
        completionHandler([.banner, .list, .sound])
    }

    public nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let taken = Taken(completionHandler)
        let action = response.actionIdentifier
        var fields: [String: String] = [:]
        for (key, value) in response.notification.request.content.userInfo {
            if let key = key as? String, let value = value as? String { fields[key] = value }
        }
        DispatchQueue.main.async {
            MainActor.assumeIsolated { self.took(action, fields) }
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
