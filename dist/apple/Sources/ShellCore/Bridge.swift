import Foundation

/// Whether the system lets the app notify, in the words a page reads from
/// `Notification.permission`.
public enum NoticePermission: String, Sendable, Equatable {
    case granted
    case denied
    case notAsked = "default"
}

/// One answer a notice offers under its words.
public struct NoticeAction: Equatable, Sendable {
    public enum Kind: String, Sendable {
        case button
        /// The person types the answer in the notification.
        case text
    }

    public let id: String
    public let title: String
    public let kind: Kind
    public let placeholder: String?

    public init(id: String, title: String, kind: Kind = .button, placeholder: String? = nil) {
        self.id = id
        self.title = title
        self.kind = kind
        self.placeholder = placeholder
    }

    init?(said: [String: Any]) {
        guard let id = said["action"] as? String, !id.isEmpty, id.count <= 64,
              let title = said["title"] as? String, !title.isEmpty, title.count <= 64
        else { return nil }
        let placeholder = (said["placeholder"] as? String).flatMap { $0.isEmpty ? nil : String($0.prefix(64)) }
        self.init(id: id, title: title, kind: said["type"] as? String == "text" ? .text : .button,
                  placeholder: placeholder)
    }
}

/// A notification the page asked the system to show.
public struct PageNotice: Equatable, Sendable {
    /// The page's own name for it, which an answer goes back under.
    public let id: String
    public let title: String
    public let body: String
    /// A newer notice with the same tag replaces this one.
    public let tag: String?
    public let actions: [NoticeAction]

    public init(id: String, title: String, body: String = "", tag: String? = nil, actions: [NoticeAction] = []) {
        self.id = id
        self.title = title
        self.body = body
        self.tag = tag
        self.actions = actions
    }
}

/// Where the page stands at its site's entry page, in the page's own word for it
/// (`document.documentElement.dataset.entry`): `open` once it is in, `refused` when the host
/// turned it away, `failed` when it could not get there.
public struct PageEntry: Equatable, Sendable {
    public let state: String
    /// A device of the person's is being asked to approve this one.
    public let pairing: Bool

    public init(state: String, pairing: Bool = false) {
        self.state = state
        self.pairing = pairing
    }

    /// The site does not know this device and nothing is under way to change that: the
    /// person needs a way to bring a link in from a device where they are signed in.
    public var wantsWayIn: Bool {
        state == "refused" && !pairing
    }
}

/// A way to bring a link in at the site's entry page that only the system has.
public enum WayIn: String, CaseIterable, Sendable, Equatable {
    /// Read the code another device shows, with the camera.
    case scan
    /// Take the link out of the clipboard.
    case paste
}

/// What the shell offers on an entry page that refuses the device: one sentence that says where a code and a link
/// come from, and a button for each way this device has. The page shows it on the entry page's own
/// column, drawn as the entry page draws its own controls (`Bridge`).
public struct WayInOffer: Equatable, Sendable {
    public struct Way: Equatable, Sendable {
        public let way: WayIn
        /// What its button says.
        public let label: String

        public init(_ way: WayIn, _ label: String) {
            self.way = way
            self.label = label
        }
    }

    public let said: String
    public let ways: [Way]

    public init(said: String, ways: [Way]) {
        self.said = said
        self.ways = ways
    }
}

/// What the page tells the shell. Everything else a page posts is dropped.
public enum PageMessage: Equatable, Sendable {
    case notify(PageNotice)
    case entry(PageEntry)
    /// The person pressed one of the shell's ways in on the entry page.
    case wayIn(WayIn)
    case withdraw(id: String)
    case askPermission
    /// Words that waited for a field are in it now.
    case wordsInserted

