import Foundation

/// The script that joins Tilo's page to the engine. It runs in the page's main frame at the
/// start of every document and does nothing on another origin, so no other site's content
/// reaches the engine through it.
///
/// It tells the engine where the page stands on its site's entry page and when what the page shows
/// changed, and it hands the engine the page's own mount: the actions the page publishes and
/// the one entry that invokes them, which is the entry the palette, agents and assistive
/// technology use. It holds no key and reads none: the device's key stays where the page
/// keeps it.
public enum EngineScript {
    /// The name the page posts to: `webkit.messageHandlers.tiloLive`.
    public static let name = "tiloLive"

    /// Whether what a mount beside the page says it opened is among the routes the page names,
    /// one line `route APP/DOCUMENT: STATE;` each. It is text of the script: a function of the
    /// app, what the mount opened and the page's lines. An app's own document of the workspace
    /// is what a mount with no document named opens. Such a mount says its record is
    /// `@tilo:package-primary`, a name that never travels: the page's line for it holds no
    /// document at all (`route so.tilo.chat/: ready;`). A mount that names nothing it opened
    /// has nothing to wait for.
    static let joined = #"""
    (app, opened, routes) => typeof opened !== 'string'
            || String(routes).includes(`${app}/${opened === '@tilo:package-primary' ? '' : opened}:`)
    """#

    /// What a program built for a simulator, and no other, adds to the script. A simulator of a
    /// phone offers WebKit no graphics adapter, and Tilo's page, which draws with one, stops
    /// there before it mounts anything. So such a program says that it has none to offer
    /// (`html: true` on what it hands the page before the page starts), and the page shows its
    /// one app as HTML; a thing mounted beside the page is asked for as HTML too (`dom: true`). The
    /// app's way in, a pairing and a conversation can then be driven in a simulator. What
    /// stands there is HTML: how a phone draws, and its keyboard over what it draws, is seen on
    /// a phone alone.
    ///
    /// The compiler's own condition decides, and nothing a program is told as it starts or
    /// while it lives. In a program for a phone or a Mac both are empty: neither word is in
    /// it, and its script reads as it does without them. Each says in the script why it is
    /// there, which also makes it long enough to stand in a program as text: whoever looks
    /// through a built program for these sentences finds them in a simulator's alone.
    #if targetEnvironment(simulator)
    static let simulator = (says: " html: true, /* a simulator offers the page no graphics adapter */",
                            mounts: "dom: true, /* shown as HTML in a simulator */ ")
    #else
    static let simulator = (says: "", mounts: "")
    #endif

    /// - Parameters:
    ///   - origin: the one site whose page may speak to the engine.
    ///   - unseen: the page is shown to nobody as it starts. A web view that is in no window
    ///     calls its page hidden and never asks it for a frame, and Tilo's page is in only once
    ///     it drew one. So an unseen page's frames are driven by a clock, slowly: it draws for
    ///     nobody, only to go on living. `__tiloLive.seen(true)` hands the frames back to the
    ///     screen while a person looks at the view.
    ///   - pass: what the script takes an ask of the person's own providers with: the engine
    ///     made it and hands it in with every such ask. Without one no such ask is taken.
    ///   - providers: the providers of the person's own the script asks for the engine; an ask
    ///     of any other is refused.
    ///   - device: what this device is called where its person lets it in and lists their
    ///     devices. Empty leaves the name to the page, which says the browser and the system.
    public static func source(origin: String, unseen: Bool, pass: String = "", providers: [String] = [],
                              device: String = "") -> String {
        let listed = (try? JSONSerialization.data(withJSONObject: providers)).flatMap { String(data: $0, encoding: .utf8) } ?? "[]"
        return source
            .replacingOccurrences(of: "%JOINED%", with: joined)
            .replacingOccurrences(of: "%ORIGIN%", with: literal(origin))
            .replacingOccurrences(of: "%NAME%", with: name)
            .replacingOccurrences(of: "%UNSEEN%", with: unseen ? "true" : "false")
            .replacingOccurrences(of: "%PASS%", with: literal(pass))
            .replacingOccurrences(of: "%PROVIDERS%", with: listed)
            .replacingOccurrences(of: "%SIMULATOR_SAYS%", with: simulator.says)
            .replacingOccurrences(of: "%SIMULATOR_MOUNTS%", with: simulator.mounts)
            // Last, so that nothing a name holds is read as one of the places above.
            .replacingOccurrences(of: "%DEVICE%", with: literal(String(device.prefix(64))))
    }

    static func literal(_ text: String) -> String {
        let data = try? JSONSerialization.data(withJSONObject: text, options: [.fragmentsAllowed])
        return data.flatMap { String(data: $0, encoding: .utf8) } ?? "\"\""
    }

    private static let source = #"""
    (() => {
      'use strict';
      if (location.origin !== %ORIGIN% || window.top !== window || globalThis.__tiloLive) return;
      const post = message => {
        try { webkit.messageHandlers.%NAME%.postMessage(message); } catch { /* no engine listens */ }
      };

      // Frames. While nobody sees the page a clock asks for them, twenty times a second at
      // most; while someone does, the screen asks, as in any browser. The page is never told
      // it is hidden: it is the person's device whether or not anyone looks at it.
      const screen = {request: window.requestAnimationFrame.bind(window), cancel: window.cancelAnimationFrame.bind(window)};
      let unseen = %UNSEEN%;
      const frames = new Map();
      let lastFrame = 0;
      const ask = (id, callback) => {
        const run = time => { frames.delete(id); callback(typeof time === 'number' ? time : performance.now()); };
        frames.set(id, unseen ? {callback, timer: setTimeout(run, 50)} : {callback, frame: screen.request(run)});
      };
      window.requestAnimationFrame = callback => {
        const id = ++lastFrame;
        ask(id, callback);
        return id;
      };
      window.cancelAnimationFrame = id => {
        const held = frames.get(id);
        if (!held) return;
        frames.delete(id);
        if (held.timer !== undefined) clearTimeout(held.timer); else screen.cancel(held.frame);
      };
      const seen = now => {
        if (unseen === !now) return;
        unseen = !now;
        // A frame asked of a screen that no longer shows the page would never come.
        if (unseen) for (const [id, held] of [...frames]) if (held.frame !== undefined) { screen.cancel(held.frame); ask(id, held.callback); }
      };
      Object.defineProperty(document, 'visibilityState', {get: () => 'visible', configurable: true});
      Object.defineProperty(document, 'hidden', {get: () => false, configurable: true});

      // Where the page stands on its site's entry page, said once per change: the page writes it on
      // its root element (`data-entry`, `data-arrival`, `data-pairing-code`) and in its status line.
      const root = document.documentElement;
      let mount;
      let device;
      let said = '';
      // What the page asks before it pairs a device (web/entry-page.mjs, `pairingAgree`): a card
      // of its own that says whose device this becomes, which server adds it and what follows,
      // with its two ways on. The app shows its person these sentences; nothing here answers.
      const whoseCard = () => {
        const card = document.querySelector('.tilo-entry-card[data-card="pair-whose"]');
        const buttons = card ? [...card.querySelectorAll('button')] : [];
        const go = buttons.find(button => button.classList.contains('tilo-entry-primary'));
        const stop = buttons.find(button => button !== go);
        return go && stop ? {card, go, stop} : null;
      };
      const asking = () => {
        const found = whoseCard();
        if (!found) return null;
        const words = element => (element?.textContent ?? '').trim().slice(0, 600);
        return {heading: words(found.card.querySelector('h2')), go: words(found.go), stop: words(found.stop),
          said: [...found.card.querySelectorAll('p')].map(words).filter(Boolean).slice(0, 8)};
      };
      const state = () => {
        const data = root.dataset;
        const now = {
          kind: 'state', entry: data.entry ?? '', arrival: data.arrival ?? '', code: data.pairingCode ?? '',
          // Why the page stopped, as the page itself names the case: its failure screen's
          // (`data-failure`, web/failure-screen.mjs), and that its server turned its open
          // session away (`data-refusal`, web/session-refusal.mjs). Names of cases, never words.
          failure: (data.failure ?? '').slice(0, 64), refusal: (data.refusal ?? '').slice(0, 64),
          // How the page's connection to its server stands, as the page itself names it
          // (`data-connection`, web/connection-status.mjs): `offline` once it is cut. A name
          // of a case, never words.
          connection: (data.connection ?? '').slice(0, 64),
          // A page that cannot draw empties its status line and says why in a card.
          status: ((document.getElementById('status')?.textContent ?? '')
            || (data.entry === 'unsupported' ? document.querySelector('.tilo-entry-card[data-card="renderer"] p')?.textContent ?? '' : '')).slice(0, 400),
          // What the page's failure screen offers to read or copy under its sentence
          // (`#arrival-failure-text`, web/failure-screen.mjs): what the page makes of the stop,
          // the server, the step, the error and the page's build, never a token, key or
          // certificate. Said only while the page shows them.
          details: (data.failure && !document.getElementById('arrival-failure-details')?.hidden
            ? document.getElementById('arrival-failure-text')?.textContent ?? '' : '').slice(0, 2000),
          route: location.pathname + location.hash.split('?')[0], mounted: Boolean(mount || device?.workspaceRecord), hosted: !mount && Boolean(device?.workspaceRecord), ask: asking(),
        };
        const text = JSON.stringify(now);
        if (text === said) return;
        said = text;
        post(now);
      };
      new MutationObserver(state).observe(root,
        {attributes: true, attributeFilter: ['data-entry', 'data-arrival', 'data-pairing-code', 'data-failure', 'data-refusal',
          'data-connection']});
      // Which look the page draws in, light or dark: the theme its person has in Tilo, as the
      // page itself says it on its root element whenever a mount takes a theme (`data-look`,
      // page_theme.rs). The name of one of two cases, never words: a page that says neither
      // is not heard, and a look is said once per change.
      let looked = '';
      const look = () => {
        const now = root.dataset.look ?? '';
        if (now === looked || (now !== 'dark' && now !== 'light')) return;
        looked = now;
        post({kind: 'look', dark: now === 'dark'});
      };
      new MutationObserver(look).observe(root, {attributes: true, attributeFilter: ['data-look']});
      look();
      const ready = () => {
        const status = document.getElementById('status');
        if (status) new MutationObserver(state).observe(status, {childList: true, characterData: true, subtree: true});
        // The page's cards are children of its body: the one that says it cannot draw may come
        // a moment after the page said that it stopped.
        if (document.body) new MutationObserver(state).observe(document.body, {childList: true});
        state();
      };
      if (document.readyState === 'loading') addEventListener('DOMContentLoaded', ready, {once: true}); else ready();
      addEventListener('hashchange', state);

      // What the page shows changed: its own description of itself for assistive technology
      // did. Said at once, by the page's own message to the engine and by no timer of the page:
      // a page nobody sees gets its timers late, by up to a second, and an answer that is being
      // written is shown piece by piece. The engine takes the words that come while its
      // listener reads as one word.
      const changed = () => post({kind: 'changed'});
      // Tilo's page says `tiloChanges` at every frame it draws and at every word of its
      // session: many times a second while nothing happens. What its device heard and which
      // documents it holds is what `tiloChannels` reads as, so only when that moved is the
      // word handed on, and then at once. What arrives in a document the device holds already
      // does not always move it, so while the app expects a change (it just changed something
      // itself, or an agent is at work on what it shows) every word is handed on.
      let heard = '';
      let expecting = 0;
      const expect = ms => { expecting = Math.max(expecting, Date.now() + Math.min(Math.max(Number(ms) || 0, 0), 60000)); };
      const perhaps = () => {
        if (Date.now() < expecting) { changed(); return; }
        let now = heard;
        try { now = String(globalThis.tiloChannels()); } catch { /* said at the page's next word */ }
        if (now === heard) return;
        heard = now;
        changed();
      };
      let watching;
      let listening;
      const watch = () => {
        watching?.disconnect();
        watching = undefined;
        listening?.();
        listening = undefined;
        // A page that says what it published is listened to (`tiloChanges`, web/changes.mjs).
        // One that does not yet is watched where it describes itself.
        const weighs = typeof globalThis.tiloChannels === 'function';
        // What the device has heard so far is where it starts from: the first read is asked for below.
        if (weighs) { try { heard = String(globalThis.tiloChannels()); } catch { /* weighed at the page's first word */ } }
        const told = globalThis.tiloChanges?.listen?.(weighs ? perhaps : changed);
        if (typeof told === 'function') listening = told;
        if (mount?.semantics) {
          watching = new MutationObserver(changed);
          watching.observe(mount.semantics, {subtree: true, childList: true, characterData: true, attributes: true});
        }
        state();
        changed();
      };
      // The page names its mount `tiloMount` once it is in. Hearing the name being given
      // needs no clock.
      Object.defineProperty(globalThis, 'tiloMount', {
        configurable: true, enumerable: true,
        get: () => mount,
        set: next => { mount = next; queueMicrotask(watch); },
      });

      // A refusal reads `code: reason`.
      const failure = error => {
        const text = String(error?.message ?? error);
        const at = text.indexOf(': ');
        const code = typeof error?.code === 'string' ? error.code : at > 0 ? text.slice(0, at) : 'failed';
        return {code, reason: at > 0 && code === text.slice(0, at) ? text.slice(at + 2) : text};
      };

      // Live things: one more mount each in this same document, through the page's own peer,
      // so each of them is this same device and none is a pairing of its own. Each stands in
      // an element that fills the view, over the page; the one in front is the one shown.
      const lives = new Map();
      let instance = 100;
      const place = id => {
        const element = document.createElement('div');
        element.dataset.tiloLive = id;
        element.style.cssText = 'position:fixed;inset:0;z-index:50;background:#121214;display:none';
        document.body.append(element);
        return element;
      };
      // Whether what a mount beside the page opened is among what the page itself holds
      // (`EngineScript.joined`).
      const joined = %JOINED%;
      // A page that offers `tiloOpen(element, {app, record, newDocument, instance})` mounts
      // beside itself. One that does not yet is reached through its own modules: the mount
      // the page itself calls, told the page's own profile, device, workspace and peer.
      const beside = async (element, what) => {
        if (typeof globalThis.tiloOpen === 'function') return globalThis.tiloOpen(element, what);
        const hint = document.querySelector('link[rel="modulepreload"][href]')?.href;
        const scope = globalThis.tiloRouter?.route?.target?.scope;
        const kept = Object.keys(sessionStorage).find(name => name.startsWith('tilo-device-slot:'));
        if (!hint || scope?.kind !== 'workspace' || !kept) throw new Error('not-open: The page holds no workspace to show this in');
        if (Number(sessionStorage.getItem(kept)) !== 0) throw new Error('not-open: The page is not the first of its browser');
        const base = new URL('.', hint).href;
        const [host, peers] = await Promise.all([import(new URL('host.mjs', base).href), import(new URL('peer.mjs', base).href)]);
        const profile = kept.slice('tilo-device-slot:'.length);
        const device = peers.profileDevice(profile, localStorage, 0);
        let last;
        // The page's peer is the first its runtime started, unless it started another first.
        for (const runtimeId of [1, 2, 3]) {
          try {
            const made = await host.mountDesktop(element, {...what, %SIMULATOR_MOUNTS%profile, device, workspace: scope.workspace,
              peer: {runtimeId, origin: scope.host, workspace: scope.workspace}});
            const record = made.state()?.record;
            const routes = globalThis.tiloChannels?.() ?? '';
            if (made.state()?.phase !== 'failed' && joined(what.app, record, routes)) return made;
            last = new Error(`failed: ${made.state()?.error ?? 'The record did not join the workspace'}`);
            await made.cleanup().catch(() => {});
          } catch (error) { last = error; }
        }
        throw last;
      };
      const front = id => {
        for (const [name, live] of lives) live.element.style.display = name === id ? 'block' : 'none';
      };
      const live = id => (id ? lives.get(id)?.mount : mount);
      // The size a live thing's content asks for, as its mount's status says it (`content`):
      // handed on when it is first known and when it changes. The app gives its place that
      // height.
      const sizes = new Map();
      const sized = (id, status) => {
        const asks = status?.content;
        if (!asks || !Number.isFinite(asks.width) || !Number.isFinite(asks.height)) return;
        const now = `${Math.ceil(asks.width)}x${Math.ceil(asks.height)}`;
        if (sizes.get(id) === now) return;
        sizes.set(id, now);
        post({kind: 'live', id, width: Math.ceil(asks.width), height: Math.ceil(asks.height)});
      };
      // Chat shows the view one message carries, alone: the view `view` of the message's own
      // item, opened by the action the item offers for it. The message is named and never its
      // view's text, so whose code the view is and what it reads stays Chat's to say.
      const alone = async (shown, view) => {
        const message = String(view?.message ?? '');
        if (!/^[A-Za-z0-9._:-]{1,128}$/.test(message)) throw new Error('unknown_action: no such message');
        const item = `/messages/${message}`;
        for (let tries = 0; ; tries++) {
          let listed = [];
          try { listed = shown.actions() ?? []; } catch { /* the mount is still coming in */ }
          const open = listed.find(action => action.view?.id === 'view' && action.view.open_action === action.address?.action
            && String(action.address?.resource ?? '').endsWith(item));
          if (open) { await shown.invokeAction(open.address.resource, open.address.action, {}, {confirm: false}); return; }
          // A message further back is offered once it is addressed.
          if (tries >= 12) { await shown.invokeAction(`chat.workspace${item}`, 'show-view', {}, {confirm: false}); return; }
          await new Promise(done => setTimeout(done, 250));
        }
      };

      // The device's own entry. A page that knows a native app may hold it hands this over once
      // it is in (web/native-host.mjs): its asks of the workspace's host as the person on this
      // device. It is kept in this script's own scope and on no global, and only the first of
      // a document is taken, so nothing that runs later puts another in its place.
      let pendingDevice;
      let approvedDevice;
      Object.defineProperty(globalThis, 'tiloNativeHost', {value: Object.freeze({
        // Keep this admitted page running as the app's certified device.
        keepsDevice: true,
        // What this device is called where its person lets it in: the app's name for it.
        name: %DEVICE%,%SIMULATOR_SAYS%
        confirmDevice(nonce) {
          if (device || pendingDevice || typeof nonce !== 'string') return Promise.resolve(false);
          return new Promise(resolve => {
            const timer = setTimeout(() => { pendingDevice = undefined; resolve(false); }, 3000);
            pendingDevice = {nonce, confirm() { clearTimeout(timer); approvedDevice = nonce; pendingDevice = undefined; resolve(true); }};
          });
        },
        device(entry, nonce) {
          if (device || typeof entry?.invoke !== 'function') return false;
          if (!approvedDevice || approvedDevice !== nonce) return false;
          approvedDevice = undefined;
          device = entry;
          if (entry.workspaceRecord && !mount) queueMicrotask(watch);
          return true;
        },
      })});
      // The pass is this engine's own: the engine made it and hands it in with every ask of
      // the person's providers. It stands here, in no function a page's script could print.
      const pass = %PASS%;
      const providers = new Set(%PROVIDERS%);
      const agentActions = new Set(['read', 'ensure-personal-assistant', 'stop-sub-agent']);
      // An app's own document of the workspace, as the workspace's server names what it offers
      // of one: the app and nothing after it. Under any longer name the server offers nothing of it.
      const ownDocument = app => device.workspaceRecord ? device.workspaceRecord(app) : `record:${app}:`;
      // Every row comes from the Chat record the host opened for this device.
      // A named resource is resolved again by the host immediately before it runs.
      async function chatRows(query) {
        const provider = await ownDocument('so.tilo.chat');
        return (await device.catalogue({provider, ...query})).rows ?? [];
      }
      // The chain the workspace's tracker keeps its issues in (Tasks' `tasks.issues`), as every
      // device of the workspace names it, and what is asked of it for the app.
      const issues = 'chain:derived:issues@derived:default@tab:0000000000000000';
      const issueActions = new Set(['read', 'create']);
      const holds = given => {
        if (typeof given !== 'string' || !pass || given.length !== pass.length) return false;
        let differs = 0;
        for (let at = 0; at < pass.length; at++) differs |= given.charCodeAt(at) ^ pass.charCodeAt(at);
        return differs === 0;
      };
      // What crosses to the app: never the protected value that went in, wherever an answer
      // carries it, and no text under a name that only ever holds a secret.
      const secretNames = new Set(['secret', 'password', 'passphrase', 'apikey', 'api_key', 'privatekey', 'private_key',
        'token', 'accesstoken', 'access_token', 'refreshtoken', 'refresh_token']);
      const without = (value, hidden) => {
        if (typeof value === 'string') return hidden && value.includes(hidden) ? value.split(hidden).join('') : value;
        if (Array.isArray(value)) return value.map(item => without(item, hidden));
        if (!value || typeof value !== 'object') return value;
        const kept = {};
        for (const [name, field] of Object.entries(value)) {
          if (typeof field === 'string' && secretNames.has(name.toLowerCase())) continue;
          kept[name] = without(field, hidden);
        }
        return kept;
      };
      const unknown = (provider, action) =>
        JSON.stringify({error: {code: 'unknown_action', reason: `no action addressed by (${provider}, ${action})`}});
      const noDevice = () =>
        JSON.stringify({error: {code: 'unavailable', reason: 'This page hands a native app no way to its person\'s own providers'}});
      const inScope = scope => device && scope && scope.person === device.person && scope.workspace === device.workspace;
      const changedDevice = () => JSON.stringify({error: {code: 'wrong_device', reason: 'Your account or workspace changed'}});

      // The steps of putting one file into the workspace this page is in (`put` below).
      const putSteps = new Set(['offered', 'begin', 'append', 'finish']);

      Object.defineProperty(globalThis, '__tiloLive', {value: Object.freeze({
        seen,
        confirmDevice(given, nonce) {
          if (holds(given) && pendingDevice?.nonce === nonce) pendingDevice.confirm();
        },
        // The app expects what the page holds to change within `ms`.
        expect(ms) { expect(ms); return true; },
        // One of the person's own providers on their host, asked for the app's own code alone:
        // with this engine's pass, and of the providers the engine names. `hidden` is a field
        // of the argument and what the person typed into a protected field for it.
        async person(given, provider, resource, action, argument, hidden, scope) {
          if (!holds(given) || !providers.has(provider)) return unknown(provider, action);
          if (!device) return noDevice();
          if (scope && !inScope(scope)) return changedDevice();
          const secret = hidden && typeof hidden.value === 'string' ? hidden.value : '';
          try {
            const plain = argument && Object.keys(argument).length ? argument : null;
            const answer = await device.invoke({provider, address: {resource, action}},
              hidden ? {...(argument ?? {}), [hidden.field]: secret} : plain);
            return JSON.stringify({ok: without({state: answer?.state ?? null, value: answer?.output?.value ?? null,
              error: answer?.error ?? null}, secret)});
          } catch (error) { return JSON.stringify({error: without(failure(error), secret)}); }
        },
        async personActions(given, provider, scope) {
          if (!holds(given) || !providers.has(provider)) return unknown(provider, 'catalogue');
          if (!inScope(scope)) return changedDevice();
          try {
            const {rows} = await device.catalogue({provider, max_results: 64});
            return JSON.stringify({ok: rows.filter(row => row.provider === provider).map(({action}) => ({
              resource: action.address.resource, action: action.address.action, params: (action.params ?? []).map(p => p.name),
            }))});
          } catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        // Whose device this is and the workspace it stands in, as the page's session says it.
        whose(given) {
          if (!holds(given)) return unknown('tilo.device', 'whose');
          if (!device) return noDevice();
          return JSON.stringify({ok: {person: device.person ?? null, workspace: device.workspace ?? null}});
        },
        // The person's own workspace on this host, which the host makes when it is first asked for.
        async own(given) {
          if (!holds(given)) return unknown('tilo.device', 'own');
          if (!device) return noDevice();
          try {
            const own = await device.ownWorkspace();
            return JSON.stringify({ok: {id: own?.id ?? null, name: own?.name ?? null, origin: own?.origin ?? null}});
          } catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        // Which of the documents the app follows changed in the workspace, as the entry of this
        // device says it (its `changed`): their names and where to ask on from,
        // never what one of them holds. The page holds the ask, ten seconds at most.
        async followed(given, of, after, waitMs) {
          if (!holds(given)) return unknown('tilo.device', 'changed');
          if (!device) return noDevice();
          if (typeof device.changed !== 'function') return unknown('tilo.device', 'changed');
          try {
            const told = await device.changed({of, after: typeof after === 'string' && after ? after : null, waitMs: Number(waitMs) || 0});
            const named = (Array.isArray(told?.changed) ? told.changed : []).slice(0, 256)
              .filter(one => typeof one?.app === 'string' && typeof one?.document === 'string')
              .map(one => ({app: one.app, document: one.document}));
            return JSON.stringify({ok: {changed: named, cursor: typeof told?.cursor === 'string' ? told.cursor : ''}});
          } catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        // One of the person's files goes into the workspace this page is in, a piece at a
        // time, through what the page hands a native app for it (`put`): `begin` with the
        // file's size, `append` with a piece as base64 text at the offset the piece before it
        // ended, `finish` for what the workspace keeps the file under. `offered` asks nothing
        // of anyone: it says whether this page hands such a way at all. Only the app's own
        // code reaches this, with the engine's pass. What is answered is the page's answer to
        // the step and nothing of a piece; nothing of one is posted to the engine or kept.
        async put(given, step, of) {
          if (!holds(given) || !putSteps.has(step)) return unknown('tilo.content', 'put');
          if (!device) return noDevice();
          const put = device.put;
          if (!put || ![put.begin, put.append, put.finish].every(one => typeof one === 'function')) return unknown('tilo.content', 'put');
          try {
            if (step === 'offered') return JSON.stringify({ok: true});
            if (step === 'begin') return JSON.stringify({ok: {upload: (await put.begin(of?.size))?.upload ?? null}});
            if (step === 'append') return JSON.stringify({ok: {written: (await put.append(of?.upload, of?.offset, of?.piece))?.written ?? null}});
            return JSON.stringify({ok: {content: (await put.finish(of?.upload))?.content ?? null}});
          } catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        // A file a message carries is fetched by a link: an address on this page's own site
        // that serves the content for a short time, which the page hands a native app for it
        // (`link`). With nothing named it asks nothing of anyone: it says whether this page
        // hands such a way at all. Only the app's own code reaches this, with the engine's
        // pass. The link is the answer and nothing else here: it is posted to nobody, kept
        // nowhere and said to no log.
        async link(given, of) {
          if (!holds(given)) return unknown('tilo.content', 'link');
          if (!device) return noDevice();
          if (typeof device.link !== 'function') return unknown('tilo.content', 'link');
          if (of === null || of === undefined) return JSON.stringify({ok: true});
          try {
            const link = await device.link(String(of.content ?? ''), String(of.name ?? ''));
            return JSON.stringify({ok: {path: String(link?.path ?? ''), expiresInSecs: Number(link?.expiresInSecs) || 0}});
          } catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        // The person's answer to what the page asks before it pairs, handed on as a press of
        // the page's own button. Only the app's own code reaches this, with the engine's
        // pass, and the app calls it only from its person's press.
        pairing(given, goOn) {
          const found = holds(given) ? whoseCard() : null;
          if (!found) return false;
          (goOn === true ? found.go : found.stop).click();
          return true;
        },
        // The agents the workspace keeps (Assistant's `tilo.workspace.agent`), asked of the
        // workspace's server as the person on this device: their read, one agent's read, the
        // action that sets up a person's assistant, and the one that stops a sub-agent. Nothing else of Assistant's
        // document is asked this way.
        async agents(given, action, argument, item) {
          const named = item === null || item === undefined || (typeof item === 'string' && /^[A-Za-z0-9._-]{1,128}$/.test(item));
          if (!holds(given) || !agentActions.has(action) || !named || (item && action !== 'read')) return unknown('tilo.workspace.agent', action);
          if (!device) return noDevice();
          try {
            const provider = await ownDocument('so.tilo.assistant');
            const answer = await device.invoke({provider, address: {resource: item ? `tilo.workspace.agent/agents/${item}` : 'tilo.workspace.agent', action}},
              argument && Object.keys(argument).length ? argument : null);
            return JSON.stringify({ok: without({state: answer?.state ?? null, value: answer?.output?.value ?? null,
              error: answer?.error ?? null}, '')});
          } catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        // The tasks of the person's assistant are issues of the workspace's tracker, which this
        // page does not hold: it shows Chat. They are asked of the workspace's server as the
        // person on this device: the read of the tracker's issues and the action that files one,
        // and nothing else of Tasks.
        async tasks(given, action, argument, invocation) {
          if (!holds(given) || !issueActions.has(action)) return unknown('tasks.issues', action);
          if (!device) return noDevice();
          try {
            const answer = await device.invoke({provider: await ownDocument('so.tilo.tasks'), address: {resource: issues, action}},
              argument && Object.keys(argument).length ? argument : null, {invocation: invocation ?? undefined});
            return JSON.stringify({ok: without({state: answer?.state ?? null, value: answer?.output?.value ?? null,
              error: answer?.error ?? null}, '')});
          } catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        async actions(resource, id, given, kind) {
          if (!id && !mount && device?.workspaceRecord) {
            if (!holds(given)) return unknown('so.tilo.chat', 'list');
            try {
              const rows = await chatRows({parent_resource: resource ?? undefined, resource_name: kind ?? undefined});
              return JSON.stringify({ok: rows.flatMap(row => row.action ? [row.action] : [])});
            } catch (error) { return JSON.stringify({error: failure(error)}); }
          }
          const from = live(id);
          if (!from) return JSON.stringify({error: {code: 'not-open', reason: 'The page is not in'}});
          try { return JSON.stringify({ok: from.actions(resource ?? undefined)}); }
          catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        async invoke(resource, action, args, confirm, id, given, invocation) {
          if (!id && !mount && device?.workspaceRecord) {
            if (!holds(given)) return unknown(resource, action);
            try {
              const row = (await chatRows({target: {resource, action}})).find(row =>
                row.action?.address?.resource === resource && row.action.address.action === action);
              if (!row) return unknown(resource, action);
              const answer = await device.invoke({provider: row.provider, address: row.action.address}, args, {confirmation: confirm, invocation: invocation ?? undefined});
              if (answer.state !== 'succeeded') return JSON.stringify({error: {
                code: answer.error?.code ?? answer.state, reason: answer.error?.message ?? 'The workspace did not complete the action'}});
              if (action !== 'read') changed();
              return JSON.stringify({ok: answer});
            } catch (error) { return JSON.stringify({error: failure(error)}); }
          }
          const from = live(id);
          if (!from) return JSON.stringify({error: {code: 'not-open', reason: 'The page is not in'}});
          try {
            const answer = JSON.stringify({ok: await from.invokeAction(resource, action, args ?? {}, {confirm: Boolean(confirm)}) ?? null});
            // What the app itself changed is read again without waiting for anyone's word,
            // and what comes of it is expected.
            if (action !== 'read') { expect(15000); changed(); }
            return answer;
          } catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        // Mount `what` (`{app, record}`, `{app, newDocument: true}`, or Chat with the `view` of
        // one message, `{app, view: {message, conversation}}`) as the live thing `id`
        // and bring it to the front. One that is mounted already only comes to the front.
        async show(id, what) {
          if (!mount && device?.workspaceRecord) return JSON.stringify({error: {code: 'unknown_action',
            reason: 'This workspace page does not offer an embedded view of this resource'}});
          if (!mount) return JSON.stringify({error: {code: 'not-open', reason: 'The page is not in'}});
          try {
            if (!lives.has(id)) {
              const element = place(id);
              element.style.display = 'block';
              // A message's view stands in Chat's own document of the workspace: Chat is mounted
              // as the page itself opens it, with no document named.
              const {view, ...rest} = what;
              let made;
              try {
                made = await beside(element, {...rest, instance: instance++, onStatus: status => sized(id, status)});
                if (view) await alone(made, view);
                lives.set(id, {element, mount: made});
              } catch (error) {
                element.remove();
                sizes.delete(id);
                await made?.cleanup?.().catch(() => {});
                throw error;
              }
            }
            front(id);
            const shown = lives.get(id).mount;
            shown.editor?.focus?.();
            return JSON.stringify({ok: {record: shown.state()?.record ?? null, kept: lives.size}});
          } catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        // Nothing is in front: the view shows Chat's own page, which nobody is shown.
        rest() { front(null); return true; },
        // What a live thing draws now, for a check that has no eyes: its mount's own count of
        // frames, the size its content asks for and the names its semantics hold. No picture:
        // a canvas read out here gives a frame it drew before, not what is shown.
        async drawn(id) {
          const held = lives.get(id);
          if (!held) return JSON.stringify({error: {code: 'not-open', reason: 'No such live thing is mounted'}});
          try {
            try { held.mount.requestSemantics?.(); } catch { /* not offered */ }
            await new Promise(done => setTimeout(done, 400));
            const state = held.mount.state?.() ?? {};
            const words = [];
            const node = n => { if (!n) return; if (n.name) words.push(String(n.name)); if (n.value) words.push(String(n.value)); (n.children || []).forEach(node); };
            const walk = n => { if (!n) return; if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
            walk(state.semantics?.root);
            const canvas = held.element.querySelector('canvas');
            return JSON.stringify({ok: {phase: state.phase ?? null, frames: state.frames ?? null, size: state.logicalSize ?? null,
              content: state.content ?? null, words, canvas: canvas ? [canvas.width, canvas.height] : null}});
          } catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        async close(id) {
          const gone = lives.get(id);
          if (!gone) return false;
          lives.delete(id);
          sizes.delete(id);
          gone.element.remove();
          await gone.mount.cleanup?.().catch(() => {});
          return true;
        },
      })});
    })();
    """#
}
