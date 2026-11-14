/**
 * What the browser tab says while the person is elsewhere. A tab in a row of
 * tabs shows only its title and icon, so both carry what needs the person:
 * - the title leads with the count of notices they have not seen (the desk's
 *   bell), then an agent waiting for them ("Claude Code needs you") or one
 *   that finished while they were away ("Claude Code finished"), then the
 *   workspace;
 * - the icon carries a badge while anything of that is there.
 * An agent's finishing counts as news only until the person looks at the tab
 * again: they saw it on the desk then.
 *
 * The browser's notifications themselves are the runtime's to show (it shows
 * them only while the tab is hidden); this page asks for the permission they
 * need, once, at the moment the person first has news they could have been
 * told of elsewhere, never at load. The answer is the browser's to keep; a
 * "Not now" is kept here so the offer is not made again.
 */

/** The localStorage key that keeps a "Not now". */
export const DECLINED_KEY = 'tilo.notifications.declined';

/** `names` as a list read out: "A", "A and B", "A, B and C". */
function listed(names) {
  if (names.length <= 1) return names[0] ?? '';
  return `${names.slice(0, -1).join(', ')} and ${names.at(-1)}`;
}

/** The tab's title for `workspace` and what needs the person: `unread`
 * notices, agents `waiting` for them and agents `finished` while they were
 * away. */
export function tabTitle({workspace = null, unread = 0, waiting = [], finished = []} = {}) {
  const parts = [];
  if (waiting.length) parts.push(`${listed(waiting)} ${waiting.length === 1 ? 'needs' : 'need'} you`);
  const done = finished.filter(name => !waiting.includes(name));
  if (done.length) parts.push(`${listed(done)} finished`);
  parts.push(workspace ? `Tilo · ${workspace}` : 'Tilo');
  const count = unread > 0 ? `(${unread > 99 ? '99+' : unread}) ` : '';
  return `${count}${parts.join(' · ')}`;
}

/** What the icon's badge says: the unread count (`9+` beyond nine), a mark
 * when only an agent needs the person, or nothing. */
export function badgeText({unread = 0, waiting = [], finished = []} = {}) {
  if (unread > 0) return unread > 9 ? '9+' : String(unread);
  return waiting.length || finished.length ? '!' : null;
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
 * them, nobody asked yet, the person did not say "Not now", and there is
 * news the person could have been told of elsewhere. */
export function shouldOffer({permission, declined, attention}) {
  if (permission !== 'default' || declined) return false;
  return Boolean(attention && (attention.waiting?.length || attention.finished?.length || attention.unread > 0));
}

/**
 * Title the tab and badge its icon in `window` from the desktop's attention
 * (`update`) and the workspace in front (`setWorkspace`), and offer the
 * browser's notifications once they would have helped.
 */
export function createTabAttention({window: win = globalThis, storage = safeStorage(win)} = {}) {
  const doc = win.document;
  let workspace = null;
  let attention = {unread: 0, waiting: [], finished: []};
  // Finished agents' notices the person saw: on the desk, while the tab was in view.
  const seen = new Set();
  let shownBadge;
  let offer = null;
  const iconLink = () => doc?.querySelector?.('link[rel~="icon"]') ?? null;
  const baseIcon = iconLink()?.href ?? null;
  const hidden = () => doc?.visibilityState === 'hidden';
  const finishedNames = () => {
    const names = [];
    for (const {id, agent} of attention.finished ?? []) {
      if (!seen.has(id) && !names.includes(agent)) names.push(agent);
    }
    return names;
  };
  const current = () => ({unread: attention.unread ?? 0, waiting: attention.waiting ?? [], finished: finishedNames()});

  function render() {
    const now = current();
    const title = tabTitle({workspace, ...now});
    if (doc && doc.title !== title) doc.title = title;
    const text = badgeText(now);
    if (text === shownBadge) return;
    shownBadge = text;
    const link = iconLink();
    if (!link || !baseIcon) return;
    if (!text) { link.href = baseIcon; return; }
    void badgedIcon(baseIcon, text, {document: doc, Image: win.Image}).then(url => {
      if (url && shownBadge === text) link.href = url;
    }).catch(() => {});
  }

  function look() {
    // In view, what is on the desk is seen.
    if (hidden()) return;
    for (const {id} of attention.finished ?? []) seen.add(id);
    render();
    maybeOffer();
  }

  function maybeOffer() {
    if (offer || hidden()) return;
    const permission = typeof win.Notification === 'function' ? win.Notification.permission : 'unavailable';
    let declined = false;
    try { declined = storage?.getItem(DECLINED_KEY) === '1'; } catch { /* no storage: ask */ }
    if (!shouldOffer({permission, declined, attention: {...attention, finished: attention.finished ?? []}})) return;
    offer = showOffer(doc, {
      allow: () => { offer?.remove(); void win.Notification.requestPermission?.(); },
      decline: () => { offer?.remove(); try { storage?.setItem(DECLINED_KEY, '1'); } catch { /* kept for this page only */ } },
    });
  }

  doc?.addEventListener?.('visibilitychange', look);
  return {
    setWorkspace(name) {
      if (typeof name !== 'string' || !name || name === workspace) return;
      workspace = name;
      render();
    },
    update(next) {
      attention = {unread: next?.unread ?? 0, waiting: next?.waiting ?? [], finished: next?.finished ?? []};
      if (!hidden()) for (const {id} of attention.finished) seen.add(id);
      render();
      maybeOffer();
    },
    get title() { return doc?.title; },
  };
}

function safeStorage(win) {
  try { return win.localStorage ?? null; } catch { return null; }
}

/** The offer card: what turning notifications on does, Turn on and Not now. */
function showOffer(doc, {allow, decline}) {
  if (!doc?.body) return null;
  const card = doc.createElement('div');
  card.setAttribute('role', 'dialog');
  card.setAttribute('aria-label', 'Browser notifications');
  card.dataset.tiloNotificationOffer = '';
  Object.assign(card.style, {position: 'fixed', right: '16px', bottom: '16px', zIndex: '2147483646', maxWidth: '320px',
    padding: '14px 16px', borderRadius: '12px', background: '#1d1f24', color: '#eef0f4', font: '14px system-ui,sans-serif',
    boxShadow: '0 12px 40px rgba(0,0,0,.45)', display: 'flex', flexDirection: 'column', gap: '10px'});
  const text = doc.createElement('p');
  text.textContent = 'Get a notification when an agent needs you or finishes, or someone mentions you, while Tilo is in another tab.';
  Object.assign(text.style, {margin: '0', lineHeight: '1.4'});
  const row = doc.createElement('div');
  Object.assign(row.style, {display: 'flex', gap: '8px', justifyContent: 'flex-end'});
  const button = (label, primary, act) => {
    const node = doc.createElement('button');
    node.type = 'button';
    node.textContent = label;
    Object.assign(node.style, {padding: '6px 14px', borderRadius: '8px', border: '0', font: 'inherit', cursor: 'pointer',
      background: primary ? '#4c6ef5' : '#30333a', color: '#fff'});
    node.addEventListener('click', act, {once: true});
    return node;
  };
  row.append(button('Not now', false, decline), button('Turn on', true, allow));
  card.append(text, row);
  doc.body.append(card);
  return card;
}
