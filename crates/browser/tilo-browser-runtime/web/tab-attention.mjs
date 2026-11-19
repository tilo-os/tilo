/**
 * What the browser tab says while the person is elsewhere. A tab in a row of
 * tabs shows only its title and icon, so both carry what needs the person:
 * - the title leads with the count of notices they have not seen (the shell's
 *   bell), then an agent waiting for them ("Claude Code needs you"), then the
 *   workspace;
 * - the icon carries a badge while anything of that is there.
 *
 * The browser's notifications themselves are the runtime's to show (it shows
 * them only while the tab is hidden); this page asks for the permission they
 * need, once, at the moment the person first has news they could have been
 * told of elsewhere, never at load. It asks where the shell asks everything:
 * a notification on the shell's own stack, with Turn on and Not now, which
 * waits in the notification centre when put off. The browser keeps the
 * answer; the page keeps that it asked, so it never asks twice.
 */

/** The localStorage key that keeps that the page asked. */
export const OFFERED_KEY = 'tilo.notifications.offered';

/** What the offer's Turn on asks of the page. */
export const TURN_ON_ACTION = 'browser-notifications-turn-on';

/** `names` as a list read out: "A", "A and B", "A, B and C". */
function listed(names) {
  if (names.length <= 1) return names[0] ?? '';
  return `${names.slice(0, -1).join(', ')} and ${names.at(-1)}`;
}

/** The tab's title for `workspace` and what needs the person: `unread`
 * notices and agents `waiting` for them. */
export function tabTitle({workspace = null, unread = 0, waiting = []} = {}) {
  const parts = [];
  if (waiting.length) parts.push(`${listed(waiting)} ${waiting.length === 1 ? 'needs' : 'need'} you`);
  parts.push(workspace ? `Tilo · ${workspace}` : 'Tilo');
  const count = unread > 0 ? `(${unread > 99 ? '99+' : unread}) ` : '';
  return `${count}${parts.join(' · ')}`;
}

/** What the icon's badge says: the unread count (`9+` beyond nine), a mark
 * when only an agent needs the person, or nothing. */
export function badgeText({unread = 0, waiting = []} = {}) {
  if (unread > 0) return unread > 9 ? '9+' : String(unread);
  return waiting.length ? '!' : null;
}

/** The icon with `text` badged on its corner, as a PNG data URL drawn with
 * `document`'s canvas; `null` where the page cannot draw one. */
export async function badgedIcon(iconUrl, text, {document: doc = globalThis.document, Image: ImageType = globalThis.Image} = {}) {
  const canvas = doc?.createElement?.('canvas');
  const context = canvas?.getContext?.('2d');
  if (!context || typeof ImageType !== 'function') return null;
  const size = 64;
  canvas.width = size;
  canvas.height = size;
  const icon = new ImageType();
  await new Promise((resolve, reject) => { icon.onload = resolve; icon.onerror = reject; icon.src = iconUrl; });
  context.drawImage(icon, 0, 0, size, size);
  const radius = text.length > 1 ? 20 : 17;
  context.fillStyle = '#e5484d';
  context.beginPath();
  context.arc(size - radius, radius, radius, 0, Math.PI * 2);
  context.fill();
  context.fillStyle = '#ffffff';
  context.font = `bold ${text.length > 1 ? 24 : 28}px system-ui, sans-serif`;
  context.textAlign = 'center';
  context.textBaseline = 'middle';
  context.fillText(text, size - radius, radius + 1);
  return canvas.toDataURL('image/png');
}

/** Whether to offer the browser's notifications now: the browser can show
 * them, the page never asked, and there is news the person could have been
 * told of elsewhere. */
export function shouldOffer({permission, offered, attention}) {
  if (permission !== 'default' || offered) return false;
  return Boolean(attention && (attention.waiting?.length || attention.unread > 0));
}

/**
 * Title the tab and badge its icon in `window` from the desktop's attention
 * (`update`) and the workspace in front (`setWorkspace`), and offer the
 * browser's notifications on the shell (`attach` its mount) once they would
 * have helped; the shell hands the offer's Turn on back to the page
 * (`handles`, `perform`).
 */
export function createTabAttention({window: win = globalThis, storage = safeStorage(win)} = {}) {
  const doc = win.document;
  let workspace = null;
  let attention = {unread: 0, waiting: []};
  let shownBadge;
  let mount = null;
  const iconLink = () => doc?.querySelector?.('link[rel~="icon"]') ?? null;
  const baseIcon = iconLink()?.href ?? null;
  const hidden = () => doc?.visibilityState === 'hidden';

  function render() {
    const title = tabTitle({workspace, ...attention});
    if (doc && doc.title !== title) doc.title = title;
    const text = badgeText(attention);
    if (text === shownBadge) return;
    shownBadge = text;
    const link = iconLink();
    if (!link || !baseIcon) return;
    if (!text) { link.href = baseIcon; return; }
    void badgedIcon(baseIcon, text, {document: doc, Image: win.Image}).then(url => {
      if (url && shownBadge === text) link.href = url;
    }).catch(() => {});
  }

  function maybeOffer() {
    if (!mount?.offerHostAction || hidden()) return;
    const permission = typeof win.Notification === 'function' ? win.Notification.permission : 'unavailable';
    let offered = false;
    try { offered = storage?.getItem(OFFERED_KEY) === '1'; } catch { /* no storage: ask */ }
    if (!shouldOffer({permission, offered, attention})) return;
    try { storage?.setItem(OFFERED_KEY, '1'); } catch { /* asked on this page only */ }
    mount.offerHostAction({
      action: {id: TURN_ON_ACTION, title: 'Turn on'},
      title: 'Get notified when an agent needs you',
      body: 'Also when someone mentions you, with this tab in the background',
    });
  }

  doc?.addEventListener?.('visibilitychange', () => { render(); maybeOffer(); });
  return {
    setWorkspace(name) {
      if (typeof name !== 'string' || !name || name === workspace) return;
      workspace = name;
      render();
    },
    update(next) {
      attention = {unread: next?.unread ?? 0, waiting: next?.waiting ?? []};
      render();
      maybeOffer();
    },
    attach(shell) {
      mount = shell ?? null;
      maybeOffer();
    },
    handles: id => id === TURN_ON_ACTION,
    perform() {
      // The person's press on the shell's Turn on is still the gesture the browser asks inside.
      void Promise.resolve(win.Notification?.requestPermission?.()).then(answer => {
        // The page subscribes to pushes now (web-push.mjs).
        if (answer === 'granted') win.dispatchEvent?.(new win.Event('tilo-notifications-allowed'));
      }).catch(() => {});
    },
    get title() { return doc?.title; },
  };
}

function safeStorage(win) {
  try { return win.localStorage ?? null; } catch { return null; }
}
