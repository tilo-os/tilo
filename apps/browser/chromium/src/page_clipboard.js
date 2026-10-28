// The page's clipboard, for an engine that has none of its own (Linux, where
// every page renders offscreen): what the page copies goes to the Browser,
// which puts it on the system clipboard, and what the page pastes or reads
// comes from there (README, "Page clipboard").
//
// Evaluated in a document's main world before its own scripts run, as a
// function of `send`, which takes one copy ({text, html, secret}), and `ask`,
// which asks for the system clipboard's text and calls back with it, or with
// null when there is none (absent where the document reads nothing). It
// returns `prime`, which keeps the system clipboard's text for the next paste
// Chromium makes in this document.
//
// Chromium decides what may copy and read, as in Chrome: a copy event fires
// only when a copy may happen, `writeText` resolves only once it was allowed,
// and `readText` asks the person first. The host holds to the same rules
// whatever a page's script does here: it takes a page's copy only within the
// person's activation of it, and reads the system clipboard only for a site
// the person allowed to read it.
(send, ask) => {
  const apply = Reflect.apply;
  const NativePromise = Promise;
  const then = Promise.prototype.then;
  const addListener = EventTarget.prototype.addEventListener;
  const later = setTimeout;
  const MAX_TEXT = 4 * 1024 * 1024, MAX_HTML = 1024 * 1024;
  // A password, or a code the page asks for as one: what is copied from it
  // is a secret, which the clipboard history and device sync never keep.
  const SECRET_AUTOCOMPLETE = /\b(current-password|new-password|one-time-code)\b/i;
  const secretField = (element) => !!element && element.localName === 'input' &&
    (element.type === 'password' || SECRET_AUTOCOMPLETE.test(element.autocomplete || ''));
  // The focused element, through open shadow roots.
  const focused = () => {
    let element = document.activeElement;
    while (element && element.shadowRoot && element.shadowRoot.activeElement) element = element.shadowRoot.activeElement;
    return element;
  };
  const textField = (element) => !!element && (element.localName === 'textarea' ||
    (element.localName === 'input' && typeof element.selectionStart === 'number'));
  const secretText = (text) => {
    if (secretField(focused())) return true;
    if (!text) return false;
    for (const input of document.querySelectorAll('input')) if (secretField(input) && input.value === text) return true;
    return false;
  };
  const deliver = (text, html, secret) => {
    if (typeof text !== 'string' || !text || text.length > MAX_TEXT) return;
    send({text, html: typeof html === 'string' && html && html.length <= MAX_HTML ? html : '', secret: !!secret});
  };

  // What a copy event's own action takes: the focused field's selected
  // characters, or the document's selection with its markup. Chromium
  // copies nothing out of a password field.
  const selected = () => {
    const field = focused();
    if (field && field.localName === 'input' && field.type === 'password') return null;
    if (textField(field)) {
      const start = field.selectionStart, end = field.selectionEnd;
      if (typeof start === 'number' && end > start) return {text: String(field.value).slice(start, end), html: '', secret: secretField(field)};
    }
    const selection = document.getSelection();
    if (!selection || !selection.rangeCount) return null;
    const text = selection.toString();
    let html = '';
    try {
      const holder = document.createElement('div');
      for (let i = 0; i < selection.rangeCount; ++i) holder.appendChild(selection.getRangeAt(i).cloneContents());
      html = holder.innerHTML;
    } catch (error) {}
    return {text, html, secret: false};
  };

  // What a page's copy listener sets on the event, which Chromium copies in
  // place of the selection when the listener cancels the event.
  const setData = DataTransfer.prototype.setData;
  const clearData = DataTransfer.prototype.clearData;
  const set = new WeakMap();
  DataTransfer.prototype.setData = function (type, data) {
    const result = apply(setData, this, arguments);
    const store = set.get(this);
    if (store && arguments.length >= 2) store.set(String(type).toLowerCase(), String(data));
    return result;
  };
  DataTransfer.prototype.clearData = function (type) {
    const result = apply(clearData, this, arguments);
    const store = set.get(this);
    if (store) { if (arguments.length && type !== undefined) store.delete(String(type).toLowerCase()); else store.clear(); }
    return result;
  };
  const copied = (event) => {
    // Only a copy Chromium makes: a script's own event copies nothing.
    if (!event.isTrusted || !event.clipboardData) return;
    const store = new Map();
    set.set(event.clipboardData, store);
    const own = selected();
    const secret = secretText(own && own.text);
    // Once every listener has run and Chromium has copied.
    later(() => {
      if (!event.defaultPrevented) { if (own) deliver(own.text, own.html, secret || own.secret); return; }
      const text = store.has('text/plain') ? store.get('text/plain') : store.get('text');
      deliver(text, store.get('text/html'), secret || secretText(text));
    }, 0);
  };
  apply(addListener, window, ['copy', copied, true]);
  apply(addListener, window, ['cut', copied, true]);

  // The asynchronous clipboard (`navigator.clipboard`), which only a
  // secure document has, and which Chromium may expose only once the
  // document's first script runs: wrapped as soon as it is there, at the
  // latest by the person's first click or key, which any copy of the page's
  // own needs.
  let wrapped = false;
  const wrapClipboard = () => {
    if (wrapped || typeof Clipboard !== 'function') return;
    wrapped = true;
    const clipboard = Clipboard.prototype;
    const writeText = clipboard.writeText, write = clipboard.write;
    clipboard.writeText = function (text) {
      let value;
      try { value = String(text); } catch (error) { return apply(writeText, this, arguments); }
      const done = arguments.length ? apply(writeText, this, [value]) : apply(writeText, this, arguments);
      const secret = secretText(value);
      apply(then, done, [() => deliver(value, '', secret), () => {}]);
      return done;
    };
    clipboard.write = function (items) {
      const done = apply(write, this, arguments);
      const list = Array.isArray(items) ? items.slice(0, 16) : [];
      apply(then, done, [async () => {
        for (const item of list) {
          try {
            const types = item.types || [];
            const read = async (type) => types.includes(type) ? (await item.getType(type)).text() : undefined;
            const text = await read('text/plain'), html = await read('text/html');
            if (text !== undefined) { deliver(text, html, secretText(text)); return; }
          } catch (error) {}
        }
      }, () => {}]);
      return done;
    };
    // Reading: once Chromium allowed a read, it reads the system clipboard
    // instead of the engine's own. The host reads it only for a site the
    // person allowed to, whatever the page did to these.
    if (!ask) return;
    const fromSystem = (stale) => new NativePromise((resolve) => ask((text) => resolve(typeof text === 'string' ? text : stale)));
    const readText = clipboard.readText, read = clipboard.read;
    clipboard.readText = function () {
      return apply(then, apply(readText, this, arguments), [fromSystem]);
    };
    clipboard.read = function () {
      return apply(then, apply(read, this, arguments), [(stale) => apply(then, fromSystem(null), [(text) => {
        if (text === null) return stale;
        return [new ClipboardItem({'text/plain': new Blob([text], {type: 'text/plain'})})];
      }])]);
    };
  };
  wrapClipboard();
  queueMicrotask(wrapClipboard);
  for (const type of ['pointerdown', 'keydown']) apply(addListener, window, [type, wrapClipboard, true]);

  // Pasting: Chromium pastes from its own clipboard, so the paste it starts
  // is put back as one of the system clipboard's text, for the page to take
  // or to leave to its field.
  let primed = null;
  apply(addListener, window, ['paste', (event) => {
    const text = primed;
    primed = null;
    if (!event.isTrusted || text === null) return;
    event.stopImmediatePropagation();
    event.preventDefault();
    const data = new DataTransfer();
    data.setData('text/plain', text);
    const target = event.target;
    const paste = new ClipboardEvent('paste', {clipboardData: data, bubbles: true, cancelable: true, composed: true});
    if (target.dispatchEvent(paste) && text) document.execCommand('insertText', false, text);
  }, true]);
  // A paste that never came (the page took the key itself) leaves nothing
  // for a later one.
  let primes = 0;
  return (text) => {
    primed = typeof text === 'string' ? text : null;
    const prime = ++primes;
    later(() => { if (primes === prime) primed = null; }, 2000);
  };
}
