// A content script's `chrome` in a page where Chromium made the extension
// no world of its own. Chromium makes one only for an extension's declared
// content scripts; a script it injects on demand (`scripting.executeScript`
// after a click) runs in a world the host makes, which this gives what
// Chrome gives a content script: runtime messaging and ports, storage and
// its changes, and i18n. Each call goes to the host through a binding only
// this world has, and the host asks the extension's worker or its pages.
// In the world of declared content scripts (`content.declared`), which has
// Chromium's `chrome`, it adds only the ports the extension opens to it
// (tabs.connect), which Chromium cannot deliver to a tab it does not know.
(content) => {
  const send = globalThis.__tiloExtension;
  if (typeof send !== 'function' || globalThis.__tiloExtensionContent) return;
  let next = 0;
  const pending = new Map();
  const call = (method, args) => new Promise((resolve, reject) => {
    const id = ++next;
    pending.set(id, {resolve, reject});
    send(JSON.stringify({id, method, args}));
  });
  // A call nothing answers (id 0): what a port carries.
  const tell = (method, args) => send(JSON.stringify({id: 0, method, args}));
  // Chrome's APIs return a promise, or call a trailing callback instead,
  // with runtime.lastError set while it runs when the call failed.
  const api = (method) => (...args) => {
    const callback = typeof args[args.length - 1] === 'function' ? args.pop() : null;
    const result = method(...args);
    if (!callback) return result;
    result.then((value) => callback(value), (error) => {
      const text = String((error && error.message) || error);
      let checked = false;
      Object.defineProperty(runtime, 'lastError', {
        configurable: true,
        enumerable: true,
        get() { checked = true; return {message: text}; },
      });
      try {
        callback();
      } finally {
        Object.defineProperty(runtime, 'lastError', {configurable: true, enumerable: true, writable: true, value: undefined});
        if (!checked) console.error(`Unchecked runtime.lastError: ${text}`);
      }
    });
    return undefined;
  };
  const event = () => {
    const listeners = new Set();
    return {
      listeners,
      addListener: (listener) => { if (typeof listener === 'function') listeners.add(listener); },
      removeListener: (listener) => listeners.delete(listener),
      hasListener: (listener) => listeners.has(listener),
      hasListeners: () => listeners.size > 0,
    };
  };
  const fire = (target, ...args) => {
    for (const listener of [...target.listeners]) {
      try { listener(...args); } catch (error) { console.error(error); }
    }
  };
  const onMessage = event();
  const onConnect = event();
  const onChanged = event();
  const area = (name) => ({
    get: api((keys) => call('storage', {area: name, op: 'get', args: keys === undefined ? [] : [keys]})),
    set: api((items) => call('storage', {area: name, op: 'set', args: [items]})),
    remove: api((keys) => call('storage', {area: name, op: 'remove', args: [keys]})),
    clear: api(() => call('storage', {area: name, op: 'clear', args: []})),
    getBytesInUse: api((keys) => call('storage', {area: name, op: 'getBytesInUse', args: keys === undefined ? [] : [keys]})),
    onChanged: event(),
  });
  const messages = content.messages || {};
  const message = (name, substitutions) => {
    const entry = messages[String(name).toLowerCase()];
    if (!entry) return '';
    const values = [].concat(substitutions === undefined ? [] : substitutions).map(String);
    let text = String(entry.message || '');
    for (const [key, placeholder] of Object.entries(entry.placeholders || {})) {
      const value = String(placeholder.content || '').replace(/\$(\d)/g, (_, n) => values[Number(n) - 1] || '');
      text = text.replace(new RegExp('\\$' + key + '\\$', 'gi'), value);
    }
    return text.replace(/\$(\d)/g, (_, n) => values[Number(n) - 1] || '').replace(/\$\$/g, '$');
  };

  // Ports, as Chrome's: each end posts JSON values to the other and is told
  // when it closes. One this world opens is known here as `c<n>`; one the
  // extension opens by the name the host gives it.
  const ports = new Map();
  let nextPort = 0;
  const NO_RECEIVER = 'Could not establish connection. Receiving end does not exist.';
  const port = (id, name, sender) => {
    const own = {onMessage: event(), onDisconnect: event()};
    own.port = {
      name,
      onMessage: own.onMessage,
      onDisconnect: own.onDisconnect,
      postMessage(value) {
        if (ports.get(id) !== own) throw new Error('Attempting to use a disconnected port object');
        // Copied now, as Chrome copies it: a later change is not sent.
        const copy = value === undefined ? null : JSON.parse(JSON.stringify(value));
        tell('port', {port: id, op: 'post', message: copy});
      },
      disconnect() {
        if (ports.get(id) !== own) return;
        ports.delete(id);
        tell('port', {port: id, op: 'disconnect'});
      },
    };
    if (sender) own.port.sender = sender;
    ports.set(id, own);
    return own.port;
  };
  const runtime = {
    id: content.id,
    getURL: (path) => `chrome-extension://${content.id}/${String(path).replace(/^\//, '')}`,
    getManifest: () => JSON.parse(JSON.stringify(content.manifest || {})),
    sendMessage: api((...args) => {
      // sendMessage([extensionId,] message [, options]), bound as Chrome
      // binds it: of two, the first is the extension's id when it can be one
      // (a string, null or undefined), else the second is the options.
      if (args.length > 2 || (args.length === 2 && (args[0] == null || typeof args[0] === 'string'))) {
        const target = args.shift();
        if (target != null && target !== content.id) return Promise.reject(new Error(NO_RECEIVER));
      }
      return call('sendMessage', {message: args[0]});
    }),
    // connect([extensionId,] [connectInfo]): as in Chrome, a first string,
    // null or undefined is the id, and null or undefined means this one.
    connect: (...args) => {
      const target = args.length && (args[0] == null || typeof args[0] === 'string') ? args.shift() ?? content.id : content.id;
      const info = args[0] || {};
      const id = 'c' + ++nextPort;
      const opened = port(id, info.name === undefined ? '' : String(info.name));
      // Another extension takes no port from this world.
      if (target !== content.id) setTimeout(() => globalThis.__tiloExtensionContent.port(id, 'disconnect', null, NO_RECEIVER));
      else tell('connect', {port: id, name: opened.name});
      return opened;
    },
    onMessage,
    onConnect,
    lastError: undefined,
  };
  const storage = {local: area('local'), sync: area('sync'), session: area('session'), onChanged};
  if (!content.declared) {
    globalThis.chrome = {
      runtime,
      storage,
      i18n: {
        getMessage: message,
        getUILanguage: () => content.language || navigator.language,
      },
    };
  }
  Object.defineProperty(globalThis, '__tiloExtensionContent', {
    value: Object.freeze({
      settle(id, ok, value) {
        const waiting = pending.get(id);
        if (!waiting) return;
        pending.delete(id);
        if (ok) waiting.resolve(value === null ? undefined : value);
        else waiting.reject(new Error(String(value)));
      },
      // A message for this world's listeners, answered as Chrome answers it:
      // by the first reply, or with nothing (`none` when none listens).
      deliver(message, sender) {
        return new Promise((resolve) => {
          if (!onMessage.listeners.size) { resolve({none: true}); return; }
          let replied = false;
          let waits = false;
          const reply = (value) => { if (!replied) { replied = true; resolve({reply: value === undefined ? null : value}); } };
          for (const listener of onMessage.listeners) {
            let result;
            try { result = listener(message, sender, reply); } catch (error) { console.error(error); }
            if (result === true) waits = true;
            else if (result && typeof result.then === 'function') { waits = true; result.then(reply, () => reply(undefined)); }
          }
          if (!replied && !waits) resolve({});
        });
      },
      // The extension opened a port to this world: taken where a listener
      // waits for one.
      connect(id, name, sender) {
        const listening = content.declared ? chrome.runtime.onConnect.hasListeners() : onConnect.listeners.size > 0;
        if (!listening || ports.has(id)) return false;
        const opened = port(id, name, sender);
        if (content.declared) chrome.runtime.onConnect.dispatch(opened);
        else fire(onConnect, opened);
        return true;
      },
      // A message on a port, or its other end closing (with why, when it
      // never opened).
      port(id, op, value, error) {
        const own = ports.get(id);
        if (!own) return false;
        if (op === 'message') {
          fire(own.onMessage, value, own.port);
          return true;
        }
        ports.delete(id);
        runtime.lastError = error ? {message: error} : undefined;
        try { fire(own.onDisconnect, own.port); } finally { runtime.lastError = undefined; }
        return true;
      },
      // The extension's storage changed, wherever it was changed.
      changed(changes, name) {
        fire(onChanged, changes, name);
        if (storage[name]) fire(storage[name].onChanged, changes);
      },
    }),
  });
}
