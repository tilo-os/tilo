#if os(macOS)
import AppKit
import Foundation
import ShellCore
import TiloLive
import WebKit

// The engine with nothing on screen, for a check to drive: this program opens no window, is
// never the app in front, and prints one JSON object a line.
//
//   TiloLiveProbe --site ORIGIN --store UUID|none (--pair LINK-FILE | --route ROUTE-FILE)
//                 [--code-file FILE] [--keep-route FILE] [--keep-record FILE]
//   TiloLiveProbe --site ORIGIN --store UUID --forget
//
// With --pair it pairs as a new device of whoever made the link in LINK-FILE, and writes the
// code a person compares into --code-file. With --route it comes back as the device its store
// already is. Once in it writes the route it is at into --keep-route, prints every message
// of Chat as it arrives (each read is started by the page's own word that something changed,
// never by a clock), and takes steps on standard input, one JSON object a line:
//   {"do": "send", "kind": "conversation", "id": "c_chat", "name": "chat", "text": "…"}
//   {"do": "add-device"}  {"do": "stop-pairing"}   the answer to what the page asks before it pairs
//   {"do": "agents", "action": "read"}    the workspace's agents as its server says them; also
//                                         "ensure-personal-assistant"
//   {"do": "person", "provider": "tilo.models", "resource": "models", "action": "list"}
//                                         one of the person's own providers; with "argument", and
//                                         with "field" and "file" for what a protected field holds
//   {"do": "window"}                      the web view goes into a window nobody can see
//   {"do": "show", "id": "doc", "app": "so.tilo.word-processor", "record": "…"}   or "new": true
//   {"do": "text", "id": "doc"}           what the live document reads as
//   {"do": "click", "x": 300, "y": 300}   a real mouse press, from the view's top left corner
//   {"do": "type", "text": "…"}           real key presses, as the system delivers them
//   {"do": "shot", "file": "…png"}        what the web view drew, from the web view itself
//   {"do": "quit"}
// After every read of Chat it prints {"looked": {...}}. It ends when its standard input ends.
//
// The window of "window" is never put on a screen and never the key window of the system: it
// is made and not ordered in, and only answers that it is visible and key, so the web view
// runs its frames and takes the events handed to it (as PageDrive holds its page).
// With --forget it removes the store, and with it the device's key, and ends.
// It prints no link, no code and no key. A store that is kept keeps the device: the program
// that made it must be the one that opens it again, because the key WebKit wraps the page's
// device key with belongs to the program that made it.

/// A window that is never put on screen and answers as one that is.
final class HeldWindow: NSWindow {
    override var isVisible: Bool { true }
    override var occlusionState: NSWindow.OcclusionState { [.visible] }
    override var screen: NSScreen? { NSScreen.screens.first }
    override var isKeyWindow: Bool { true }
    override var isMainWindow: Bool { true }
}

@MainActor
final class Probe {
    private let engine: TiloEngine
    private let codeFile: String?
    private let routeFile: String?
    /// The messages printed so far, each with the words it was printed with.
    private var known: [String: String] = [:]
    private var signals = 0
    private var reads = 0
    private var looking = false
    /// What asked for one more read while one was going on.
    private var again: String?
    private let began = Date()
    private var window: HeldWindow?
    private let size = NSSize(width: 900, height: 600)
    private var events = 0
    private let recordFile: String?

    init(engine: TiloEngine, codeFile: String?, routeFile: String?, recordFile: String?) {
        self.engine = engine
        self.codeFile = codeFile
        self.routeFile = routeFile
        self.recordFile = recordFile
    }

    func run() {
        Task { for await state in engine.states() { stood(state) } }
        Task {
            for await _ in engine.changes() {
                signals += 1
                await look(after: "the page's word")
            }
        }
    }

