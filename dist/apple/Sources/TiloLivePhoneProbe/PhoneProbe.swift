#if os(iOS)
import ShellCore
import SwiftUI
import TiloLive
import UIKit

// The engine in an iPhone app with nothing of Tilo on its screen, for a check to drive in a
// simulator. Its one view is the engine's unseen place (`EngineRoom`) in the app's root view:
// where an iPhone app that uses TiloLive puts it, because a web view runs its page there only
// while it stands in a window.
//
// It is told what to do by files in its own Documents folder, which a check on the Mac reaches
// (`simctl get_app_container … data`), and says what happens on its standard output, one JSON
// object a line (`simctl launch --console`):
//   site    the Tilo site's origin
//   store   the store's UUID, the same at every launch: the device it holds is kept
//   link    a pairing link; without one it opens `route`
//   route   where it stood when it was in (it writes this itself)
//   code    the code to compare (it writes this itself)
//   send    words to send through Chat's own send once it is in and has read what is there
// It prints no link, no code and no key.

@MainActor
final class PhoneProbe: ObservableObject {
    let stage: LiveStage?
    private let engine: TiloEngine?
    private let folder = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    private let began = Date()
    private var known: [String] = []
    private var looking = false
    private var again: String?
    private var started = false
    private var sent = false

    init() {
        let folder = folder
        let read = { (name: String) -> String? in
            (try? String(contentsOf: folder.appendingPathComponent(name), encoding: .utf8))?
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }
        guard let site = read("site"), let settings = try? ShellSettings(site: site),
              let store = read("store").flatMap(UUID.init(uuidString:))
        else {
            engine = nil
            stage = nil
            PhoneProbe.say(["error": "Documents/site names a Tilo site and Documents/store a UUID"])
            return
        }
        let engine = TiloEngine(settings: settings, store: .kept(store))
        self.engine = engine
        stage = LiveStage(engine: engine)
    }

