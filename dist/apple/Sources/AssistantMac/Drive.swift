#if os(macOS)
import AppKit
import AssistantCore
import AssistantTilo
import AssistantUI
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
///   {"do": "choose", "model": "…"}                    as the main chat's choice of a model
///   {"do": "send", "text": "…"}                      as the message field sends
///   {"do": "answer", "question": "…", "choice": "…"}  as a question's button answers
///   {"do": "keep", "request": "…", "file": "…"}       as the protected field keeps what was typed
///   {"do": "link", "preset": "…", "file": "…"}       as the provider step's protected field links a provider;
///                                                    with "model" the provider's default model is set as well
///   {"do": "event", "name": "…", "when": "…", "instruction": "…"}   something that comes at a time
///   {"do": "stop"}                                    stop the answer that is being written
///   {"do": "models"}   {"do": "state"}   {"do": "quit"}
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
                    try await WKWebsiteDataStore.remove(forIdentifier: store)
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
        case .asked(let ask):
            once("session", ["is": "asked", "heading": .string(ask.heading), "said": .array(ask.said.map(JSON.string)),
                             "go": .string(ask.go), "stop": .string(ask.stop)])
        case .pairing: once("session", ["is": "pairing"])
        case .confirming(let code): once("session", ["is": "confirming", "code": .string(code)])
        case .signedIn(let person):
            once("session", ["is": "signedIn", "name": .string(person.name), "workspace": .string(person.workspace),
                             "person": .string(String(person.id.value.prefix(6)))])
        }
        once("reach", .string(model.reach == .reachable ? "reachable" : "unreachable"))
        switch model.setup {
        case .unknown: once("setup", ["is": "unknown"])
        case .findingAssistant: once("setup", ["is": "findingAssistant"])
        case .noAssistant(let why): once("setup", ["is": "noAssistant", "why": .string(why)])
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
        if let problem = model.problem { once("problem", .string(problem)) } else { said["problem"] = nil }
        once("beside", ["subAgents": .number(Double(model.subAgents.count)), "tasks": .number(Double(model.tasks.count)),
                        "automations": .number(Double(model.automations.count)), "models": .number(Double(model.models.count)),
                        "model": model.supervision.model.map(JSON.string) ?? .null,
                        "logins": .number(Double(model.supervision.logins.count))])
        once("comes", .array(model.automations.map { coming in
            ["title": .string(coming.title), "when": .string(coming.summary), "on": .bool(coming.isOn),
             "next": coming.next.map { .number($0.timeIntervalSince1970) } ?? .null]
        }))
    }

    private static func line(of item: Item) -> JSON {
        var line: [String: JSON] = ["id": .string(item.id.value), "at": .number(item.at.timeIntervalSince1970)]
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
        case "send":
            guard let typed = step["text"]?.string else { AssistantDrive.say(["error": "send takes text"]); return }
            // As the message field does: the words stand in the draft, and Send takes them.
            model.drafts[.main] = Draft(text: typed)
            model.send(in: .main)
            AssistantDrive.say(["asked": "send", "ms": .number((Date().timeIntervalSince1970 * 1000).rounded())])
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
        case "add-device", "stop-pairing":
            // The person's own press under what Tilo asked: whoever drives this program is its person.
            model.answerPairing(goOn: what == "add-device")
            AssistantDrive.say(["asked": .string(what)])
        case "make-assistant":
            model.makeAssistant()
            AssistantDrive.say(["asked": "make-assistant"])
        case "choose":
            guard let chosen = step["model"]?.string else { AssistantDrive.say(["error": "choose takes model"]); return }
            model.choose(.init(chosen))
            AssistantDrive.say(["asked": "choose"])
        case "stop":
            model.stopAnswering(in: .main)
            AssistantDrive.say(["asked": "stop"])
        case "models":
            model.loadModels()
            AssistantDrive.say(["asked": "models"])
        case "state":
            report(everything: true)
        case "quit":
            AssistantDrive.say(["end": true])
            exit(0)
        default:
            AssistantDrive.say(["error": .string("no step \(what)")])
        }
    }

    private static func say(_ line: JSON) {
        print(line.text)
        fflush(stdout)
    }
}
#endif
