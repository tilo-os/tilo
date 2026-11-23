// Private-world observer of composition on the page, registered before the
// page's own scripts so page automation can tell whether text input is in
// progress.
globalThis.__tiloTextComposition = (() => {
  let trusted = false, unknown = false;
  // Page-generated composition is tracked per editor, so that an editor the
  // page removes mid-composition (custom editors swap their nodes) stops
  // counting instead of leaving composition active until the next load.
  // Pages cannot grow it without bound: past the cap the state is unknown.
  const synthetic = new Set(), MAX_SYNTHETIC = 64;
  const connected = Object.getOwnPropertyDescriptor(Node.prototype, 'isConnected').get;
  const isConnected = node => { try { return Reflect.apply(connected, node, []); } catch { return false; } };
  const prune = () => { for (const target of synthetic) if (!isConnected(target)) synthetic.delete(target); };
  let root = null;
  // An initial about:blank iframe can retain its Window when it navigates.
  // Observe the native load on that Window, then bind its current Document.
  // A late context or a document.open-removed listener remains unknown.
  const observer = new MutationObserver(records => {
    for (const record of records) {
      for (const node of record.removedNodes) if (node === root) unknown = true;
    }
    if (root !== document.documentElement) unknown = true;
    if (unknown) { root = null; observer.disconnect(); }
  });
  const loaded = event => {
    if (!event.isTrusted || unknown || !['http:', 'https:'].includes(location.protocol)) return;
    root = document.documentElement;
    if (!root) return;
    observer.observe(document, {childList: true});
    removeEventListener('DOMContentLoaded', loaded);
  };
  addEventListener('DOMContentLoaded', loaded);
  addEventListener('compositionstart', event => {
    if (event.isTrusted) { trusted = true; return; }
    prune();
    if (!isConnected(event.target)) return;
    if (synthetic.size >= MAX_SYNTHETIC && !synthetic.has(event.target)) unknown = true;
    else synthetic.add(event.target);
  }, true);
  addEventListener('compositionend', event => {
    // Page-generated end events cannot clear actual native composition.
    if (event.isTrusted) trusted = false; else synthetic.delete(event.target);
  }, true);
  return Object.freeze({active: () => {
    prune();
    return unknown || !root || root !== document.documentElement || trusted || synthetic.size > 0;
  }});
})();
