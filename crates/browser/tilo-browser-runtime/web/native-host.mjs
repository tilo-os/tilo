// A native app that holds this page as its person's device.
//
// Tilo's page in a system web view is a device of its person: its key, its session with the
// workspace's host, the records it keeps in step. A native app around it draws views of its own
// and asks Tilo through the page. What its own code may ask as this device, beyond what the
// page's mount publishes, is handed to it here: once, and to nobody else. The page puts no
// handle on its session on a global, and it does not start to for a native host.
//
// A native app says it is there before any of the page's code runs: its document-start script
// defines `globalThis.tiloNativeHost` with a function `device`, and with `name` where it names
// its device itself (what its person reads where they let it in: `deviceLabel` in `pairing.mjs`). A browser has none, so in a
// browser nothing is handed to anyone. The host keeps what it is given where only its own code
// reaches it.
//
// What is handed over is the device's own entry and nothing wider. Every ask goes through the
// session this page holds with its workspace's host, as the person on this device, and the host
// decides each by the device's certificate, exactly as it does for the page's own windows: an
// app, an agent and anyone else's device are answered what an unknown action is answered.

/**
 * The entry of the device `peer` is, or `null` for a page whose workspace has no host to ask:
 *
 * - `person` and `workspace`: whom this device is certified as (root key, hex) and the workspace
 *   this page is in.
 * - `catalogue(query)`: what the host offers this person (`CatalogueQuery`).
 * - `invoke(target, argument, {confirmation, invocation})`: run one action and answer what the
 *   host said of it, `{invocation, state, output?, error?}`. Nothing of the call is kept in the
 *   page: an argument may be what the person typed into a protected field.
 * - `status(invocation)`: what became of one the host accepted and still runs.
 * - `ownWorkspace()`: the person's own workspace on this host, made the first time it is asked
 *   for, with this device in it: `{id, name, origin}`.
 */
export function deviceEntry({peer, ownWorkspace}) {
  const actions = peer?.actions;
  if (!actions || typeof actions.once !== 'function') return null;
  return Object.freeze({
    person: peer.userPublicKey ?? null,
    workspace: peer.workspace ?? null,
    catalogue: query => actions.catalogue(query),
    invoke: (target, argument, options) => actions.once(target, argument, options),
    status: invocation => actions.observe(invocation),
    ownWorkspace: () => {
      if (typeof ownWorkspace !== 'function') throw new Error('unknown_action: This page cannot ask for the person\'s own workspace');
      return ownWorkspace();
    },
  });
}

/**
 * Hand this page's device entry to the native host in the document, when one does.
 * Answers whether one took it. Nothing is put where another script finds it.
 */
export function handToNativeHost({peer, ownWorkspace}, host = globalThis.tiloNativeHost) {
  if (typeof host?.device !== 'function') return false;
  const entry = deviceEntry({peer, ownWorkspace});
  if (!entry) return false;
  try {
    host.device(entry);
  } catch (error) {
    console.warn('the native host did not take this device\'s entry', error);
    return false;
  }
  return true;
}