    public init?(body: Any) {
        guard let said = body as? [String: Any], let kind = said["kind"] as? String else { return nil }
        switch kind {
        case "notify":
            guard let id = said["id"] as? String, !id.isEmpty, id.count <= 64,
                  let title = said["title"] as? String, !title.isEmpty
            else { return nil }
            let tag = (said["tag"] as? String).flatMap { $0.isEmpty ? nil : String($0.prefix(64)) }
            let actions = (said["actions"] as? [[String: Any]] ?? []).prefix(4).compactMap(NoticeAction.init(said:))
            self = .notify(PageNotice(id: id, title: String(title.prefix(256)),
                                      body: String((said["body"] as? String ?? "").prefix(2048)),
                                      tag: tag, actions: actions))
        case "withdraw":
            guard let id = said["id"] as? String, !id.isEmpty, id.count <= 64 else { return nil }
            self = .withdraw(id: id)
        case "ask-permission":
            self = .askPermission
        case "entry":
            guard let state = said["state"] as? String, state.count <= 32 else { return nil }
            self = .entry(PageEntry(state: state, pairing: said["pairing"] as? Bool ?? false))
        case "way-in":
            guard let way = (said["way"] as? String).flatMap(WayIn.init(rawValue:)) else { return nil }
            self = .wayIn(way)
        case "words-inserted":
            self = .wordsInserted
        default:
            return nil
        }
    }
}

/// What became of words the shell handed to the page.
public enum ComposeOutcome: String, Sendable, Equatable {
    /// They are in the field the person writes in.
    case inserted
    /// No field has the caret: they go in when the person next presses into one.
    case waiting
    case nothing
    /// The page is not Tilo's, or has not loaded.
    case noBridge = "no-bridge"
}

/// The script that joins Tilo's page to the system. It runs before the page's own code, in
/// the page's main frame, and does nothing on any origin but the site's.
///
/// To the page it is the Notifications API, which a system web view lacks: `Notification`
/// with `permission`, `requestPermission()`, `close()` and the `click` and `close` events,
/// as `crates/platform/tilo-notify/web/notify.mjs` uses them. Beyond what a browser gives a
/// page's own notification it takes `actions` (`{action, title, type, placeholder}`,
/// `Notification.maxActions` of them): each becomes a button in the system's notification,
/// a `type: 'text'` one a field, and the `click` event then carries `action` and `reply`.
///
/// To the shell it is `__tiloShell`: `permission(state)`, `chosen(id, action, reply)`,
/// `closed(id)` and `compose(words)`. It also tells the shell where the page stands on its
/// site's entry page, which the page writes on its root element.
///
/// A build told while it is closed gives the page where this app is reached (`push(address)`),
/// which its session hands to the host (`hearingPush`), and what a push answered
/// (`told(name, action, reply)`), which it answers through the log (`hearingTold`). A sign-out
/// (`signedOut`) has it unregister the address (`hearingSignedOut`).
///
/// On an entry page that refuses the device it shows the shell's `WayInOffer` on that page. The entry page is
/// plain markup in one dark look, set in the system's type, and it scrolls; a native bar
/// under it followed the system's appearance and stood apart. So the offer is put into the
/// entry page's own column as markup with the entry page's measures and colours: a button for each way,
/// and the sentence under them. It stands first among what the entry page offers someone who
/// already has something (`#arrival-refusal .refusal-alt` on the entry page), under the
/// actions of the screen for a refusal of another kind (`#arrival-failure`), and under the
/// status line on a page that has neither, such as the page that shows one app. A press is told to the shell
/// (`way-in`), and what the shell has to say of a link it refuses is shown under the buttons
/// (`refused(words)`). The offer goes when the page is in, and while a pairing is under way.
///
/// An entry page that draws these options itself marks its element `data-way-in`, reads what this
/// device has from `__tiloShell.ways` and calls `__tiloShell.wayIn(way)`; nothing is added
/// to such an entry page.
public enum Bridge {
    /// The message handler the page's side posts to.
    public static let name = "tiloShell"

