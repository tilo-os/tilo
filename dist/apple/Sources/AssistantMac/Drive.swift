#if os(macOS)
import AppKit
import AssistantCore
import AssistantTilo
import AssistantUI
import CryptoKit
import Foundation
import Observation
import ShellCore
import TiloLive
import WebKit

/// Tilo Assistant with no window, for a check to drive: the same source on Tilo and the same
/// model the window's views read and call, and nothing on screen. It is never the app in front
/// and has no icon in the Dock.
///
///   AssistantMac --drive --site ORIGIN --store UUID [--main-chat CONVERSATION]
///                [--device-name NAME] [--code-file FILE]
///   AssistantMac --drive --store UUID --forget
///
/// It prints one JSON object a line: where the device stands, the session, each piece of the
/// main chat as the model holds it, what the model says did not go through. Steps arrive on
/// standard input, one JSON object a line:
///   {"do": "pair", "file": "…"}                      a file that holds a pairing link
///   {"do": "add-device"}   {"do": "stop-pairing"}    as the two buttons under what Tilo asks before it pairs
///   {"do": "make-assistant"}                         as Try again does where the assistant could not be made
///   {"do": "chat-permission"}   {"do": "assistant-work-permission"}   request one scope of assistant access
///   {"do": "assistant-app-permission", "apps": ["so.tilo.chat", "so.tilo.calendar"]}   request just these apps
///   {"do": "check-chat-permission"}   {"do": "revoke-chat-permission", "confirm": true}   check or withdraw access
///   {"do": "choose", "model": "…"}                    as the main chat's choice of a model
///   {"do": "send", "text": "…", "files": ["…"]}       as the message field sends, with the files picked under it
///   {"do": "answer", "question": "…", "choice": "…"}  as a question's button answers
///   {"do": "keep", "request": "…", "file": "…"}       as the protected field keeps what was typed
///   {"do": "link", "preset": "…", "file": "…"}       as the provider step's protected field links a provider;
///                                                    with "model" the provider's default model is set as well
///   {"do": "another-key", "provider": "…"}            as "Use another key for NAME" under the assistant view's Model
///                                                    line: the provider step stands in the main chat again, and
///                                                    "link" with that provider as its preset types the new key
///   {"do": "link-another"}   {"do": "not-now"}        as "Link another provider" there, and as Not now under the step
///   {"do": "reply-way"}                               as the press under the newest reply whose model gave no answer
///                                                    for the key's sake: the same step stands in the main chat
///   {"do": "event", "name": "…", "when": "…", "instruction": "…"}   something that comes at a time
///   {"do": "task", "title": "…"}                      as the plus of the Tasks panel gives the assistant a task
///   {"do": "open", "file": "…"}                       as a press on a file's tag: the file is fetched and shown to nobody
///   {"do": "stop"}                                    stop the answer that is being written
///   {"do": "saw", "item": "…"}                        as opening a piece from its notification does
///   {"do": "press", "item": "…", "offer": "…"}        as a press on a button under a piece's notice
///   {"do": "show-live", "item": "…"}                  as the place of a piece's live thing does when it comes to
///                                                    the front: it says whether the thing loaded
///   {"do": "live-text", "item": "…"}                  what a live document of the word processor reads as now
///   {"do": "sign-out"}                               as "Sign out of this device" does: the device is nobody's here
///   {"do": "live-read", "item": "…", "picture": "…"}   what a piece's live thing draws now: the size its content asks
///                                                    for, its mount's frames and the names it says; with "picture"
///                                                    the web view's own snapshot of itself (FILE.snapshot.png, said
///                                                    in a line of its own when it comes)
///   {"do": "models"}   {"do": "state"}   {"do": "quit"}
/// Of each reply that grew it says, once its writing has ended, when it drew the first change
/// of its words and when the last, in milliseconds, and how many it drew ("grew"): what a
/// person at the window would have seen, counted where no window is.
/// It prints no link, no code and no secret, and ends when its standard input ends. With
/// --forget it removes the store, and with it the device's key, and what it kept beside it.
@MainActor
final class AssistantDrive {
    static var isAsked: Bool { CommandLine.arguments.contains("--drive") }