    private func stood(_ state: TiloEngine.State) {
        let seconds = JSON.number((Date().timeIntervalSince(began) * 10).rounded() / 10)
        switch state {
        case .away: break
        case .arriving(let step):
            // A page that stopped on its way in and is loaded once more says why it stopped.
            var line: [String: JSON] = ["state": "arriving", "step": .string(step), "s": seconds]
            if step == "load", let why = engine.lastStop { line["stoppedBefore"] = .string(String(why.prefix(300))) }
            say(.object(line))
        case .asking(let ask):
            // The page asks before it pairs. Whoever drives the probe is its person: `add-device` goes on.
            say(["state": "asking", "heading": .string(ask.heading), "said": .array(ask.said.map(JSON.string)), "s": seconds])
        case .pairing(let code):
            if let codeFile { keep(code, in: codeFile) }
            say(["state": "pairing", "s": seconds])
        case .open(let route):
            if let routeFile { keep(route, in: routeFile) }
            // The workspace by the start of its id: enough to tell two apart, and no address.
            let workspace = EngineAddress.workspace(of: route)?.split(separator: "/").last.map { String($0.prefix(8)) } ?? ""
            say(["state": "open", "app": .string(route.split(separator: "/").last.map(String.init) ?? ""),
                 "workspace": .string(workspace), "s": seconds])
            Task { await look(after: "getting in") }
        case .refused(let why): say(["state": "refused", "why": .string(why), "s": seconds])
        case .gone(let why): say(["state": "gone", "why": .string(why), "s": seconds])
        case .failed(let why): say(["state": "failed", "why": .string(why), "s": seconds])
        }
    }

    /// Read Chat's newest messages and print the ones not printed yet. What asks for a read
    /// while one is going on gets one more read of its own, so every message is printed under
    /// what really started the read that found it. After every read it says so (`looked`): a
    /// check then knows the program has caught up with what was there.
    private func look(after why: String) async {
        guard case .open = engine.state else { return }
        guard !looking else { again = why; return }
        looking = true
        defer { looking = false }
        var reason = why
        while true {
            again = nil
            var printed = 0
            do {
                reads += 1
                let latest = try await engine.chatLatest(50)
                for item in latest where known[item] == nil || latest.suffix(8).contains(item) {
                    // An agent's answer is there before its words are: the newest are read again,
                    // and printed again when their words changed.
                    guard let message = try await engine.chatMessage(item), known[item] != message.text else { continue }
                    known[item] = message.text
                    printed += 1
                    say(["message": ["id": .string(String(item.split(separator: "/").last ?? "")), "channel": .string(message.conversation),
                                     "author": .string(String(message.author.id.prefix(6))),
                                     "by": message.author.kind.map { .string($0.rawValue) } ?? .null, "text": .string(message.text),
                                     "at": .number(message.at.timeIntervalSince1970)],
                         "after": .string(reason), "ms": .number((Date().timeIntervalSince1970 * 1000).rounded())])
                }
            } catch {
                say(["error": .string("reading Chat: \(error)")])
            }
            say(["looked": ["after": .string(reason), "printed": .number(Double(printed)), "known": .number(Double(known.count))]])
            guard let next = again else { break }
            reason = next
        }
    }