    public static func script(origin: String, permission: NoticePermission, offer: WayInOffer? = nil,
                              push: String? = nil) -> String {
        source
            .replacingOccurrences(of: "%ORIGIN%", with: literal(origin))
            .replacingOccurrences(of: "%PERMISSION%", with: literal(permission.rawValue))
            .replacingOccurrences(of: "%OFFER%", with: offer.map(literal) ?? "null")
            .replacingOccurrences(of: "%PUSH%", with: push.map(literal) ?? "null")
    }

    /// `offer` as a JavaScript object literal.
    static func literal(_ offer: WayInOffer) -> String {
        let said: [String: Any] = ["said": offer.said,
                                   "ways": offer.ways.map { ["way": $0.way.rawValue, "label": $0.label] }]
        let encoded = try? JSONSerialization.data(withJSONObject: said, options: [.sortedKeys])
        return encoded.flatMap { String(data: $0, encoding: .utf8) } ?? "null"
    }

    /// `text` as a JavaScript string literal.
    static func literal(_ text: String) -> String {
        let encoded = try? JSONSerialization.data(withJSONObject: text, options: .fragmentsAllowed)
        return encoded.flatMap { String(data: $0, encoding: .utf8) } ?? "\"\""
    }

    private static let source = #"""
    (() => {
      'use strict';
      if (location.origin !== %ORIGIN% || window.top !== window || globalThis.__tiloShell) return;
      const port = globalThis.webkit?.messageHandlers?.tiloShell;
      if (!port) return;
      const tell = (kind, said = {}) => { try { port.postMessage({kind, ...said}); } catch { /* the shell is gone */ } };
      const cut = (value, most) => String(value ?? '').slice(0, most);

      // Notifications: the system's, shown by the shell.
      let permission = %PERMISSION%;
      const asking = [];
      const shown = new Map();
      const names = new WeakMap();
      let serial = 0;

      // Where this app is reached while it is closed, and what a push answered: the page's
      // own session hands the first to the person's host and answers the second through the
      // log, so a push and the page name a thing one way.
      let push = %PUSH%;
      const pushHeard = [];
      let told = null;
      const toldHeard = [];
      let out = false;
      const outHeard = [];
      class ShellNotification extends EventTarget {
        static get permission() { return permission; }
        static get maxActions() { return 4; }
        static requestPermission(done) {
          const asked = permission === 'default'
            ? new Promise(resolve => { asking.push(resolve); tell('ask-permission'); })
            : Promise.resolve(permission);
          if (typeof done === 'function') void asked.then(done);
          return asked;
        }
        constructor(title, options = {}) {
          super();
          const id = `${Date.now().toString(36)}-${++serial}`;
          names.set(this, id);
          Object.defineProperties(this, {
            title: {value: cut(title, 256), enumerable: true},
            body: {value: cut(options?.body, 2048), enumerable: true},
            tag: {value: cut(options?.tag, 64), enumerable: true},
            data: {value: options?.data ?? null, enumerable: true},
          });
          if (permission !== 'granted') {
            queueMicrotask(() => this.dispatchEvent(new Event('error')));
            return;
          }
          const actions = (Array.isArray(options?.actions) ? options.actions : []).slice(0, 4).map(action => ({
            action: cut(action?.action, 64), title: cut(action?.title, 64),
            type: action?.type === 'text' ? 'text' : 'button', placeholder: cut(action?.placeholder, 64),
          })).filter(action => action.action && action.title);
          shown.set(id, this);
          tell('notify', {id, title: this.title, body: this.body, tag: this.tag, actions});
          queueMicrotask(() => this.dispatchEvent(new Event('show')));
        }
        close() {
          const id = names.get(this);
          if (!shown.delete(id)) return;
          tell('withdraw', {id});
          this.dispatchEvent(new Event('close'));
        }
      }
      for (const type of ['click', 'close', 'error', 'show']) {
        const kept = new WeakMap();
        Object.defineProperty(ShellNotification.prototype, `on${type}`, {
          configurable: true,
          get() { return kept.get(this) ?? null; },
          set(handler) {
            const before = kept.get(this);
            if (before) this.removeEventListener(type, before);
            kept.delete(this);
            if (typeof handler !== 'function') return;
            kept.set(this, handler);
            this.addEventListener(type, handler);
          },
        });
      }
      Object.defineProperty(globalThis, 'Notification', {value: ShellNotification, configurable: true, writable: true});

      // Words from the shell (dictated, shared, typed into a notification) go where the
      // person writes. Never into a password field.
      let waiting = null;
      const plain = ['', 'text', 'search', 'url', 'email', 'tel'];
      const takesWords = el => Boolean(el) && !el.disabled && !el.readOnly && (el.isContentEditable
        || el.tagName === 'TEXTAREA'
        || (el.tagName === 'INPUT' && plain.includes((el.getAttribute('type') ?? '').toLowerCase())));
      const put = (el, words) => {
        // As a paste first. Tilo's page takes a paste as words wherever its own text has the
        // caret, line breaks included; typed line by line, a line break would send a message.
        const carried = new DataTransfer();
        carried.setData('text/plain', words);
        const paste = new ClipboardEvent('paste', {clipboardData: carried, bubbles: true, cancelable: true});
        if (!el.dispatchEvent(paste)) return true;
        if (document.execCommand('insertText', false, words)) return true;
        if (typeof el.setRangeText !== 'function') return false;
        const end = el.value.length;
        el.setRangeText(words, el.selectionStart ?? end, el.selectionEnd ?? end, 'end');
        el.dispatchEvent(new InputEvent('input', {inputType: 'insertText', data: words, bubbles: true}));
        return true;
      };
      const compose = words => {
        if (typeof words !== 'string' || !words) return 'nothing';
        const el = document.activeElement;
        if (takesWords(el) && put(el, words)) return 'inserted';
        // Nothing has the caret. Whatever waits already keeps its place in front.
        waiting = waiting === null ? words : `${waiting}\n${words}`;
        return 'waiting';
      };
      document.addEventListener('focusin', event => {
        if (waiting === null || !takesWords(event.target)) return;
        // Once the focus has settled: the page moves its own editor to the caret first.
        setTimeout(() => {
          const el = document.activeElement;
          if (waiting === null || !takesWords(el)) return;
          const words = waiting;
          waiting = null;
          if (put(el, words)) tell('words-inserted'); else waiting = words;
        }, 0);
      }, true);

      // Where the page stands at its site's entry page.
      const root = document.documentElement;
      if (root) {
        const entry = () => tell('entry', {state: cut(root.dataset.entry, 32), pairing: Boolean(root.dataset.pairingCode)});
        new MutationObserver(entry).observe(root, {attributes: true, attributeFilter: ['data-entry', 'data-pairing-code']});
      }

      // The way in. While the site keeps this device out, what only the system can do for
      // the person stands on the entry page's own column, drawn as the entry page draws its own controls.
      const offer = %OFFER%;
      const ways = Object.freeze(offer ? offer.ways.map(one => one.way) : []);
      const wayIn = way => {
        if (!ways.includes(way)) return false;
        tell('way-in', {way});
        return true;
      };
      // The entry page's measures and colours (index.html of the entry page): a control's height
      // and corner, the raised fill of a second choice, its focus ring, and under the
      // controls a note on the entry page's quiet tone, as under its own wait list.
      const look = `
        .tilo-shell-way-in { box-sizing: border-box; display: grid; justify-items: center; gap: 10px; width: 100%; margin: 0 0 20px; padding: 0; text-align: center; font: 13px/1.5 system-ui, sans-serif; color: #7f7f89; }
        .tilo-shell-way-in > p { margin: 0; font-size: 13px; line-height: 1.5; color: #7f7f89; text-wrap: balance; }
        .tilo-shell-way-in > p:empty { display: none; }
        .tilo-shell-way-in > p[role=alert] { color: #ff8a80; overflow-wrap: anywhere; }
        .tilo-shell-ways { display: flex; flex-wrap: wrap; justify-content: center; gap: 8px; width: 100%; }
        .tilo-shell-ways button { position: relative; display: inline-flex; align-items: center; justify-content: center; gap: 8px; box-sizing: border-box; flex: 1 1 0; min-width: 136px; min-height: 40px; margin: 0; padding: 8px 16px; font: inherit; font-size: 15px; font-weight: 500; line-height: 1.3; border: 0; border-radius: 8px; background: #26282e; color: #ececf0; cursor: pointer; -webkit-tap-highlight-color: transparent; touch-action: manipulation; }
        .tilo-shell-ways button:only-child { flex: 0 1 auto; }
        .tilo-shell-ways button::after { content: ''; position: absolute; inset: -3px 0; }
        @media (hover: hover) { .tilo-shell-ways button:hover { background: #30333a; } }
        .tilo-shell-ways button:active { background: #30333a; }
        .tilo-shell-ways button:focus-visible { outline: 2px solid #9ab4ff; outline-offset: 2px; }
        .tilo-shell-ways svg { flex: none; width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }
        .tilo-shell-way-in[data-place=failure] { margin: 24px 0 0; }
        .tilo-shell-way-in[data-place=alone] { position: fixed; z-index: 2; left: 50%; transform: translateX(-50%); width: min(400px, calc(100% - 48px)); margin: 0; }
      `;
      // The toolkit's own icons (`ui::icons`): a code, and a paste.
      const drawings = {
        scan: [['rect', {x: 3, y: 3, width: 5, height: 5, rx: 1}], ['rect', {x: 16, y: 3, width: 5, height: 5, rx: 1}],
          ['rect', {x: 3, y: 16, width: 5, height: 5, rx: 1}],
          ['path', {d: 'M21 16h-3a2 2 0 0 0-2 2v3M21 21v.01M12 7v3a2 2 0 0 1-2 2H7M3 12h.01M12 3h.01M12 16v.01M16 12h1M21 12v.01M12 21v-1'}]],
        paste: [['path', {d: 'M11 14h10M16 4h2a2 2 0 0 1 2 2v1.344M17 18l4-4-4-4M8 4H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 1.793-1.113'}],
          ['rect', {x: 8, y: 2, width: 8, height: 4, rx: 1}]],
      };
      const drawn = shapes => {
        const space = 'http://www.w3.org/2000/svg';
        const picture = document.createElementNS(space, 'svg');
        picture.setAttribute('viewBox', '0 0 24 24');
        picture.setAttribute('aria-hidden', 'true');
        for (const [kind, measures] of shapes) {
          const shape = document.createElementNS(space, kind);
          for (const [name, value] of Object.entries(measures)) shape.setAttribute(name, value);
          picture.append(shape);
        }
        return picture;
      };
      let group = null;
      let problem = null;
      const build = () => {
        const style = document.createElement('style');
        style.textContent = look;
        (document.head ?? root).append(style);
        group = document.createElement('div');
        group.className = 'tilo-shell-way-in';
        group.dataset.wayIn = 'shell';
        const said = document.createElement('p');
        // Another control's name, which stands in quotes, is never broken over two lines.
        said.textContent = offer.said.replace(/“[^”]*”/g, name => name.replace(/ /g, '\u00a0'));
        const row = document.createElement('div');
        row.className = 'tilo-shell-ways';
        problem = document.createElement('p');
        problem.setAttribute('role', 'alert');
        for (const {way, label} of offer.ways) {
          const press = document.createElement('button');
          press.type = 'button';
          press.dataset.way = way;
          if (drawings[way]) press.append(drawn(drawings[way]));
          press.append(label);
          press.addEventListener('click', () => {
            problem.textContent = '';
            wayIn(way);
          });
          row.append(press);
        }
        group.append(row, said, problem);
      };
      // Under the status line, on a page whose entry page is that line alone.
      const under = () => {
        if (!group?.isConnected || group.dataset.place !== 'alone') return;
        const line = document.getElementById('status');
        const foot = line?.getClientRects().length ? line.getBoundingClientRect().bottom : innerHeight / 2;
        group.style.top = `${Math.round(foot + 20)}px`;
      };
      let watched = false;
      const place = () => {
        const closed = root.dataset.entry === 'refused' && !root.dataset.pairingCode;
        if (!closed || !document.body || document.querySelector('[data-way-in]:not([data-way-in="shell"])')) {
          group?.remove();
          return;
        }
        if (!group) build();
        const others = 'refusal' in root.dataset && document.querySelector('#arrival-refusal .refusal-alt');
        const actions = 'failure' in root.dataset && document.querySelector('#arrival-failure .failure-actions');
        if (others) {
          group.dataset.place = 'refusal';
          if (others.firstElementChild !== group) others.prepend(group);
        } else if (actions) {
          group.dataset.place = 'failure';
          if (actions.nextElementSibling !== group) actions.after(group);
        } else {
          group.dataset.place = 'alone';
          if (group.parentNode !== document.body) document.body.append(group);
          const line = document.getElementById('status');
          if (line && !watched && typeof ResizeObserver === 'function') {
            watched = true;
            new ResizeObserver(under).observe(line);
          }
        }
        group.style.top = '';
        under();
      };
      if (root && ways.length) {
        new MutationObserver(place).observe(root, {attributes: true,
          attributeFilter: ['data-entry', 'data-pairing-code', 'data-refusal', 'data-failure']});
        addEventListener('resize', under);
      }

      Object.defineProperty(globalThis, '__tiloShell', {value: Object.freeze({
        permission(state) {
          if (!['granted', 'denied', 'default'].includes(state)) return;
          permission = state;
          if (state !== 'default') for (const resolve of asking.splice(0)) resolve(state);
        },
        chosen(id, action, reply) {
          const notice = shown.get(id);
          if (!notice) return false;
          const click = new Event('click', {cancelable: true});
          click.action = typeof action === 'string' ? action : '';
          click.reply = typeof reply === 'string' ? reply : null;
          notice.dispatchEvent(click);
          return true;
        },
        closed(id) {
          const notice = shown.get(id);
          if (!notice) return;
          shown.delete(id);
          notice.dispatchEvent(new Event('close'));
        },
        push(address) {
          if (typeof address !== 'string' || !address) return false;
          push = address;
          for (const hear of pushHeard.splice(0)) { try { hear(address); } catch { /* the page is gone */ } }
          return true;
        },
        hearingPush(hear) {
          if (typeof hear !== 'function') return false;
          if (push) { try { hear(push); } catch { /* the page is gone */ } } else pushHeard.push(hear);
          return true;
        },
        told(name, action, reply) {
          if (typeof name !== 'string' || !name) return false;
          told = {name, action: typeof action === 'string' ? action : null,
                  reply: typeof reply === 'string' ? reply : null};
          for (const hear of toldHeard.splice(0)) { try { hear(told); } catch { /* the page is gone */ } }
          return true;
        },
        hearingTold(hear) {
          if (typeof hear !== 'function') return false;
          if (told) { try { hear(told); } catch { /* the page is gone */ } } else toldHeard.push(hear);
          return true;
        },
        signedOut() {
          if (out) return true;
          out = true;
          for (const hear of outHeard.splice(0)) { try { hear(); } catch { /* the page is gone */ } }
          return true;
        },
        hearingSignedOut(hear) {
          if (typeof hear !== 'function') return false;
          if (out) { try { hear(); } catch { /* the page is gone */ } } else outHeard.push(hear);
          return true;
        },
        compose,
        ways,
        wayIn,
        refused(words) {
          if (!group?.isConnected || typeof words !== 'string') return false;
          problem.textContent = cut(words, 512);
          group.scrollIntoView({block: 'nearest'});
          return true;
        },
      })});
    })();
    """#
}
