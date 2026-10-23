// A content script's `chrome` in a page where Chromium made the extension
// no world of its own. Chromium makes one only for an extension's declared
// content scripts; a script it injects on demand (`scripting.executeScript`
// after a click) runs in a world the host makes, which this gives what
// Chrome gives a content script: runtime messaging, storage and i18n. Each
// call goes to the host through a binding only this world has, and the host
// asks the extension's worker.
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
  const onMessage = event();
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
  globalThis.chrome = {
    runtime: {
      id: content.id,
      getURL: (path) => `chrome-extension://${content.id}/${String(path).replace(/^\//, '')}`,
      getManifest: () => JSON.parse(JSON.stringify(content.manifest || {})),
      sendMessage: api((...args) => {
        // sendMessage([extensionId,] message [, options])
        if (args.length > 1 && typeof args[0] === 'string' && args[0] === content.id) args.shift();
        return call('sendMessage', {message: args[0]});
      }),
      onMessage,
      lastError: undefined,
    },
    storage: {local: area('local'), sync: area('sync'), session: area('session'), onChanged},
    i18n: {
      getMessage: message,
      getUILanguage: () => content.language || navigator.language,
    },
  };
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
      // by the first reply, or with nothing.
      deliver(message, sender) {
        return new Promise((resolve) => {
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
    }),
  });
}