    func take(_ line: String) async {
        guard let step = JSON(text: line), let what = step["do"]?.string else {
            say(["error": "a step is a JSON object with \"do\""])
            return
        }
        switch what {
        case "send":
            guard let kind = step["kind"]?.string.flatMap(ChatRecipient.Kind.init(rawValue:)), let id = step["id"]?.string,
                  let text = step["text"]?.string
            else { say(["error": "send takes kind, id, name and text"]); return }
            let asked = Date()
            do {
                let message = try await engine.chatSend(text, to: ChatRecipient(kind, id: id, name: step["name"]?.string ?? id))
                say(["sent": ["message": .string(message), "ms": .number((Date().timeIntervalSince(asked) * 1000).rounded())]])
            } catch {
                say(["error": .string("sending: \(error)")])
            }
        case "window":
            hold()
            say(["window": "held"])
        case "show":
            guard let id = step["id"]?.string, let app = step["app"]?.string else { say(["error": "show takes id and app"]); return }
            let what: TiloEngine.Mounted = step["record"]?.string.map { .record(app: app, record: $0) } ?? .new(app: app)
            let asked = Date()
            do {
                let record = try await engine.show(live: id, what)
                if let record, let recordFile { keep(record, in: recordFile) }
                say(["shown": ["id": .string(id), "record": .bool(record != nil), "ms": .number((Date().timeIntervalSince(asked) * 1000).rounded())]])
            } catch {
                say(["error": .string("showing: \(error)")])
            }
        case "text":
            do {
                let read = try await engine.invoke("read-text", on: "word-processor.document", [:], in: step["id"]?.string)
                say(["text": read["output"]?["value"]?["content"]?["markdown"] ?? .null])
            } catch {
                say(["error": .string("reading the document: \(error)")])
            }
        case "add-device", "stop-pairing":
            say(["answered": .bool(await engine.answerPairing(goOn: what == "add-device"))])
        case "agents":
            // The workspace's agents as its server says them to this device: one ask of the engine.
            guard let action = step["action"]?.string else { say(["error": "agents takes action"]); return }
            do {
                say(["agents": try await engine.askAgents(action, step["argument"]?.object ?? [:], agent: step["agent"]?.string)])
            } catch {
                say(["agents": false, "why": .string("\(error)")])
            }
        case "person":
            // One of the person's own providers on their server: one ask of the engine. What a
            // protected field holds is read from a file, and no answer says it.
            guard let provider = step["provider"]?.string.flatMap(PersonProvider.init(rawValue:)),
                  let resource = step["resource"]?.string, let action = step["action"]?.string
            else { say(["error": "person takes provider, resource and action"]); return }
            var protected: (field: String, value: Protected)?
            if let field = step["field"]?.string, let file = step["file"]?.string { protected = (field, Protected(words(of: file))) }
            do {
                say(["person": try await engine.ask(provider, action, of: resource, step["argument"]?.object ?? [:], protected: protected)])
            } catch {
                say(["person": false, "why": .string("\(error)")])
            }
        case "click":
            guard let x = step["x"]?.number, let y = step["y"]?.number else { say(["error": "click takes x and y"]); return }
            await click(at: NSPoint(x: x, y: y))
            say(["clicked": true])
        case "type":
            guard let text = step["text"]?.string else { say(["error": "type takes text"]); return }
            await type(text)
            say(["typed": .number(Double(text.count))])
        case "shot":
            guard let file = step["file"]?.string else { say(["error": "shot takes file"]); return }
            do {
                let picture = try await engine.view.takeSnapshot(configuration: nil)
                guard let drawn = picture.tiffRepresentation.flatMap(NSBitmapImageRep.init(data:)),
                      let png = drawn.representation(using: .png, properties: [:])
                else { say(["error": "the picture could not be written"]); return }
                try png.write(to: URL(fileURLWithPath: file))
                say(["shot": ["width": .number(Double(drawn.pixelsWide)), "height": .number(Double(drawn.pixelsHigh)), "bytes": .number(Double(png.count))]])
            } catch {
                say(["error": .string("taking the picture: \(error.localizedDescription)")])
            }
        case "ask":
            // For whoever writes a check: one question of the page, answered as text.
            guard let body = step["body"]?.string else { say(["error": "ask takes body"]); return }
            do {
                let answer = try await engine.view.callAsyncJavaScript(body, arguments: [:], in: nil, contentWorld: .page)
                say(["answer": .string((answer as? String) ?? String(describing: answer))])
            } catch {
                say(["error": .string("asking the page: \((error as NSError).userInfo["WKJavaScriptExceptionMessage"] ?? error.localizedDescription)")])
            }
        case "quit":
            say(["end": ["signals": .number(Double(signals)), "reads": .number(Double(reads)), "messages": .number(Double(known.count))]])
            exit(0)
        default:
            say(["error": .string("no step \(what)")])
        }
    }

    // MARK: A window nobody sees, and the mouse and the keys as the system delivers them

    private func hold() {
        guard window == nil else { return }
        let frame = NSRect(origin: .zero, size: size)
        let held = HeldWindow(contentRect: frame, styleMask: [.titled], backing: .buffered, defer: false)
        held.isReleasedWhenClosed = false
        let content = NSView(frame: frame)
        held.contentView = content
        engine.view.frame = content.bounds
        engine.view.autoresizingMask = [.width, .height]
        content.addSubview(engine.view)
        held.makeFirstResponder(engine.view)
        window = held
        engine.setSeen(true)
    }

    private func click(at point: NSPoint) async {
        guard let window else { return }
        let view = engine.view
        let made = { (type: NSEvent.EventType) -> NSEvent? in
            self.events += 1
            return NSEvent.mouseEvent(with: type, location: NSPoint(x: point.x, y: self.size.height - point.y), modifierFlags: [],
                                      timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
                                      context: nil, eventNumber: self.events, clickCount: type == .mouseMoved ? 0 : 1,
                                      pressure: type == .leftMouseDown ? 1 : 0)
        }
        if let moved = made(.mouseMoved) { view.mouseMoved(with: moved) }
        if let down = made(.leftMouseDown) { view.mouseDown(with: down) }
        try? await Task.sleep(for: .milliseconds(60))
        if let up = made(.leftMouseUp) { view.mouseUp(with: up) }
    }