    private func words(_ name: String) -> String? {
        (try? String(contentsOf: folder.appendingPathComponent(name), encoding: .utf8))?
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func keep(_ text: String, as name: String) {
        try? Data(text.utf8).write(to: folder.appendingPathComponent(name), options: [.atomic, .completeFileProtection])
    }

    /// Begin once the root view stands in the window.
    func run() {
        guard let engine, !started else { return }
        started = true
        Task { for await state in engine.states() { await stood(state) } }
        Task {
            for await _ in engine.changes() { await look(after: "the page's word") }
        }
        do {
            if let link = words("link").flatMap(URL.init(string:)) {
                try engine.pair(with: link)
            } else if let route = words("route") {
                try engine.open(route: route)
            } else {
                PhoneProbe.say(["error": "Documents/link holds a pairing link, or Documents/route a route"])
            }
        } catch {
            PhoneProbe.say(["error": "that is no address of this site"])
        }
        // Where the engine's view stands, and what the page it holds can do there.
        Task {
            try? await Task.sleep(for: .seconds(3))
            let view = engine.view
            let asked = try? await view.callAsyncJavaScript(
                "return JSON.stringify({gpu: typeof navigator.gpu, visible: document.visibilityState, entry: document.documentElement.dataset.entry ?? null, arrival: document.documentElement.dataset.arrival ?? null, engine: typeof globalThis.__tiloLive})",
                arguments: [:], in: nil, contentWorld: .page)
            PhoneProbe.say(["view": ["inWindow": .bool(view.window != nil), "inRootView": .bool(view.superview != nil),
                                     "width": .number(view.frame.width), "height": .number(view.frame.height)],
                            "page": (asked as? String).flatMap(JSON.init(text:)) ?? .null])
        }
    }

    private func stood(_ state: TiloEngine.State) async {
        let seconds = JSON.number((Date().timeIntervalSince(began) * 10).rounded() / 10)
        switch state {
        case .away: break
        case .arriving(let step):
            // A page that stopped on its way in and is loaded once more says why it stopped.
            var line: [String: JSON] = ["standing": "arriving", "step": .string(step), "s": seconds]
            if step == "load", let why = engine?.lastStop { line["stoppedBefore"] = .string(String(why.prefix(300))) }
            PhoneProbe.say(.object(line))
            // Its page is still the one that stopped, for a moment: what that page says of itself.
            if step == "load", engine?.lastStop != nil { await details() }
        case .pairing(let code):
            keep(code, as: "code")
            PhoneProbe.say(["standing": "pairing", "s": seconds])
        case .open(let route):
            keep(route, as: "route")
            let workspace = EngineAddress.workspace(of: route)?.split(separator: "/").last.map { String($0.prefix(8)) } ?? ""
            PhoneProbe.say(["standing": "open", "workspace": .string(workspace), "s": seconds])
            await look(after: "getting in")
        case .refused(let why): PhoneProbe.say(["standing": "refused", "why": .string(why), "s": seconds])
        case .failed(let why):
            PhoneProbe.say(["standing": "failed", "why": .string(why), "s": seconds])
            await details()
        }
    }

    /// What the page itself says where it could not go on: its own words and their details, and
    /// whether a key that cannot be taken out can be kept in the page's storage (a web view
    /// wraps such a key with one it keeps in the app's keychain group; a program with no such
    /// group cannot keep any, and Tilo's page keeps its device's key that way).
    private func details() async {
        guard let engine else { return }
        let asked = try? await engine.view.callAsyncJavaScript(#"""
            const keyKept = await (async () => {
              try {
                const key = await crypto.subtle.generateKey({name: 'AES-GCM', length: 256}, false, ['encrypt']);
                const db = await new Promise((resolve, reject) => {
                  const open = indexedDB.open('live-probe-key-check');
                  open.onupgradeneeded = () => open.result.createObjectStore('keys');
                  open.onsuccess = () => resolve(open.result);
                  open.onerror = () => reject(open.error);
                });
                try {
                  await new Promise((resolve, reject) => {
                    const change = db.transaction('keys', 'readwrite');
                    change.objectStore('keys').put(key, 'one');
                    change.oncomplete = resolve;
                    change.onerror = () => reject(change.error);
                    change.onabort = () => reject(change.error);
                  });
                  return 'yes';
                } finally {
                  db.close();
                  indexedDB.deleteDatabase('live-probe-key-check');
                }
              } catch (error) { return String(error?.name ?? error); }
            })();
            // No link, no code and no key is printed: long runs of hex are left out.
            const text = (document.body?.innerText ?? '').replace(/\s+/g, ' ').replace(/[0-9a-f]{24,}/gi, '#').replace(/pair=\S+/g, 'pair=#').slice(0, 1500);
            return JSON.stringify({text, entry: document.documentElement.dataset.entry ?? null, arrival: document.documentElement.dataset.arrival ?? null, secure: isSecureContext, keyKept, gpu: typeof navigator.gpu, transport: typeof WebTransport, locks: typeof navigator.locks, storage: typeof indexedDB});
            """#, arguments: [:], in: nil, contentWorld: .page)
        PhoneProbe.say(["page": (asked as? String).flatMap(JSON.init(text:)) ?? .null])
    }

    /// Read Chat's newest messages and print the ones not printed yet, as `TiloLiveProbe` does.
    private func look(after why: String) async {
        guard let engine, case .open = engine.state else { return }
        guard !looking else { again = why; return }
        looking = true
        defer { looking = false }
        var reason = why
        while true {
            again = nil
            var printed = 0
            do {
                for item in try await engine.chatLatest(50) where !known.contains(item) {
                    guard let message = try await engine.chatMessage(item) else { continue }
                    known.append(item)
                    printed += 1
                    PhoneProbe.say(["message": ["channel": .string(message.channel), "author": .string(String(message.author.prefix(6))),
                                                "text": .string(message.text)],
                                    "after": .string(reason), "ms": .number((Date().timeIntervalSince1970 * 1000).rounded())])
                }
            } catch {
                PhoneProbe.say(["error": .string("reading Chat: \(error)")])
            }
            PhoneProbe.say(["looked": ["after": .string(reason), "printed": .number(Double(printed)), "known": .number(Double(known.count))]])
            if !sent, let words = words("send"), !words.isEmpty {
                sent = true
                do {
                    let message = try await engine.chatSend(words, to: ChatRecipient(.conversation, id: "c_chat", name: "chat"))
                    PhoneProbe.say(["sent": ["message": .string(message)]])
                } catch {
                    PhoneProbe.say(["error": .string("sending: \(error)")])
                }
            }
            guard let next = again else { break }
            reason = next
        }
    }

    private static func say(_ line: JSON) {
        print(line.text)
        fflush(stdout)
    }
}

@main
struct PhoneProbeApp: App {
    @StateObject private var probe = PhoneProbe()

    var body: some Scene {
        WindowGroup {
            // Nothing of Tilo is on the screen: the engine's place takes no room.
            ZStack {
                Color.black.ignoresSafeArea()
                if let stage = probe.stage { EngineRoom(stage: stage) }
            }
            .task { probe.run() }
        }
    }
}
#else
@main
enum NotThisPlatform {
    static func main() {
        print("TiloLivePhoneProbe runs on an iPhone, or in a simulator.")
    }
}
#endif
