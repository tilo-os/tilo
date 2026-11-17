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
    public static func source(origin: String, unseen: Bool, pass: String = "", providers: [String] = []) -> String {
        let listed = (try? JSONSerialization.data(withJSONObject: providers)).flatMap { String(data: $0, encoding: .utf8) } ?? "[]"
        return source
            .replacingOccurrences(of: "%ORIGIN%", with: literal(origin))
            .replacingOccurrences(of: "%NAME%", with: name)
            .replacingOccurrences(of: "%UNSEEN%", with: unseen ? "true" : "false")
            .replacingOccurrences(of: "%PASS%", with: literal(pass))
            .replacingOccurrences(of: "%PROVIDERS%", with: listed)
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
      let said = '';
      const state = () => {
        const data = root.dataset;
        const now = {
          kind: 'state', entry: data.entry ?? '', arrival: data.arrival ?? '', code: data.pairingCode ?? '',
          // A page that cannot draw empties its status line and says why in a card.
          status: ((document.getElementById('status')?.textContent ?? '')
            || (data.entry === 'unsupported' ? document.querySelector('.tilo-entry-card[data-card="renderer"] p')?.textContent ?? '' : '')).slice(0, 400),
          route: location.pathname + location.hash.split('?')[0], mounted: Boolean(mount),
        };
        const text = JSON.stringify(now);
        if (text === said) return;
        said = text;
        post(now);
      };
      new MutationObserver(state).observe(root,
        {attributes: true, attributeFilter: ['data-entry', 'data-arrival', 'data-pairing-code']});
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
      // did. Said at most once in forty milliseconds; the engine reads what it wants then.
      let pending = 0;
      const changed = () => {
        if (pending) return;
        pending = setTimeout(() => { pending = 0; post({kind: 'changed'}); }, 40);
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
        const told = globalThis.tiloChanges?.listen?.(changed);
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
            const made = await host.mountDesktop(element, {...what, profile, device, workspace: scope.workspace,
              peer: {runtimeId, origin: scope.host, workspace: scope.workspace}});
            const record = made.state()?.record;
            const routes = globalThis.tiloChannels?.() ?? '';
            if (made.state()?.phase !== 'failed' && (typeof record !== 'string' || routes.includes(`${what.app}/${record}:`))) return made;
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

      // The device's own entry. A page that knows a native app may hold it hands this over once
      // it is in (web/native-host.mjs): its asks of the workspace's host as the person on this
      // device. It is kept in this script's own scope and on no global, and only the first of
      // a document is taken, so nothing that runs later puts another in its place.
      let device;
      Object.defineProperty(globalThis, 'tiloNativeHost', {value: Object.freeze({
        device(entry) { if (!device && entry && typeof entry.invoke === 'function') device = entry; },
      })});
      // The pass is this engine's own: the engine made it and hands it in with every ask of
      // the person's providers. It stands here, in no function a page's script could print.
      const pass = %PASS%;
      const providers = new Set(%PROVIDERS%);
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

      Object.defineProperty(globalThis, '__tiloLive', {value: Object.freeze({
        seen,
        // One of the person's own providers on their host, asked for the app's own code alone:
        // with this engine's pass, and of the providers the engine names. `hidden` is a field
        // of the argument and what the person typed into a protected field for it.
        async person(given, provider, resource, action, argument, hidden) {
          if (!holds(given) || !providers.has(provider)) return unknown(provider, action);
          if (!device) return noDevice();
          const secret = hidden && typeof hidden.value === 'string' ? hidden.value : '';
          try {
            const plain = argument && Object.keys(argument).length ? argument : null;
            const answer = await device.invoke({provider, address: {resource, action}},
              hidden ? {...(argument ?? {}), [hidden.field]: secret} : plain);
            return JSON.stringify({ok: without({state: answer?.state ?? null, value: answer?.output?.value ?? null,
              error: answer?.error ?? null}, secret)});
          } catch (error) { return JSON.stringify({error: without(failure(error), secret)}); }
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
        actions(resource, id) {
          const from = live(id);
          if (!from) return JSON.stringify({error: {code: 'not-open', reason: 'The page is not in'}});
          try { return JSON.stringify({ok: from.actions(resource ?? undefined)}); }
          catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        async invoke(resource, action, args, confirm, id) {
          const from = live(id);
          if (!from) return JSON.stringify({error: {code: 'not-open', reason: 'The page is not in'}});
          try { return JSON.stringify({ok: await from.invokeAction(resource, action, args ?? {}, {confirm: Boolean(confirm)}) ?? null}); }
          catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        // Mount `what` (`{app, record}`, or `{app, newDocument: true}`) as the live thing `id`
        // and bring it to the front. One that is mounted already only comes to the front.
        async show(id, what) {
          if (!mount) return JSON.stringify({error: {code: 'not-open', reason: 'The page is not in'}});
          try {
            if (!lives.has(id)) {
              const element = place(id);
              element.style.display = 'block';
              try { lives.set(id, {element, mount: await beside(element, {...what, instance: instance++})}); }
              catch (error) { element.remove(); throw error; }
            }
            front(id);
            const shown = lives.get(id).mount;
            shown.editor?.focus?.();
            return JSON.stringify({ok: {record: shown.state()?.record ?? null, kept: lives.size}});
          } catch (error) { return JSON.stringify({error: failure(error)}); }
        },
        // Nothing is in front: the view shows Chat's own page, which nobody is shown.
        rest() { front(null); return true; },
        async close(id) {
          const gone = lives.get(id);
          if (!gone) return false;
          lives.delete(id);
          gone.element.remove();
          await gone.mount.cleanup?.().catch(() => {});
          return true;
        },
      })});
    })();
    """#
}
