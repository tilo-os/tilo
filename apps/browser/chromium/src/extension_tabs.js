// Tabs and windows for Chrome extensions, which Chromium does not give them in
// an offscreen embedding: its tab registry holds only the tabs of its own
// windows, and Tilo draws every page offscreen. Run in an extension's service
// worker and its own pages, this answers the tab and window APIs, the toolbar
// click, keyboard shortcuts, the address's keyword, a tab's capture, context
// menu items, notifications, signing in, scripts on a tab, what network
// rules did in a tab and downloads from the host's tabs instead, over a
// request the host answers itself. A page of the extension is known by its
// address; its worker, which has none, by a token the Browser wrote with
// this script into the worker it runs first (extensions.rs).
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

  // Calls not answered yet. In Chrome a call is on its way once made, so a
  // popup acts and closes at once (`tabs.reload(tab.id); window.close()`,
  // as Privacy Badger's "Disable for this site" and uBlock Origin Lite's
  // element zapper do). Here a call is a request its page cancels as it
  // closes, so the page closes once its calls are answered, or after
  // two seconds.
  let pending = 0;
  const unblocked = [];
  const settle = () => {
    pending -= 1;
    if (!pending) for (const resume of unblocked.splice(0)) resume();
  };
  if (typeof document !== 'undefined' && typeof globalThis.close === 'function') {
    const close = globalThis.close.bind(globalThis);
    let closing = false;
    globalThis.close = () => {
      if (!pending) { close(); return; }
      if (closing) return;
      closing = true;
      const late = setTimeout(close, 2000);
      unblocked.push(() => { clearTimeout(late); close(); });
    };
  }

  // Chrome's APIs return a promise, or call a trailing callback instead.
  // A callback learns of a failure as Chrome tells it: runtime.lastError is
  // set while it runs, which is what extensions check before they read
  // what they were given.
  const failed = (callback, error) => {
    const message = String((error && error.message) || error);
    let checked = false;
    let set = false;
    try {
      Object.defineProperty(runtime, 'lastError', {
        configurable: true,
        enumerable: true,
        get() { checked = true; return {message}; },
      });
      set = true;
    } catch (ignored) {
      // Chromium's own lastError stays; the callback still runs.
    }
    try {
      callback();
    } finally {
      if (set) delete runtime.lastError;
      if (!checked) console.error(`Unchecked runtime.lastError: ${message}`);
    }
  };
  const api = (method) => (...args) => {
    const callback = typeof args[args.length - 1] === 'function' ? args.pop() : null;
    const result = method(...args);
    pending += 1;
    result.then(settle, settle);
    if (!callback) return result;
    result.then((value) => callback(value), (error) => failed(callback, error));
    return undefined;
  };
  const replace = (namespace, name, method) => {
    if (namespace) namespace[name] = api(method);
  };

  // Events the host fires: a toolbar click, a tab that changed. The host
  // fires only those an extension listens to, so it is told of each.
  const listeners = new Map();
  const listened = new Set();
  // What a listener is given where the host's arguments are not all it
  // gets: a callback (omnibox.onInputChanged's `suggest`).
  const adapters = new Map();
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
  // Ports to the scripts the host runs in pages where the extension has no
  // world of its own (extension_content.js): one it opens with tabs.connect,
  // or one such a script opens with runtime.connect. What they carry goes to
  // the host in order: while one request is under way, the next gathers all
  // that follows.
  const emitter = () => {
    const own = new Set();
    return {
      own,
      addListener: (listener) => { if (typeof listener === 'function') own.add(listener); },
      removeListener: (listener) => { own.delete(listener); },
      hasListener: (listener) => own.has(listener),
      hasListeners: () => own.size > 0,
    };
  };
  const fire = (target, ...args) => {
    for (const listener of [...target.own]) {
      try {
        listener(...args);
      } catch (error) {
        console.error(error);
      }
    }
  };
  let outbox = [];
  let sending = false;
  const post = async (op) => {
    outbox.push(op);
    if (sending) return;
    sending = true;
    while (outbox.length) {
      const ops = outbox;
      outbox = [];
      await call('ports', {ops}).catch((error) => console.error(error));
    }
    sending = false;
  };
  const ports = new Map();
  const port = (id, name, sender) => {
    const own = {onMessage: emitter(), onDisconnect: emitter()};
    own.port = {
      name,
      onMessage: own.onMessage,
      onDisconnect: own.onDisconnect,
      postMessage(value) {
        if (ports.get(id) !== own) throw new Error('Attempting to use a disconnected port object');
        // Copied now, as Chrome copies it: a later change is not sent.
        post({op: 'post', port: id, message: value === undefined ? null : JSON.parse(JSON.stringify(value))});
      },
      disconnect() {
        if (ports.get(id) !== own) return;
        ports.delete(id);
        post({op: 'disconnect', port: id});
      },
    };
    if (sender) own.port.sender = sender;
    ports.set(id, own);
    return own.port;
  };

  // Those scripts hear of the extension's storage changing. The worker
  // hears of every change, also of one that wakes it, which only a listener
  // its first script adds is in time for; a page tells of them only for an
  // extension without a worker. Once no such script is left, the host says
  // so and the listener goes, so a sleeping worker is not woken for them.
  const manifest = runtime.getManifest();
  const worker = typeof ServiceWorkerGlobalScope === 'function' && globalThis instanceof ServiceWorkerGlobalScope;
  const tells = worker || !(manifest.background && manifest.background.service_worker);
  const storageEvents = globalThis.chrome.storage && chrome.storage.onChanged;
  let watched = 0;
  const forward = (changes, area) => {
    const asked = watched;
    call('storage.changed', {changes, area}).then((wanted) => {
      if (!wanted && asked === watched) storageEvents.removeListener(forward);
    }, () => {});
  };
  const watch = () => {
    if (!tells || !storageEvents) return false;
    ++watched;
    if (!storageEvents.hasListener(forward)) storageEvents.addListener(forward);
    return true;
  };
  if ((manifest.permissions || []).includes('scripting')) watch();

  // Network rules (declarativeNetRequest). Chromium counts what they do in
  // the Browser's tabs for no tab (-1), so it shows no count for the page
  // and answers for no tab. The Browser's own extension sees the requests
  // rules acted on in each tab, and the host counts them for the page: on
  // the button of an extension that asks (displayActionCountAsBadgeText),
  // changed as it says (tabUpdate). Which rules matched a request is asked
  // of the extension's own rules here.
  const rules = globalThis.chrome.declarativeNetRequest;
  const testOutcome = rules && typeof rules.testMatchOutcome === 'function' ? rules.testMatchOutcome.bind(rules) : null;
  const matchedRules = (request) => {
    if (!testOutcome) return Promise.resolve([]);
    const {url, type, method, initiator, tabId} = request;
    const asked = {url, type, method, tabId};
    if (initiator) asked.initiator = initiator;
    return testOutcome(asked).then((outcome) => outcome.matchedRules || [], () => []);
  };

  // Events the person causes, during whose listeners an API that needs a
  // gesture (sidePanel.open) may be called; `acting` while one runs. In a
  // page of the extension the page's own activation counts.
  const PERSON = new Set(['action.onClicked', 'commands.onCommand', 'contextMenus.onClicked']);
  let acting = 0;
  const gesture = () => acting > 0
    || (typeof navigator !== 'undefined' && !!navigator.userActivation && navigator.userActivation.isActive);

  // The extension's side panel, which the Browser shows beside the page.
  // Chromium keeps its options, for every tab and for one, and how a click
  // on the button treats it, and answers for them; it finds no window to
  // open it in, so the host asks the Browser to show it beside the tab's
  // page, and to show what the options say where it shows already.
  const sidePanel = (() => {
    const panel = chrome.sidePanel;
    if (!panel || typeof panel.getOptions !== 'function') return null;
    const native = {};
    for (const name of ['getOptions', 'setOptions', 'getPanelBehavior', 'setPanelBehavior']) native[name] = panel[name].bind(panel);
    // Whether a click on the button opens the panel, read once and kept as
    // the extension changes it; null until read.
    let opensOnClick = null;
    const read = native.getPanelBehavior().then((behavior) => {
      if (opensOnClick === null) opensOnClick = !!(behavior && behavior.openPanelOnActionClick);
    }, () => {
      if (opensOnClick === null) opensOnClick = false;
    });
    replace(panel, 'setPanelBehavior', async (behavior) => {
      await native.setPanelBehavior(behavior || {});
      if (behavior && typeof behavior.openPanelOnActionClick === 'boolean') opensOnClick = behavior.openPanelOnActionClick;
    });
    // The page the options show, named from the extension's root; none
    // where the panel is off or has no page.
    const pageOf = (options) => {
      if (!options || options.enabled === false || typeof options.path !== 'string' || !options.path) return null;
      return options.path.startsWith('chrome-extension:') ? options.path : runtime.getURL(options.path.replace(/^\//, ''));
    };
    // In Chrome a window holds tabs; here each tab is a window of its own.
    const tabOf = (options) => {
      const {tabId, windowId} = options || {};
      if (Number.isInteger(tabId)) return tabId;
      if (Number.isInteger(windowId)) return windowId;
      throw new Error('At least one of `tabId` and `windowId` must be provided');
    };
    replace(panel, 'open', (options) => {
      const acted = gesture();
      return (async () => {
        if (!acted) throw new Error('`sidePanel.open()` may only be called in response to a user gesture.');
        const tabId = tabOf(options);
        const forTab = Number.isInteger((options || {}).tabId);
        const url = pageOf(await native.getOptions(forTab ? {tabId} : {}));
        if (!url) throw new Error(`No active side panel for ${forTab ? 'tabId' : 'windowId'}: ${tabId}`);
        await call('sidePanel.show', {tabId, url, mode: 'open'});
      })();
    });
    replace(panel, 'close', async (options) => {
      await call('sidePanel.show', {tabId: tabOf(options), url: null, mode: 'close'});
    });
    // Where the panel shows, it follows its options: another page, or
    // closed once turned off. Options for every tab reach each tab without
    // options of its own.
    replace(panel, 'setOptions', async (options) => {
      await native.setOptions(options || {});
      const tabId = (options || {}).tabId;
      const shown = Number.isInteger(tabId) ? [tabId] : await call('sidePanel.shown');
      await Promise.all((shown || []).map(async (shownOver) => {
        const url = pageOf(await native.getOptions({tabId: shownOver}));
        await call('sidePanel.show', {tabId: shownOver, url, mode: 'options'});
      }));
    });
    return {native, pageOf, read, opensOnClick: () => opensOnClick};
  })();

  // The worker's first script has run once this task ends: its listeners
  // are registered, and an event may reach them.
  let ready = false;
  setTimeout(() => { ready = true; });
  Object.defineProperty(globalThis, '__tiloExtensionTabs', {
    value: Object.freeze({
      get ready() { return ready; },
      watch,
      // A script in a page opened a port to the extension: taken here when
      // something listens for one.
      connected(id, name, sender) {
        const onConnect = runtime.onConnect;
        if (!onConnect || !onConnect.hasListeners() || ports.has(id)) return false;
        onConnect.dispatch(port(id, name, sender));
        return true;
      },
      // A message on a port, or its other end closing: false for a port not
      // known here (the worker slept since it opened).
      port(id, op, value) {
        const own = ports.get(id);
        if (!own) return false;
        if (op === 'message') {
          fire(own.onMessage, value, own.port);
        } else {
          ports.delete(id);
          fire(own.onDisconnect, own.port);
        }
        return true;
      },
      // Which of these requests rules acted on in a tab the extension's own
      // rules match: the host counts them for the page.
      matched(requests) {
        return Promise.all((requests || []).map((request) => matchedRules(request).then((found) => found.length > 0)));
      },
      // The host tells the extension of a tab event: every listener runs.
      // One the person caused (a click, a shortcut, a menu item) lets its
      // listeners do what needs a gesture while they run, as in Chrome.
      dispatch(name, args) {
        const run = () => {
          const adapt = adapters.get(name);
          const given = adapt ? adapt(args) : args;
          const acted = PERSON.has(name);
          if (acted) ++acting;
          try {
            for (const listener of listeners.get(name) || []) {
              try {
                listener(...given);
              } catch (error) {
                console.error(error);
              }
            }
          } finally {
            if (acted) --acting;
          }
          return (listeners.get(name) || new Set()).size;
        };
        return name === 'webNavigation.onCreatedNavigationTarget' ? sourceFrame(args[0]).then(run) : run();
      },
      // The person clicked the extension's toolbar button over `tab`: its
      // side panel opens or closes there when it asked for that
      // (`openPanelOnActionClick`) and has one for the tab, as in Chrome;
      // otherwise the extension hears of the click. One known not to open
      // its panel on a click hears of it at once, while the click's gesture
      // lasts.
      clicked(tab) {
        if (!sidePanel || sidePanel.opensOnClick() === false) return this.dispatch('action.onClicked', [tab]);
        return (async () => {
          await sidePanel.read;
          const url = sidePanel.opensOnClick() && sidePanel.pageOf(await sidePanel.native.getOptions({tabId: tab.id}));
          if (!url) return this.dispatch('action.onClicked', [tab]);
          await call('sidePanel.show', {tabId: tab.id, url, mode: 'toggle'});
          return 0;
        })();
      },
      // The person opened the extension's side panel over the tab `tabId`
      // from the Browser: its page for that tab, if it has one.
      async showPanel(tabId) {
        const url = sidePanel && sidePanel.pageOf(await sidePanel.native.getOptions({tabId}));
        if (!url) return false;
        await call('sidePanel.show', {tabId, url, mode: 'open'});
        return true;
      },
    }),
  });

  // A page that opened a tab is known to the host by its tab, and by its
  // frame's address if that was not the main frame: the frame's id and its
  // process are Chromium's, as is the page's frame list here.
  const sourceFrame = async (details) => {
    const frameUrl = details.sourceFrameUrl;
    delete details.sourceFrameUrl;
    let frames = null;
    try {
      frames = await chrome.webNavigation.getAllFrames({tabId: details.sourceTabId});
    } catch (error) {
      // The page is gone: the main frame, process unknown.
    }
    const frame = (frames || []).find((each) => (frameUrl === undefined ? each.frameId === 0 : each.url === frameUrl))
      || (frames || []).find((each) => each.frameId === 0);
    if (frame) {
      details.sourceFrameId = frame.frameId;
      details.sourceProcessId = frame.processId;
    }
  };

  // The host asks which frame of a page the extension's world there is, by
  // a port from that world: Chromium tells only the extension
  // (`sender.frameId`, `sender.documentId`). It is answered here and never
  // reaches the extension's own listeners.
  const onConnect = runtime.onConnect;
  if (onConnect) {
    const probe = (port) => port !== null && typeof port === 'object' && port.name === '__tiloFrame';
    const add = onConnect.addListener.bind(onConnect);
    const remove = onConnect.removeListener.bind(onConnect);
    add((port) => {
      const sender = probe(port) && port.sender;
      if (!sender || sender.id !== runtime.id || !sender.tab || typeof sender.frameId !== 'number') return;
      try {
        port.postMessage({frameId: sender.frameId, documentId: sender.documentId ?? null});
      } catch (error) {
        // The world that asked is gone.
      }
    });
    const own = new Map();
    onConnect.addListener = (listener) => {
      if (typeof listener !== 'function' || own.has(listener)) return;
      const filtered = function (port, ...rest) {
        return probe(port) ? undefined : listener.call(this, port, ...rest);
      };
      own.set(listener, filtered);
      add(filtered);
    };
    onConnect.removeListener = (listener) => {
      const filtered = own.get(listener);
      if (!filtered) return;
      own.delete(listener);
      remove(filtered);
    };
    onConnect.hasListener = (listener) => own.has(listener);
    onConnect.hasListeners = () => own.size > 0;
  }

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
    call('tabs.sendMessage', {tabId, message, frameId: options && options.frameId, documentId: options && options.documentId}));
  if (tabs) {
    tabs.connect = (tabId, info) => {
      const id = 'x' + crypto.randomUUID();
      const opened = port(id, info && info.name !== undefined ? String(info.name) : '');
      post({op: 'open', port: id, tabId, name: opened.name});
      return opened;
    };
  }
  replace(tabs, 'detectLanguage', (tabId) => call('tabs.detectLanguage', {tabId}));
  replace(tabs, 'getZoom', (tabId) => call('tabs.getZoom', {tabId}));
  replace(tabs, 'getZoomSettings', (tabId) => call('tabs.getZoomSettings', {tabId}));
  replace(windows, 'get', (windowId, query) => call('windows.get', {windowId, query}));
  replace(windows, 'getCurrent', (query) => call('windows.getCurrent', {query}));
  replace(windows, 'getLastFocused', (query) => call('windows.getLastFocused', {query}));
  replace(windows, 'getAll', (query) => call('windows.getAll', {query}));
  replace(windows, 'create', (data) => call('windows.create', {...(data || {}), url: [].concat((data || {}).url || []).map(relative)}));
  replace(windows, 'update', (windowId, info) => call('windows.update', {windowId, info}));
  event(tabs, 'onCreated', 'tabs.onCreated');
  event(tabs, 'onUpdated', 'tabs.onUpdated');
  event(tabs, 'onActivated', 'tabs.onActivated');
  event(tabs, 'onHighlighted', 'tabs.onHighlighted');
  event(tabs, 'onZoomChange', 'tabs.onZoomChange');
  event(tabs, 'onRemoved', 'tabs.onRemoved');
  event(windows, 'onFocusChanged', 'windows.onFocusChanged');
  // Chromium follows a tab's navigations itself and tells of them by the
  // ids these APIs give; a page opening a tab it cannot see, the host does.
  event(chrome.webNavigation, 'onCreatedNavigationTarget', 'webNavigation.onCreatedNavigationTarget');
  event(action, 'onClicked', 'action.onClicked');
  event(chrome.browserAction, 'onClicked', 'action.onClicked');

  // The toolbar button's look, which the Browser draws. For every tab
  // Chromium keeps it and answers for it; each change it accepted is told to
  // the host as well, which tells the Browser. For one tab (an ad blocker's
  // count for the page) Chromium finds no tab, these being none of its
  // windows', and refuses ("No tab with id"): the host keeps it until the
  // tab's next document, answers for it, and tells the Browser. A badge
  // colour is taken and not shown: the toolbar's colours are the system's.
  const toolbar = (button) => {
    if (!button) return;
    const native = {};
    for (const name of ['setBadgeText', 'setTitle', 'setPopup', 'setIcon', 'enable', 'disable', 'getBadgeText',
      'getTitle', 'getPopup', 'isEnabled', 'setBadgeBackgroundColor', 'setBadgeTextColor']) {
      if (typeof button[name] === 'function') native[name] = button[name].bind(button);
    }
    // The callback form: MV2's browserAction has no promises.
    const run = (name, ...args) => new Promise((resolve, reject) => {
      native[name](...args, (value) => {
        const error = runtime.lastError;
        if (error) reject(new Error(error.message)); else resolve(value);
      });
    });
    const forTab = (details) => details !== null && typeof details === 'object' && Number.isInteger(details.tabId);
    const set = (tabId, key, value) =>
      call('action.set', {tabId: Number.isInteger(tabId) ? tabId : null, key, value: value ?? null}).then(() => undefined);
    // For every tab Chromium has it already; a page the host serves no
    // bridge to only goes untold.
    const tellAll = (key, value) => set(null, key, value).catch(() => {});
    // An icon image the Browser can draw: the size nearest toolbar size.
    const nearest = (sizes) => {
      if (!sizes || typeof sizes !== 'object' || 'width' in sizes) return sizes;
      const keys = Object.keys(sizes).map(Number).filter((size) => size > 0).sort((a, b) => a - b);
      const key = keys.find((size) => size >= 32) || keys[keys.length - 1];
      return key === undefined ? undefined : sizes[key];
    };
    const iconOf = (details) => {
      const path = nearest(typeof details.path === 'string' ? {32: details.path} : details.path);
      if (typeof path === 'string') return {path: new URL(path, location.href).pathname};
      const image = nearest(details.imageData);
      if (!image || !(image.data instanceof Uint8ClampedArray)) return null;
      const {width, height, data} = image;
      if (width < 1 || height < 1 || width > 128 || height > 128 || data.length !== width * height * 4) return null;
      let binary = '';
      for (let at = 0; at < data.length; at += 0x8000) binary += String.fromCharCode(...data.subarray(at, at + 0x8000));
      return {width, height, data: btoa(binary)};
    };
    const text = (value) => (typeof value === 'string' ? value : null);
    // A popup is named from the extension's root, wherever it is set from.
    const popupOf = (popup) => (typeof popup !== 'string' ? null
      : popup && !popup.startsWith('chrome-extension:') ? runtime.getURL(popup.replace(/^\//, '')) : popup);
    const setters = {
      setBadgeText: ['text', (details) => text(details.text)],
      setTitle: ['title', (details) => text(details.title)],
      setPopup: ['popup', (details) => popupOf(details.popup)],
      setIcon: ['icon', iconOf],
    };
    for (const [name, [key, valueOf]] of Object.entries(setters)) {
      if (!native[name]) continue;
      replace(button, name, async (details) => {
        if (forTab(details)) return set(details.tabId, key, valueOf(details));
        await run(name, details);
        // The popup for every tab as Chromium resolved it.
        await tellAll(key, key === 'popup' && native.getPopup ? await run('getPopup', {}) : valueOf(details || {}));
        return undefined;
      });
    }
    for (const [name, key] of [['getBadgeText', 'text'], ['getTitle', 'title'], ['getPopup', 'popup']]) {
      if (!native[name]) continue;
      replace(button, name, async (details) => {
        if (!forTab(details)) return run(name, details || {});
        const value = await call('action.get', {tabId: details.tabId, key});
        return value === undefined || value === null ? run(name, {}) : value;
      });
    }
    for (const name of ['setBadgeBackgroundColor', 'setBadgeTextColor']) {
      if (!native[name]) continue;
      replace(button, name, (details) => (forTab(details)
        ? call('tabs.get', {tabId: details.tabId}).then(() => undefined)
        : run(name, details)));
    }
    for (const [name, enabled] of [['enable', true], ['disable', false]]) {
      if (!native[name]) continue;
      replace(button, name, async (tabId) => {
        if (Number.isInteger(tabId)) return set(tabId, 'enabled', enabled);
        await run(name);
        await tellAll('enabled', enabled);
        return undefined;
      });
    }
    if (native.isEnabled) {
      replace(button, 'isEnabled', async (tabId) => {
        if (!Number.isInteger(tabId)) return run('isEnabled');
        const value = await call('action.get', {tabId, key: 'enabled'});
        return typeof value === 'boolean' ? value : run('isEnabled');
      });
    }
    // Chromium finds no window to open it in: the Browser opens it over the
    // page the window shows, as a click on the button would, if that page
    // has a popup.
    if (native.getPopup) {
      replace(button, 'openPopup', async (options) => {
        const windowId = options && options.windowId;
        const window = await (Number.isInteger(windowId)
          ? call('windows.get', {windowId})
          : call('windows.getLastFocused', {})).catch(() => {
          throw new Error('Could not find an active browser window.');
        });
        const tabId = window.id;
        const popup = await button.getPopup({tabId});
        const enabled = native.isEnabled ? await button.isEnabled(tabId) : true;
        if (!popup || !enabled) throw new Error('Extension does not have a popup on the active tab.');
        await call('action.openPopup', {tabId, popup});
      });
    }
  };
  toolbar(action);
  toolbar(chrome.browserAction);

  // Downloads. Chromium starts an extension's download on none of the
  // host's pages, and cancels it: the host starts it on the page in front
  // instead, through the Browser's own downloads (capabilities.h), so the
  // person is asked as for any download, which also answers `saveAs`. It is
  // saved under the last part of `filename`, in a folder of its own like
  // every download here. Everything else (search, pause, resume, cancel,
  // erase, onCreated and onChanged) is Chromium's own, which sees every
  // download of the profile; open and show ask the Browser, which opens
  // files for the person.
  const downloads = chrome.downloads;
  if (downloads) {
    replace(downloads, 'download', async (options) => {
      const {url, filename, method, headers, body} = options || {};
      if (typeof url !== 'string' || !url) throw new Error('Invalid URL');
      // The host starts a download by its address alone.
      if ((method && String(method).toUpperCase() !== 'GET') || body !== undefined) {
        throw new Error('Tilo does not support downloads with a request body yet');
      }
      if (Array.isArray(headers) && headers.length) {
        throw new Error('Tilo does not support downloads with headers of their own yet');
      }
      let address;
      try {
        address = new URL(url, location.href).href;
      } catch (error) {
        throw new Error('Invalid URL');
      }
      return call('downloads.download', {url: address, filename: typeof filename === 'string' ? filename : ''});
    });
    replace(downloads, 'open', (downloadId) => call('downloads.open', {downloadId}));
    // Chrome answers show() at once, with nothing to wait for.
    const show = api((downloadId) => call('downloads.show', {downloadId}));
    downloads.show = (downloadId) => {
      show(downloadId).catch((error) => console.error(`Unchecked runtime.lastError: ${error.message}`));
    };
  }

  // Keyboard shortcuts: the host gives each command the key its manifest
  // suggests, as Chrome does, and fires onCommand with the page it was
  // pressed on (extension_commands.h). The toolbar button's own command acts
  // as a click on it, and the Browser answers that.
  if (!chrome.commands && manifest.commands) chrome.commands = {onCommand: {}};
  const commands = chrome.commands;
  if (commands) {
    const button = manifest.action ? '_execute_action'
      : manifest.browser_action ? '_execute_browser_action'
        : manifest.page_action ? '_execute_page_action' : null;
    const localized = (text) => (typeof text === 'string' && chrome.i18n
      ? text.replace(/__MSG_(\w+)__/g, (whole, key) => chrome.i18n.getMessage(key) || whole) : text);
    replace(commands, 'getAll', async () => {
      const listed = (await call('commands.getAll')) || [];
      // Chrome lists the button's command also when the manifest names none.
      if (button && !listed.some(({name}) => name === button)) listed.unshift({name: button, description: '', shortcut: ''});
      return listed.map(({name, description, shortcut}) => ({name, description: localized(description), shortcut}));
    });
    event(commands, 'onCommand', 'commands.onCommand');
  }

  // Chromium's own call, in the callback form older extensions' APIs take.
  const nativeCall = (method, ...args) => new Promise((resolve, reject) => {
    method(...args, (value) => {
      const error = runtime.lastError;
      if (error) reject(new Error(error.message)); else resolve(value);
    });
  });
  if (rules && typeof rules.setExtensionActionOptions === 'function') {
    const native = rules.setExtensionActionOptions.bind(rules);
    replace(rules, 'setExtensionActionOptions', async (options) => {
      const {displayActionCountAsBadgeText: display, tabUpdate} = options || {};
      if (tabUpdate !== undefined && (tabUpdate === null || typeof tabUpdate !== 'object'
        || !Number.isInteger(tabUpdate.tabId) || !Number.isInteger(tabUpdate.increment))) {
        throw new Error('Invalid value for tabUpdate');
      }
      // Chromium keeps the choice for every tab, and answers getBadgeText by it.
      if (typeof display === 'boolean') await nativeCall(native, {displayActionCountAsBadgeText: display});
      if (typeof display === 'boolean' || tabUpdate) {
        await call('dnr.options', {display: typeof display === 'boolean' ? display : null, tabUpdate: tabUpdate || null});
      }
    });
  }
  // The rules that matched in one tab: Chromium finds none of the Browser's
  // tabs and refuses ("No tab with id"). The host knows the requests rules
  // acted on in the tab's page; which of the extension's own rules matched
  // each is asked of them.
  if (rules && typeof rules.getMatchedRules === 'function') {
    const native = rules.getMatchedRules.bind(rules);
    replace(rules, 'getMatchedRules', async (filter) => {
      const tabId = filter && filter.tabId;
      try {
        return await nativeCall(native, filter || {});
      } catch (error) {
        if (!Number.isInteger(tabId) || !/^No tab with id/.test(error.message)) throw error;
      }
      const since = typeof filter.minTimeStamp === 'number' ? filter.minTimeStamp : -Infinity;
      const requests = (await call('dnr.requests', {tabId})).filter((request) => request.timeStamp >= since);
      const found = await Promise.all(requests.map(matchedRules));
      const rulesMatchedInfo = [];
      requests.forEach((request, index) => {
        for (const rule of found[index]) rulesMatchedInfo.push({rule, tabId, timeStamp: request.timeStamp});
      });
      return {rulesMatchedInfo};
    });
  }

  // Access an extension asks for later (`optional_permissions`,
  // `optional_host_permissions`). Chromium asks the person in its own
  // window's dialog, which Tilo's pages are in none of: the host asks the
  // Browser instead, which keeps what the person allowed with the extension
  // (extension_permissions.h). As in Chrome, a request is made in answer to
  // something the person did, and the extension's worker and pages hear of
  // what it gained or gave up.
  const permissions = chrome.permissions;
  if (permissions) {
    const setOf = (given) => ({
      permissions: [].concat((given && given.permissions) || []),
      origins: [].concat((given && given.origins) || []),
    });
    replace(permissions, 'getAll', () => call('permissions.getAll'));
    replace(permissions, 'contains', (given) => call('permissions.contains', setOf(given)));
    replace(permissions, 'request', (given) => {
      const activation = typeof navigator !== 'undefined' && navigator.userActivation;
      if (!activation || !activation.isActive) {
        return Promise.reject(new Error('This function must be called during a user gesture'));
      }
      return call('permissions.request', setOf(given));
    });
    replace(permissions, 'remove', (given) => call('permissions.remove', setOf(given)));
    event(permissions, 'onAdded', 'permissions.onAdded');
    event(permissions, 'onRemoved', 'permissions.onRemoved');
  }

  // The address's keyword (the manifest's `omnibox`): the Browser tells the
  // host what the person types after it, and the host tells the extension,
  // as Chrome does. What it suggests for a change, and its default
  // suggestion, show under the address; Enter, or a suggestion chosen,
  // reaches onInputEntered with the page in the current tab.
  if (!chrome.omnibox && manifest.omnibox) chrome.omnibox = {};
  const omnibox = chrome.omnibox;
  if (omnibox) {
    const events = ['onInputStarted', 'onInputChanged', 'onInputEntered', 'onInputCancelled'];
    for (const key of events) {
      if (!omnibox[key]) omnibox[key] = {};
      event(omnibox, key, 'omnibox.' + key);
    }
    const text = (value) => (value === undefined || value === null ? '' : String(value));
    // Only what the Browser shows of a suggestion crosses to the host.
    const plain = (suggestion) => ({content: text(suggestion && suggestion.content),
      description: text(suggestion && suggestion.description)});
    adapters.set('omnibox.onInputChanged', ([input, request]) => [input, (suggestions) => {
      call('omnibox.suggest', {request, suggestions: [].concat(suggestions || []).map(plain)})
        .catch((error) => console.error(error));
    }]);
    replace(omnibox, 'setDefaultSuggestion', (suggestion) =>
      call('omnibox.setDefaultSuggestion', {description: plain(suggestion).description}).then(() => undefined));
  }

  // Signing in (`identity`). Chrome shows the page an extension names in a
  // window of its own and ends the call once that page goes to the
  // extension's own address; here the Browser shows it in a panel over the
  // page in front, or loads it out of sight when the extension may not ask
  // the person, and the host watches where it goes (extension_sign_in.h).
  // The outcome is asked for in turns the host holds a while, and between
  // them a call to Chromium keeps a worker awake, as Chrome keeps it awake
  // while the call is under way. No one signs in to the Browser itself, so
  // getAuthToken, which asks the Browser's account, says so.
  if (!chrome.identity && (manifest.permissions || []).includes('identity')) chrome.identity = {};
  const identity = chrome.identity;
  if (identity) {
    if (typeof identity.getRedirectURL !== 'function') {
      identity.getRedirectURL = (path) => {
        let own = path === undefined || path === null ? '/' : String(path);
        if (!own.startsWith('/')) own = '/' + own;
        return `https://${runtime.id}.chromiumapp.org${own}`;
      };
    }
    const awake = () => {
      try {
        runtime.getPlatformInfo(() => void runtime.lastError);
      } catch (ignored) {
        // A page needs no keeping awake.
      }
    };
    replace(identity, 'launchWebAuthFlow', async (details) => {
      if (!details || typeof details.url !== 'string') throw new Error('identity.launchWebAuthFlow needs the url of the page to sign in on');
      const timeout = details.timeoutMsForNonInteractive;
      const flow = await call('identity.launchWebAuthFlow', {
        url: details.url,
        interactive: details.interactive === true,
        abortOnLoad: details.abortOnLoadForNonInteractive !== false,
        timeout: Number.isFinite(timeout) ? Math.max(0, Math.round(timeout)) : null,
      });
      for (;;) {
        const url = await call('identity.wait', {flow});
        if (typeof url === 'string') return url;
        awake();
      }
    });
    replace(identity, 'getAuthToken', async () => {
      throw new Error('The user is not signed in.');
    });
  }

  // Context menu items: the host keeps them and shows them in the page's
  // menu (extension_menus.h); a choice comes back as onClicked. An
  // extension may ask for them later, too.
  const declared = (manifest.permissions || []).concat(manifest.optional_permissions || []);
  if (!chrome.contextMenus && declared.includes('contextMenus')) {
    chrome.contextMenus = {onClicked: {}};
  }
  const menus = chrome.contextMenus;
  if (menus) {
    // Changes reach the host in the order they were made: an extension
    // often removes all its items and makes them anew.
    let queue = Promise.resolve();
    const inOrder = (method, args) => {
      const result = queue.then(() => call(method, args));
      queue = result.catch(() => {});
      return result;
    };
    // A script's own `onclick` cannot cross to the host; onClicked can.
    const plain = (properties) => {
      const own = {...(properties || {})};
      delete own.onclick;
      return own;
    };
    let generated = 0;
    // `create` answers with the item's id at once, and calls back once made.
    menus.create = (properties, callback) => {
      const own = plain(properties);
      if (own.id === undefined) own.id = ++generated;
      inOrder('contextMenus.create', {properties: own}).then(() => {
        if (typeof callback === 'function') callback();
      }, (error) => {
        if (typeof callback === 'function') failed(callback, error);
        else console.error(`Unchecked runtime.lastError: ${error.message}`);
      });
      return own.id;
    };
    replace(menus, 'update', (id, properties) => inOrder('contextMenus.update', {id, properties: plain(properties)}));
    replace(menus, 'remove', (id) => inOrder('contextMenus.remove', {id}));
    replace(menus, 'removeAll', () => inOrder('contextMenus.removeAll'));
    event(menus, 'onClicked', 'contextMenus.onClicked');
  }

  // Notifications: Tilo's notification service shows them, through the
  // Browser, and the host keeps them (extension_notifications.h). A choice
  // comes back as onClicked or onButtonClicked, then onClosed.
  const notices = chrome.notifications;
  if (notices) {
    // What crosses to the host: plain data, so an icon given as pixels
    // counts as given and a function is left out.
    const plainOptions = (options) => {
      const own = {...(options || {})};
      for (const key of ['iconUrl', 'imageUrl', 'appIconMaskUrl']) {
        if (own[key] !== undefined && typeof own[key] !== 'string') own[key] = {};
      }
      return JSON.parse(JSON.stringify(own));
    };
    // `create(options)` makes up the id.
    replace(notices, 'create', (id, options) => {
      if (options === undefined && id !== null && typeof id === 'object') [id, options] = ['', id];
      return call('notifications.create', {id: typeof id === 'string' ? id : '', options: plainOptions(options)});
    });
    replace(notices, 'update', (id, options) => call('notifications.update', {id: String(id), options: plainOptions(options)}));
    replace(notices, 'clear', (id) => call('notifications.clear', {id: String(id)}));
    replace(notices, 'getAll', async () => (await call('notifications.getAll')) || {});
    event(notices, 'onClicked', 'notifications.onClicked');
    event(notices, 'onButtonClicked', 'notifications.onButtonClicked');
    event(notices, 'onClosed', 'notifications.onClosed');
  }

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
      const results = await call('scripting.executeScript', {
        target,
        world,
        func: typeof func === 'function' ? func.toString() : undefined,
        args: args || [],
        files: await sources(files),
        content: await contentOf(),
      });
      // The script may run in a world the host made, which hears of storage.
      if (world !== 'MAIN') watch();
      return results;
    });
    replace(scripting, 'insertCSS', async (injection) => {
      const {target, css, files} = injection || {};
      return call('scripting.insertCSS', {target, css: [css || '', ...(await sources(files))].join('\n')});
    });
  }
})();
