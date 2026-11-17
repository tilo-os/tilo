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

/// Where the page stands at its site's door, in the page's own word for it
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

/// What the page tells the shell. Everything else a page posts is dropped.
public enum PageMessage: Equatable, Sendable {
    case notify(PageNotice)
    case entry(PageEntry)
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
/// `closed(id)` and `compose(words)`. It also tells the shell where the page stands at its
/// site's door, which the page writes on its root element.
public enum Bridge {
    /// The message handler the page's side posts to.
    public static let name = "tiloShell"

    public static func script(origin: String, permission: NoticePermission) -> String {
        source
            .replacingOccurrences(of: "%ORIGIN%", with: literal(origin))
            .replacingOccurrences(of: "%PERMISSION%", with: literal(permission.rawValue))
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
        if (takesWords(el) && put(el, words)) { waiting = null; return 'inserted'; }
        waiting = words;
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

      // Where the page stands at its site's door, for the shell's own way in.
      const root = document.documentElement;
      if (root) {
        const door = () => tell('entry', {state: cut(root.dataset.entry, 32), pairing: Boolean(root.dataset.pairingCode)});
        new MutationObserver(door).observe(root, {attributes: true, attributeFilter: ['data-entry', 'data-pairing-code']});
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
        compose,
      })});
    })();
    """#
}