    private let source: TiloSource
    private let model: AssistantModel
    private let codeFile: String?
    private let began = Date()
    private var said: [String: String] = [:]
    private var pieces: [Item.ID: String] = [:]
    /// What was drawn of each reply while it grew, and when the person last sent.
    private var growth = ReplyGrowth()
    private var sent: Date?

    private init(source: TiloSource, codeFile: String?) {
        self.source = source
        self.codeFile = codeFile
        model = AssistantModel(source: source)
    }

    private static func argument(_ name: String) -> String? {
        let all = CommandLine.arguments
        guard let at = all.firstIndex(of: name), all.indices.contains(at + 1) else { return nil }
        return all[at + 1]
    }

    private static func stop(_ why: String) -> Never {
        FileHandle.standardError.write(Data("AssistantMac --drive: \(why)\n".utf8))
        exit(2)
    }

    private static func suite(_ store: UUID) -> String { "so.tilo.assistant.drive.\(store.uuidString.lowercased())" }

    static func run() -> Never {
        let app = NSApplication.shared
        // Never the app in front, no icon in the Dock, no window.
        app.setActivationPolicy(.prohibited)
        guard let store = argument("--store").flatMap(UUID.init(uuidString:)) else { stop("--store is a UUID") }
        if CommandLine.arguments.contains("--forget") {
            Task { @MainActor in
                // What the app kept beside the store, and the file the system kept it in.
                UserDefaults.standard.removePersistentDomain(forName: suite(store))
                UserDefaults.standard.synchronize()
                if let library = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask).first {
                    try? FileManager.default.removeItem(at: library.appendingPathComponent("Preferences/\(suite(store)).plist"))
                }
                do {
                    try await TiloEngine.forget(store: store)
                    say(["forgot": true])
                } catch {
                    say(["forgot": false, "why": .string(error.localizedDescription)])
                }
                exit(0)
            }
            app.run()
            exit(0)
        }
        guard let site = argument("--site"), let settings = try? ShellSettings(site: site) else { stop("--site names a Tilo site") }
        guard let defaults = UserDefaults(suiteName: suite(store)) else { stop("no place to keep what the app keeps") }
        // Without a conversation the app finds its person's assistant by itself, and makes it where they keep none.
        let chat: TiloSource.MainChat = argument("--main-chat").map { .conversation(id: $0, name: $0) } ?? .assistant
        let source = TiloSource(settings: settings, store: store, mainChat: chat, defaults: defaults,
                                deviceName: argument("--device-name") ?? "")
        let drive = AssistantDrive(source: source, codeFile: argument("--code-file"))
        drive.start()
        // Steps arrive on a thread of their own. When whoever drives is gone, so is the program.
        Thread.detachNewThread {
            while let line = readLine() {
                let step = line.trimmingCharacters(in: .whitespaces)
                if step.isEmpty { continue }
                Task { @MainActor in await drive.take(step) }
            }
            exit(0)
        }
        app.run()
        exit(0)
    }

    private func start() {
        // What the frame's Teller is handed, and the notice it would hand the system. This
        // program has no place among the system's notifications, so it says the notice instead.
        model.told = { item, chat in
            guard let notice = NoticePlan.notice(for: item, in: chat) else { return }
            AssistantDrive.say(["told": ["item": .string(item.id.value), "kind": .string(notice.kind.rawValue),
                                         "title": .string(notice.title), "mark": notice.subtitle.map(JSON.string) ?? .null,
                                         "body": .string(notice.words), "interrupts": .bool(notice.interrupts),
                                         "offers": .array(notice.offers.map { ["id": .string($0.id.value), "title": .string($0.title)] }),
                                         "raised": .number(Double(item.raised))]])
        }
        // What the Teller would bring to a notice the system still shows, without telling again.
        model.offered = { item, chat in
            guard let notice = NoticePlan.notice(for: item, in: chat) else { return }
            AssistantDrive.say(["offered": ["item": .string(item.id.value),
                                            "offers": .array(notice.offers.map { ["id": .string($0.id.value), "title": .string($0.title)] })]])
        }
        model.start()
        let engine = source.engine
        Task { [weak self] in
            for await state in engine.states() { self?.stood(state) }
        }
        watch()
    }

    private func stood(_ state: TiloEngine.State) {
        let seconds = JSON.number((Date().timeIntervalSince(began) * 10).rounded() / 10)
        switch state {
        case .away: break
        case .arriving(let step): AssistantDrive.say(["state": "arriving", "step": .string(step), "s": seconds])
        case .asking(let ask): AssistantDrive.say(["state": "asking", "said": .number(Double(ask.said.count)), "s": seconds])
        case .pairing(let code):
            if let codeFile {
                FileManager.default.createFile(atPath: codeFile, contents: Data(code.utf8), attributes: [.posixPermissions: 0o600])
            }
            AssistantDrive.say(["state": "pairing", "s": seconds])
        case .open(let route):
            // The workspace by the start of its id: enough to tell two apart, and no address.
            let workspace = EngineAddress.workspace(of: route)?.split(separator: "/").last.map { String($0.prefix(8)) } ?? ""
            AssistantDrive.say(["state": "open", "workspace": .string(workspace), "s": seconds])
        case .refused(let why): AssistantDrive.say(["state": "refused", "why": .string(why), "s": seconds])
        case .gone(let why): AssistantDrive.say(["state": "gone", "why": .string(why), "s": seconds])
        case .failed(let why): AssistantDrive.say(["state": "failed", "why": .string(why), "s": seconds])
        }
    }

    /// Say what the model holds whenever it changes, as the window's views are drawn again.
    private func watch() {
        withObservationTracking { report() } onChange: { [weak self] in
            Task { @MainActor in self?.watch() }
        }
    }

    private func once(_ name: String, _ value: JSON) {
        let text = value.text
        guard said[name] != text else { return }
        said[name] = text
        AssistantDrive.say([name: value])
    }

    private func report(everything: Bool = false) {
        if everything {
            said.removeAll()
            pieces.removeAll()
        }
        switch model.session {
        case nil: break
        case .signedOut: once("session", ["is": "signedOut"])
        case .reset: once("session", ["is": "reset"])
        case .asked(let ask):
            once("session", ["is": "asked", "heading": .string(ask.heading), "said": .array(ask.said.map(JSON.string)),
                             "go": .string(ask.go), "stop": .string(ask.stop), "site": .string(ask.site)])
        case .pairing: once("session", ["is": "pairing"])
        // The code itself goes into --code-file and is never printed.
        case .confirming: once("session", ["is": "confirming"])
        case .signedIn(let person):
            once("session", ["is": "signedIn", "name": .string(person.name), "workspace": .string(person.workspace),
                             "person": .string(String(person.id.value.prefix(6)))])
        }
        // Three words: from the start until a first try has ended it is not known.
        switch model.reach {
        case .unknown: once("reach", .string("unknown"))
        case .reachable: once("reach", .string("reachable"))
        case .unreachable: once("reach", .string("unreachable"))
        }
        // Whether the window offers the way in where it would say that Tilo cannot be reached.
        once("joinStops", .bool(model.joinStops))
        // Whether the way in goes on from an approval: the device was approved and has not got in.
        once("joinApproved", .bool(model.joinApproved != nil))
        switch model.setup {
        case .unknown: once("setup", ["is": "unknown"])
        case .findingAssistant: once("setup", ["is": "findingAssistant"])
        case .makingWorkspace: once("setup", ["is": "makingWorkspace"])
        case .makingAssistant: once("setup", ["is": "makingAssistant"])
        case .noAssistant(let why): once("setup", ["is": "noAssistant", "why": .string(why)])
        case .chatPermission(let permission):
            let state: String = switch permission {
            case .needed: "needed"
            case .pending: "pending"
            case .joining: "joining"
            case .declined: "declined"
            case .expired: "expired"
            case .revoked: "revoked"
            case .unavailable: "unavailable"
            }
            once("setup", ["is": "chatPermission", "state": .string(state)])
        case .needsProvider(let providers): once("setup", ["is": "needsProvider", "providers": .array(providers.map { .string($0.id.value) })])
        case .needsModel(let models): once("setup", ["is": "needsModel", "models": .number(Double(models.count))])
        case .ready: once("setup", ["is": "ready"])
        }
        let conversation = model.conversation(.main)
        once("conversation", ["loaded": .bool(conversation.isLoaded), "pieces": .number(Double(conversation.items.count))])
        once("answering", .bool(conversation.isAnswering))
        for item in conversation.items {
            let line = AssistantDrive.line(of: item)
            guard pieces[item.id] != line.text else { continue }
            let known = pieces[item.id] != nil
            pieces[item.id] = line.text
            AssistantDrive.say([known ? "changed" : "piece": line, "ms": .number((Date().timeIntervalSince1970 * 1000).rounded())])
        }
        // What was drawn of each reply while it grew, counted each time the model holds something
        // new, as the window's views are drawn again then. Said once the answer's writing has
        // ended, and again where a reply changed after that.
        growth.drew(conversation, at: Date())
        if !conversation.isAnswering {
            for (id, seen) in growth.seen.sorted(by: { $0.value.first < $1.value.first }) {
                var line: [String: JSON] = ["id": .string(id.value), "drew": .number(Double(seen.drew)),
                                            "letters": .number(Double(seen.letters)),
                                            "firstMs": .number((seen.first.timeIntervalSince1970 * 1000).rounded()),
                                            "lastMs": .number((seen.last.timeIntervalSince1970 * 1000).rounded())]
                if let sent, sent <= seen.first {
                    line["firstAfterSendMs"] = .number((seen.first.timeIntervalSince(sent) * 1000).rounded())
                    line["lastAfterSendMs"] = .number((seen.last.timeIntervalSince(sent) * 1000).rounded())
                }
                let text = JSON.object(line).text
                guard said["grew-\(id.value)"] != text else { continue }
                said["grew-\(id.value)"] = text
                AssistantDrive.say(["grew": .object(line)])
            }
        }
        if let problem = model.problem { once("problem", .string(problem)) } else { said["problem"] = nil }
        once("beside", ["subAgents": .number(Double(model.subAgents.count)), "tasks": .number(Double(model.tasks.count)),
                        "automations": .number(Double(model.automations.count)), "models": .number(Double(model.models.count)),
                        "model": model.supervision.model.map(JSON.string) ?? .null,
                        "logins": .number(Double(model.supervision.logins.count))])
        // What the assistant view's Model line offers under the models, and whether the provider
        // step stands in the main chat again.
        once("ways", .array(model.keyWays.map { way -> JSON in
            switch way {
            case .anotherKey(let provider): return ["way": "another-key", "provider": .string(provider.id.value), "says": .string(way.words)]
            case .anotherProvider: return ["way": "link-another", "says": .string(way.words)]
            }
        }))
        if let step = model.keyStep {
            once("keyStep", ["is": "shown", "picked": .string(step.picked.value),
                             "providers": .array(step.providers.map { ["id": .string($0.id.value), "linked": .bool($0.isLinked)] })])
        } else {
            once("keyStep", ["is": "away"])
        }
        // The Tasks panel's list: each task by its name, where it is listed and the line under it.
        once("tasks", .array(model.tasks.map { task in
            ["title": .string(task.title), "state": .string(String(describing: task.state)),
             "note": task.note.map(JSON.string) ?? .null]
        }))
        // What the main chat offers under its newest reply, where that reply's model gave no answer.
        if let offered = model.replyWay(in: .main) {
            once("replyWay", ["item": .string(offered.item.value), "says": .string(offered.way.words)])
        } else {
            once("replyWay", .null)
        }
        once("comes", .array(model.automations.map { coming in
            ["title": .string(coming.title), "when": .string(coming.summary), "on": .bool(coming.isOn),
             "next": coming.next.map { .number($0.timeIntervalSince1970) } ?? .null]
        }))
    }

    private static func line(of item: Item) -> JSON {
        var line: [String: JSON] = ["id": .string(item.id.value), "at": .number(item.at.timeIntervalSince1970)]
        if let over = item.task { line["under"] = .string(over.title) }
        if item.isImportant { line["important"] = true }
        switch item.content {
        case .message(let message):
            line["kind"] = "message"
            line["author"] = switch message.author {
            case .person: "person"
            case .assistant: "assistant"
            case .mainAgent: "mainAgent"
            }
            line["text"] = .string(message.text)
            line["live"] = .number(Double(message.live.count))
            if message.live.contains(where: \.theirsToEnd) { line["endsShare"] = true }
            // The files a message carries, as their tags say them: by name and size.
            if !message.attachments.isEmpty {
                line["files"] = .array(message.attachments.map { file in
                    ["id": .string(file.id.value), "name": .string(file.name), "bytes": file.bytes.map { .number(Double($0)) } ?? .null]
                })
            }
            // The typed cause their server says with a reply that got no answer; never its words.
            switch message.keyFault {
            case .key(let provider, _)?: line["fault"] = ["is": "key", "provider": .string(provider.value)]
            case .noProvider?: line["fault"] = ["is": "no-provider"]
            case nil: break
            }
            // What the reply's turn does, as the line at the conversation's foot says it.
            if let turn = message.turn {
                let now: JSON = switch turn.now {
                case .thinking(let words): ["is": "thinking", "words": JSON.string(words)]
                case .step(let words): ["is": "step", "words": words.map { JSON.string($0) } ?? JSON.null]
                case .writing: ["is": "writing"]
                }
                line["turn"] = ["now": now, "steps": .number(Double(turn.steps.count)),
                                "failed": .number(Double(turn.steps.filter { $0.outcome == .failed }.count))]
            }
        case .question(let question):
            line["kind"] = "question"
            line["text"] = .string(question.text)
            line["choices"] = .array(question.choices.map { .string($0.id.value) })
            line["answer"] = question.answer.map { .string($0.value) } ?? .null
        case .secret(let request):
            line["kind"] = "secret"
            line["text"] = .string(request.text)
            line["name"] = .string(request.name)
            line["state"] = switch request.state {
            case .waiting: "waiting"
            case .kept: "kept"
            case .declined: "declined"
            case .refused: "refused"
            }
        case .report(let words):
            line["kind"] = "report"
            line["text"] = .string(words)
        case .reminder(let words):
            line["kind"] = "reminder"
            line["text"] = .string(words)
        case .brief(let brief):
            line["kind"] = "brief"
            line["text"] = .string(brief.text)
        case .connection(let offer):
            line["kind"] = "connection"
            line["text"] = .string(offer.text)
        case .event(let card):
            line["kind"] = "event"
            line["text"] = .string(card.text)
            // What the card says and offers, as its view draws it.
            line["title"] = .string(card.automation.title)
            line["state"] = .string(card.settled == .undone ? AssistantWords.eventRemoved.said : Wording().state(card.automation))
            line["tags"] = .array(Wording().cardTags(card.automation).map(JSON.string))
            line["choices"] = .array(card.choices.map { .string($0.id.value) })
            line["shown"] = card.shown.map(JSON.string) ?? .null
            line["settled"] = card.settled.map { JSON.string($0 == .undone ? "undone" : "asksEachTime") } ?? .null
        }
        return .object(line)
    }

    private func take(_ text: String) async {
        guard let step = JSON(text: text), let what = step["do"]?.string else {
            AssistantDrive.say(["error": "a step is a JSON object with \"do\""])
            return
        }
        let words = { (name: String) -> String? in
            step[name]?.string.flatMap { try? String(contentsOfFile: $0, encoding: .utf8) }?
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }
        switch what {
        case "pair":
            guard let link = words("file") else { AssistantDrive.say(["error": "pair takes a file that holds a link"]); return }
            model.pair(with: link)
            AssistantDrive.say(["asked": "pair"])
        case "open-address":
            // As the system hands the app an address of its own. The file holds one, which is never printed.
            guard let address = words("file").flatMap({ URL(string: $0) }) else {
                AssistantDrive.say(["error": "open-address takes a file that holds an address"])
                return
            }
            AppAddress.open(address, in: model)
            AssistantDrive.say(["asked": "open-address"])
        case "send":
            guard let typed = step["text"]?.string else { AssistantDrive.say(["error": "send takes text"]); return }
            // As the message field does: the words stand in the draft with the files picked
            // under it, and Send takes them.
            let files = (step["files"]?.array ?? []).compactMap(\.string).map { Draft.File(url: URL(fileURLWithPath: $0)) }
            model.drafts[.main] = Draft(text: typed, files: files)
            sent = Date()
            model.send(in: .main)
            AssistantDrive.say(["asked": "send", "ms": .number((Date().timeIntervalSince1970 * 1000).rounded())])
        case "open":
            // As a press on a file's tag. What lies on this device then is said by its name,
            // its length and a sum over its bytes: never the link, and nothing is shown.
            guard let file = step["file"]?.string else { AssistantDrive.say(["error": "open takes file"]); return }
            if let fetched = await model.file(of: .init(file)), let bytes = try? Data(contentsOf: fetched) {
                AssistantDrive.say(["opened": ["name": .string(fetched.lastPathComponent), "bytes": .number(Double(bytes.count)),
                                               "sha256": .string(SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined())]])
            } else {
                AssistantDrive.say(["opened": false, "why": .string(model.problem ?? "")])
            }
        case "answer":
            guard let question = step["question"]?.string, let choice = step["choice"]?.string else {
                AssistantDrive.say(["error": "answer takes question and choice"])
                return
            }
            model.answer(.init(question), with: .init(choice))
            AssistantDrive.say(["asked": "answer"])
        case "keep":
            guard let request = step["request"]?.string, let typed = words("file") else {
                AssistantDrive.say(["error": "keep takes request and a file that holds what was typed"])
                return
            }
            let kept = await model.keep(Secret(typed), for: .init(request))
            AssistantDrive.say(["kept": .bool(kept)])
        case "link":
            guard let preset = step["preset"]?.string, let typed = words("file") else {
                AssistantDrive.say(["error": "link takes preset and a file that holds what was typed"])
                return
            }
            guard let chosen = step["model"]?.string else {
                // As the provider step does: the key from the protected field, through the window's model.
                let taken = await model.link(.init(preset), key: Secret(typed))
                if taken {
                    AssistantDrive.say(["linked": ["models": .number(Double(model.models.count))]])
                } else {
                    AssistantDrive.say(["linked": false, "why": .string(model.problem ?? "")])
                }
                return
            }
            do {
                let offered = try await source.link(provider: preset, key: Secret(typed), model: chosen)
                AssistantDrive.say(["linked": ["models": .number(Double(offered.count))]])
            } catch SourceProblem.refused(let why) {
                AssistantDrive.say(["linked": false, "why": .string(why)])
            } catch {
                AssistantDrive.say(["linked": false, "why": "unreachable"])
            }
        case "event":
            guard let name = step["name"]?.string, let when = step["when"]?.string, let instruction = step["instruction"]?.string else {
                AssistantDrive.say(["error": "event takes name, when and instruction"])
                return
            }
            do {
                try await source.setUpEvent(name: name, when: when, instruction: instruction)
                AssistantDrive.say(["setUp": true])
            } catch SourceProblem.refused(let why) {
                AssistantDrive.say(["setUp": false, "why": .string(why)])
            } catch {
                AssistantDrive.say(["setUp": false, "why": "unreachable"])
            }
        case "task":
            // As the plus of the Tasks panel: what was typed is given to the assistant as a task.
            guard let title = step["title"]?.string else { AssistantDrive.say(["error": "task takes title"]); return }
            do {
                try await source.addTask(title)
                AssistantDrive.say(["taskAdded": true])
            } catch SourceProblem.refused(let why) {
                AssistantDrive.say(["taskAdded": false, "why": .string(why)])
            } catch {
                AssistantDrive.say(["taskAdded": false, "why": "unreachable"])
            }
        case "add-device", "stop-pairing":
            // The person's own press under what Tilo asked: whoever drives this program is its person.
            model.answerPairing(goOn: what == "add-device")
            AssistantDrive.say(["asked": .string(what)])
        case "chat-permission":
            _ = await model.requestChatPermission()
            AssistantDrive.say(["asked": "chat-permission"])
        case "assistant-work-permission":
            _ = await model.requestAssistantWorkPermission()
            AssistantDrive.say(["asked": "assistant-work-permission"])
        case "assistant-app-permission":
            guard let given = step["apps"]?.array else {
                AssistantDrive.say(["error": "choose an explicit list of apps"])
                return
            }
            let apps = given.compactMap(\.string)
            guard apps.count == given.count else { AssistantDrive.say(["error": "each selected app needs its id"]); return }
            let asked = await model.requestAssistantAppPermission(apps: apps)
            AssistantDrive.say(["requested": .bool(asked)])
        case "revoke-chat-permission":
            guard step["confirm"]?.bool == true, let access = model.supervision.assistantAccess else {
                AssistantDrive.say(["error": "revoking access needs a current grant and confirmation"])
                return
            }
            _ = await model.revokeChatPermission(expected: access)
            AssistantDrive.say(["asked": "revoke-chat-permission"])
        case "check-chat-permission":
            await model.checkChatPermission()
            AssistantDrive.say(["asked": "check-chat-permission"])
        case "make-assistant":
            model.makeAssistant()
            AssistantDrive.say(["asked": "make-assistant"])
        case "sign-out":
            // As "Sign out of this device" under the person's name does. What follows is said as
            // the window would draw it: the way in, with nobody signed in.
            model.signOut()
            AssistantDrive.say(["asked": "sign-out"])
        case "choose":
            guard let chosen = step["model"]?.string else { AssistantDrive.say(["error": "choose takes model"]); return }
            model.choose(.init(chosen))
            AssistantDrive.say(["asked": "choose"])
        case "another-key":
            // As the Model line's "Use another key for NAME": one of the ways the model offers there.
            guard let provider = step["provider"]?.string else { AssistantDrive.say(["error": "another-key takes provider"]); return }
            let offered = model.keyWays.first { way in
                if case .anotherKey(let linked) = way { linked.id.value == provider } else { false }
            }
            guard let way = offered else { AssistantDrive.say(["error": "no other key is offered for this provider"]); return }
            model.take(way)
            AssistantDrive.say(["asked": "another-key"])
        case "link-another":
            guard model.keyWays.contains(.anotherProvider) else { AssistantDrive.say(["error": "no other provider is offered"]); return }
            model.take(.anotherProvider)
            AssistantDrive.say(["asked": "link-another"])
        case "not-now":
            model.putAwayKeyStep()
            AssistantDrive.say(["asked": "not-now"])
        case "reply-way":
            // As the one press under the newest reply whose model gave no answer for the key's sake.
            guard let offered = model.replyWay(in: .main) else { AssistantDrive.say(["error": "nothing is offered under the newest reply"]); return }
            model.take(offered)
            AssistantDrive.say(["asked": "reply-way"])
        case "stop":
            model.stopAnswering(in: .main)
            AssistantDrive.say(["asked": "stop"])
        case "models":
            model.loadModels()
            AssistantDrive.say(["asked": "models"])
        case "saw":
            guard let item = step["item"]?.string else { AssistantDrive.say(["error": "saw takes item"]); return }
            model.saw(.init(item), in: .main)
            AssistantDrive.say(["asked": "saw"])
        case "press":
            // As a press on a button under a piece's notice.
            guard let item = step["item"]?.string, let offer = step["offer"]?.string else {
                AssistantDrive.say(["error": "press takes item and offer"])
                return
            }
            do {
                try await source.take(.init(offer), of: .init(item), in: .main)
                AssistantDrive.say(["pressed": true])
            } catch SourceProblem.refused(let why) {
                AssistantDrive.say(["pressed": false, "why": .string(why)])
            } catch {
                AssistantDrive.say(["pressed": false, "why": "unreachable"])
            }
        case "show-live":
            // As the place of a live thing in a message does when it comes to the front.
            guard let item = step["item"]?.string else { AssistantDrive.say(["error": "show-live takes item"]); return }
            guard let thing = live(of: item) else { AssistantDrive.say(["liveShown": ["state": "none"]]); return }
            let asked = Date()
            await source.stage.bring(thing)
            // No screen asks a web view in no window for a frame: there the clock asks, as for a
            // page nobody sees, so the thing brought up goes on drawing for the check.
            if source.engine.view.window == nil { source.engine.setSeen(false) }
            var said: [String: JSON] = ["reference": .string(thing.reference),
                                        "ms": .number((Date().timeIntervalSince(asked) * 1000).rounded())]
            switch source.stage.state(of: thing) {
            case .live: said["state"] = "live"
            case .loading: said["state"] = "loading"
            case .asleep: said["state"] = "asleep"
            case .offline: said["state"] = "offline"
            case .failed(let why):
                said["state"] = "failed"
                said["why"] = .string(why)
                switch source.stage.troubles[thing.id] {
                case .notOffered: said["trouble"] = "notOffered"
                case .notShown: said["trouble"] = "notShown"
                case nil: break
                }
            }
            AssistantDrive.say(["liveShown": .object(said)])
        case "live-text":
            guard let item = step["item"]?.string else { AssistantDrive.say(["error": "live-text takes item"]); return }
            guard let thing = live(of: item) else { AssistantDrive.say(["liveText": ["held": false]]); return }
            do {
                let read = try await source.engine.invoke("read-text", on: "word-processor.document", [:], in: thing.id)
                AssistantDrive.say(["liveText": ["held": true, "text": read["output"]?["value"]?["content"]?["markdown"] ?? .null]])
            } catch {
                AssistantDrive.say(["liveText": ["held": true, "text": .null, "why": .string("\(error)")]])
            }
        case "live-read":
            guard let item = step["item"]?.string else { AssistantDrive.say(["error": "live-read takes item"]); return }
            guard let thing = live(of: item) else { AssistantDrive.say(["liveRead": ["held": false]]); return }
            var said: [String: JSON] = ["held": true]
            if let asks = source.stage.sizes[thing.id] {
                said["asks"] = ["width": .number(Double(asks.width)), "height": .number(Double(asks.height))]
            }
            let view = source.engine.view
            if view.window == nil { source.engine.setSeen(false) }
            said["view"] = ["width": .number(Double(view.frame.width)), "height": .number(Double(view.frame.height)),
                            "inAWindow": .bool(view.window != nil)]
            let file = step["picture"]?.string
            do {
                said["mount"] = try await source.engine.drawn(live: thing.id)
            } catch {
                said["why"] = .string("\(error)")
            }
            AssistantDrive.say(["liveRead": .object(said)])
            // The web view's own picture of itself. A view in no window may never answer: it is asked
            // beside the steps and said in a line of its own when it comes.
            if let file {
                Task { @MainActor in
                    guard let image = try? await view.takeSnapshot(configuration: nil), let tiff = image.tiffRepresentation,
                          let bitmap = NSBitmapImageRep(data: tiff),
                          let png = bitmap.representation(using: .png, properties: [:])
                    else { AssistantDrive.say(["liveSnapshot": ["came": false]]); return }
                    try? png.write(to: URL(fileURLWithPath: file + ".snapshot.png"))
                    AssistantDrive.say(["liveSnapshot": ["came": true, "bytes": .number(Double(png.count)),
                                                         "width": .number(Double(bitmap.pixelsWide)),
                                                         "height": .number(Double(bitmap.pixelsHigh))]])
                }
            }
        case "stop-sharing":
            // As the press on Stop sharing under a live thing the person's own assistant shared for them.
            guard let item = step["item"]?.string else { AssistantDrive.say(["error": "stop-sharing takes item"]); return }
            guard let piece = model.conversation(.main).items.first(where: { $0.id.value == item }),
                  case .message(let message) = piece.content, let thing = message.live.first, thing.theirsToEnd
            else { AssistantDrive.say(["stoppedSharing": ["offered": false]]); return }
            do {
                try await source.stopSharing(thing.id)
                AssistantDrive.say(["stoppedSharing": ["offered": true, "taken": true]])
            } catch SourceProblem.refused(let why) {
                AssistantDrive.say(["stoppedSharing": ["offered": true, "taken": false, "why": .string(why)]])
            } catch {
                AssistantDrive.say(["stoppedSharing": ["offered": true, "taken": false, "why": "unreachable"]])
            }
        case "state":
            report(everything: true)
        case "quit":
            AssistantDrive.say(["end": true])
            exit(0)
        default:
            AssistantDrive.say(["error": .string("no step \(what)")])
        }
    }

    /// The live thing that stands for the piece `item` of the main chat, as the stage takes it.
    private func live(of item: String) -> TiloLive.LiveThing? {
        guard let piece = model.conversation(.main).items.first(where: { $0.id.value == item }),
              case .message(let message) = piece.content, let thing = message.live.first
        else { return nil }
        return TiloLive.LiveThing(id: thing.id.value, title: thing.title, reference: thing.reference)
    }

    private static func say(_ line: JSON) {
        print(line.text)
        fflush(stdout)
    }
}
#endif