    /// The keys of a US keyboard, by what they type.
    private static let keys: [Character: UInt16] = [
        "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11, "q": 12, "w": 13, "e": 14,
        "r": 15, "y": 16, "t": 17, "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "9": 25, "7": 26, "8": 28, "0": 29,
        "o": 31, "u": 32, "i": 34, "p": 35, "l": 37, "j": 38, "k": 40, ",": 43, "n": 45, "m": 46, ".": 47, " ": 49,
    ]

    private func type(_ text: String) async {
        guard let window else { return }
        for letter in text {
            let plain = Character(letter.lowercased())
            for type in [NSEvent.EventType.keyDown, .keyUp] {
                guard let event = NSEvent.keyEvent(with: type, location: .zero, modifierFlags: letter.isUppercase ? [.shift] : [],
                                                   timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
                                                   context: nil, characters: String(letter), charactersIgnoringModifiers: String(plain),
                                                   isARepeat: false, keyCode: Probe.keys[plain] ?? 0)
                else { continue }
                if type == .keyDown { engine.view.keyDown(with: event) } else { engine.view.keyUp(with: event) }
            }
            try? await Task.sleep(for: .milliseconds(15))
        }
    }

    private func keep(_ text: String, in file: String) {
        FileManager.default.createFile(atPath: file, contents: Data(text.utf8), attributes: [.posixPermissions: 0o600])
    }
}

func say(_ line: JSON) {
    print(line.text)
    fflush(stdout)
}

func argument(_ name: String) -> String? {
    let all = CommandLine.arguments
    guard let at = all.firstIndex(of: name), all.indices.contains(at + 1) else { return nil }
    return all[at + 1]
}

func stop(_ why: String) -> Never {
    FileHandle.standardError.write(Data("TiloLiveProbe: \(why)\n".utf8))
    exit(2)
}

func words(of file: String) -> String {
    guard let text = try? String(contentsOfFile: file, encoding: .utf8) else { stop("cannot read \(file)") }
    return text.trimmingCharacters(in: .whitespacesAndNewlines)
}

let app = NSApplication.shared
// Never the app in front, no icon in the Dock, no window.
app.setActivationPolicy(.prohibited)

guard let site = argument("--site"), let settings = try? ShellSettings(site: site) else { stop("--site names a Tilo site") }
let store: TiloEngine.Store
switch argument("--store") {
case "none": store = .keepsNothing
case let name?: guard let id = UUID(uuidString: name) else { stop("--store is a UUID, or none") }; store = .kept(id)
case nil: stop("--store is a UUID, or none")
}
if CommandLine.arguments.contains("--forget") {
    guard case .kept(let name) = store else { stop("--forget takes a store that is kept") }
    Task { @MainActor in
        do {
            try await TiloEngine.forget(store: name)
            say(["forgot": true])
        } catch {
            say(["forgot": false, "why": .string("\(error.localizedDescription) \((error as NSError).domain) \((error as NSError).code)")])
        }
        exit(0)
    }
    app.run()
}
let engine = TiloEngine(settings: settings, store: store)
let probe = Probe(engine: engine, codeFile: argument("--code-file"), routeFile: argument("--keep-route"),
                  recordFile: argument("--keep-record"))
probe.run()
do {
    if let file = argument("--pair") {
        guard let link = URL(string: words(of: file)) else { stop("the file holds no link") }
        try engine.pair(with: link)
    } else if let file = argument("--route") {
        try engine.open(route: words(of: file))
    } else {
        stop("--pair LINK-FILE or --route ROUTE-FILE")
    }
} catch {
    stop("that is no address of \(settings.origin)")
}

// Steps arrive on a thread of their own, so reading them never holds the page up. When whoever
// drives the program is gone, so is its standard input, and the program ends: it never stays
// behind as a device nobody looks after.
Thread.detachNewThread {
    while let line = readLine() {
        let step = line.trimmingCharacters(in: .whitespaces)
        if step.isEmpty { continue }
        Task { @MainActor in await probe.take(step) }
    }
    exit(0)
}
app.run()
#else
print("TiloLiveProbe runs on a Mac.")
#endif
