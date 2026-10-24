// Tabs and windows for Chrome extensions, which Chromium does not give them in
// an offscreen embedding: its tab registry holds only the tabs of its own
// windows, and Tilo draws every page offscreen. Run in an extension's service
// worker and its own pages, this answers the tab and window APIs, the toolbar
// click, a tab's capture and scripts on a tab from the host's tabs instead, over a request the
// host answers itself. A page of the extension is known by its address; its
// worker, which has none, by a token the Browser wrote with this script into
// the worker it runs first (extensions.rs).
(() => {
  const runtime = globalThis.chrome && chrome.runtime;
  if (!runtime || !runtime.id || location.protocol !== 'chrome-extension:') return;
  const token = globalThis.__tiloBridgeToken || null;
  delete globalThis.__tiloBridgeToken;
  if (globalThis.__tiloExtensionTabs) return;
  const BRIDGE = 'https://extension-bridge.tilo.invalid/';

  const call = async (method, args) => {
    const response = await fetch(BRIDGE + method, {
      method: 'POST',
      // A plain body is a simple request: no preflight before each call.
      headers: {'Content-Type': 'text/plain'},
      body: JSON.stringify({token, args: args === undefined ? null : args}),
      credentials: 'omit',
      cache: 'no-store',
    });
    const reply = await response.json();
    if (reply.error) throw new Error(reply.error);
    return reply.result;
  };

  // Chrome's APIs return a promise, or call a trailing callback instead.
  const api = (method) => (...args) => {
    const callback = typeof args[args.length - 1] === 'function' ? args.pop() : null;
    const result = method(...args);
    if (!callback) return result;
    result.then((value) => callback(value), (error) => {
      console.error(`Unchecked runtime.lastError: ${error.message}`);
      callback();
    });
    return undefined;
  };
  const replace = (namespace, name, method) => {
    if (namespace) namespace[name] = api(method);
  };

  // Events the host fires: a toolbar click, a tab that changed. The host
  // fires only those an extension listens to, so it is told of each.
  const listeners = new Map();
  const listened = new Set();
  const event = (namespace, key, name) => {
    const target = namespace && namespace[key];
    if (!target) return;
    const own = new Set();
    listeners.set(name, own);
    target.addListener = (listener) => {
      if (typeof listener !== 'function') return;
      own.add(listener);
      if (!listened.has(name)) {
        listened.add(name);
        call('listen', {event: name}).catch(() => listened.delete(name));
      }
    };
    target.removeListener = (listener) => own.delete(listener);
    target.hasListener = (listener) => own.has(listener);
    target.hasListeners = () => own.size > 0;
  };
  // The worker's first script has run once this task ends: its listeners
  // are registered, and an event may reach them.
  let ready = false;
  setTimeout(() => { ready = true; });
  Object.defineProperty(globalThis, '__tiloExtensionTabs', {
    value: Object.freeze({
      get ready() { return ready; },
      // The host tells the extension of a tab event: every listener runs.
      dispatch(name, args) {
        for (const listener of listeners.get(name) || []) {
          try {
            listener(...args);
          } catch (error) {
            console.error(error);
          }
        }
        return (listeners.get(name) || new Set()).size;
      },
    }),
  });

  const {tabs, windows, scripting, action} = chrome;
  const relative = (url) => (typeof url === 'string' ? new URL(url, location.href).href : url);
  replace(tabs, 'query', (query) => call('tabs.query', query || {}));
  replace(tabs, 'get', (tabId) => call('tabs.get', {tabId}));
  // In a popup or a worker there is no tab of one's own: undefined, as in Chrome.
  replace(tabs, 'getCurrent', async () => (await call('tabs.getCurrent')) ?? undefined);
  replace(tabs, 'create', (properties) =>
    call('tabs.create', {...(properties || {}), url: relative((properties || {}).url)}));
  replace(tabs, 'update', (tabId, properties) => {
    if (typeof tabId === 'object' && tabId !== null) [tabId, properties] = [undefined, tabId];
    return call('tabs.update', {tabId, properties: {...(properties || {}), url: relative((properties || {}).url)}});
  });
  replace(tabs, 'reload', (tabId, options) => call('tabs.reload', {tabId, options}));
  replace(tabs, 'remove', (tabIds) => call('tabs.remove', {tabIds: [].concat(tabIds)}));
  replace(tabs, 'captureVisibleTab', (windowId, options) => {
    if (typeof windowId === 'object' && windowId !== null) [windowId, options] = [undefined, windowId];
    return call('tabs.captureVisibleTab', {windowId: windowId ?? null, options: options || {}});
  });
  replace(tabs, 'sendMessage', (tabId, message, options) =>
    call('tabs.sendMessage', {tabId, message, frameId: options && options.frameId}));
  replace(windows, 'get', (windowId, query) => call('windows.get', {windowId, query}));
  replace(windows, 'getCurrent', (query) => call('windows.getCurrent', {query}));
  replace(windows, 'getLastFocused', (query) => call('windows.getLastFocused', {query}));
  replace(windows, 'getAll', (query) => call('windows.getAll', {query}));
  replace(windows, 'create', (data) => call('windows.create', {...(data || {}), url: [].concat((data || {}).url || []).map(relative)}));
  replace(windows, 'update', (windowId, info) => call('windows.update', {windowId, info}));
  event(tabs, 'onCreated', 'tabs.onCreated');
  event(tabs, 'onUpdated', 'tabs.onUpdated');
  event(tabs, 'onActivated', 'tabs.onActivated');
  event(tabs, 'onRemoved', 'tabs.onRemoved');
  event(windows, 'onFocusChanged', 'windows.onFocusChanged');
  event(action, 'onClicked', 'action.onClicked');

  if (scripting) {
    // A script's files are the extension's own: read here, run on the tab.
    const sources = (files) =>
      Promise.all((files || []).map((file) => fetch(runtime.getURL(file)).then((r) => r.text())));
    // What a script run where the extension has no world of its own needs
    // of a content script's `chrome` (extension_content.js).
    let content = null;
    const contentOf = async () => {
      if (content) return content;
      const manifest = runtime.getManifest();
      const language = chrome.i18n ? chrome.i18n.getUILanguage() : navigator.language;
      const messages = {};
      const load = async (locale) => {
        try {
          const response = await fetch(runtime.getURL(`_locales/${locale}/messages.json`));
          if (!response.ok) return;
          for (const [key, value] of Object.entries(await response.json())) messages[key.toLowerCase()] = value;
        } catch (error) {
          // No such locale.
        }
      };
      if (manifest.default_locale) {
        const ui = language.replace('-', '_');
        await load(manifest.default_locale);
        if (ui.split('_')[0] !== manifest.default_locale) await load(ui.split('_')[0]);
        if (ui.includes('_') && ui !== manifest.default_locale) await load(ui);
      }
      messages['@@extension_id'] = {message: runtime.id};
      messages['@@ui_locale'] = {message: language.replace('-', '_')};
      content = {manifest, messages, language};
      return content;
    };
    replace(scripting, 'executeScript', async (injection) => {
      const {target, func, args, files, world} = injection || {};
      return call('scripting.executeScript', {
        target,
        world,
        func: typeof func === 'function' ? func.toString() : undefined,
        args: args || [],
        files: await sources(files),
        content: await contentOf(),
      });
    });
    replace(scripting, 'insertCSS', async (injection) => {
      const {target, css, files} = injection || {};
      return call('scripting.insertCSS', {target, css: [css || '', ...(await sources(files))].join('\n')});
    });
  }
})();
