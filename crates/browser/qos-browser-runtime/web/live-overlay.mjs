/**
 * LIVE_V1 lane E: other people's live activity on the mounted desktop.
 *
 * Publishes the local pointer (in the focused window's coordinates), focus
 * record, editor caret/selection anchors and window drags through
 * `session.liveV1.publishPresence` (lane B coalesces to one frame per
 * animation frame; nothing is buffered here), and draws remote cursors with
 * name labels and remote window-drag ghosts on a separate DOM layer above the
 * WebGPU canvas. Overlay updates are compositor transforms only: they never
 * invalidate the document layout or re-render the canvas scene. Remote
 * carets/selections are handed to the runtime once per change, where the
 * presence-aware editors (Writer, Notes) draw them natively.
 *
 * Coordinates: CSS pixels relative to the canvas (the runtime's logical
 * scene space). A published pointer is relative to the top-left of the
 * focused window group (`focusRect`); a receiver adds the origin of its own
 * window showing the same record, and hides the cursor when it has none.
 * Content alignment: while the pointer is inside the focused window and that
 * window reports a primary vertical scroll offset (`focusScroll`), the
 * pointer is published in the record's content space instead: its x is
 * shifted by CONTENT_SPACE_X (a marker no window coordinate reaches) and its
 * y includes the publisher's scroll offset. A receiver subtracts its own
 * offset for that record, so the cursor stays on the same content when the
 * two people are scrolled differently, and hides it while that content is
 * scrolled out of its window.
 * With no focused record the pointer is in desktop coordinates. Window drags
 * publish the pointer displacement since the press (`position`) and the
 * group's current size (`size`), keyed by `[kind, group id]`.
 */

/** The ring of peer colours shared with `ui::presence::PEER_PALETTE`. */
export const PEER_PALETTE = Object.freeze([
  '#f97316', '#dfdc3a', '#87df3a', '#3adf4a', '#10b981', '#3acedf', '#3a84df', '#6366f1',
  '#893adf', '#df3adf', '#ec4899', '#df3a42', '#f4cd71', '#d1f471', '#89f471', '#71f4a3',
  '#71f4e4', '#71c8f4', '#7190f4', '#9071f4', '#d371f4', '#f471d3', '#f47194', '#f48971',
]);
const PINNED = {me: '#6366f1', alice: '#ec4899', bob: '#f97316', charlie: '#10b981'};
const FNV_OFFSET = 0xcbf29ce484222325n;
// Rust spells the multiplier `0x10_0000_01b3` (0x10000001b3); match it exactly.
const FNV_PRIME = 0x10000001b3n;
const MASK64 = (1n << 64n) - 1n;
const colorCache = new Map();

/** Stable colour for a member id; identical to Rust `ui::presence::user_color`. */
export function memberColor(id) {
  const key = String(id ?? '');
  const cached = colorCache.get(key);
  if (cached) return cached;
  let color = PINNED[key];
  if (!color) {
    let hash = FNV_OFFSET;
    for (const byte of new TextEncoder().encode(key)) hash = ((hash ^ BigInt(byte)) * FNV_PRIME) & MASK64;
    color = PEER_PALETTE[Number(hash % BigInt(PEER_PALETTE.length))];
  }
  if (colorCache.size > 512) colorCache.clear();
  colorCache.set(key, color);
  return color;
}

/** Black or white label text on a colour (WCAG relative luminance). */
export function labelInk(hex) {
  const channel = i => {
    const v = parseInt(hex.slice(1 + i * 2, 3 + i * 2), 16) / 255;
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  };
  const luminance = 0.2126 * channel(0) + 0.7152 * channel(1) + 0.0722 * channel(2);
  return luminance > 0.36 ? '#111111' : '#ffffff';
}

/** Label for a publisher: the member's display name, else a short device tag. */
export function memberLabel(publisher, member) {
  const name = typeof member?.name === 'string' ? member.name.trim() : '';
  if (name) return name.slice(0, 48);
  return member?.owner === true ? 'Owner' : `Guest ${String(publisher).slice(0, 4)}`;
}

/**
 * x offset marking a content-space pointer (2^19: far beyond any window
 * coordinate, and still 1/16 px precise in the wire's f32).
 */
export const CONTENT_SPACE_X = 524288;
const finiteScroll = value => (Number.isFinite(value) && value >= 0 ? value : null);

const sameRecord = (a, b) => Boolean(a && b && a.app === b.app && a.record === b.record);
const validRect = rect => Array.isArray(rect) && rect.length === 4 && rect.every(Number.isFinite) && rect[2] > 0 && rect[3] > 0;

/** The local window group showing `record`: the focused one first. */
function groupForRecord(snapshot, record) {
  if (!snapshot || !record) return null;
  if (sameRecord(snapshot.focus, record) && validRect(snapshot.focusRect)) {
    return {rect: snapshot.focusRect, scroll: finiteScroll(snapshot.focusScroll)};
  }
  for (const group of snapshot.groups ?? []) {
    if (sameRecord(group.record, record) && validRect(group.rect)) return {rect: group.rect, scroll: finiteScroll(group.scroll)};
  }
  return null;
}

/** The local window rectangle showing `record`: the focused one first. */
export function rectForRecord(snapshot, record) {
  return groupForRecord(snapshot, record)?.rect ?? null;
}

const inside = (rect, x, y) => x >= rect[0] && y >= rect[1] && x < rect[0] + rect[2] && y < rect[1] + rect[3];

/** Local pointer (canvas CSS px) -> published `{focus, pointer}`. */
export function localPointerPresence(snapshot, point) {
  const focus = snapshot?.focus ?? null;
  if (!point) return {focus, pointer: null};
  const rect = focus && validRect(snapshot.focusRect) ? snapshot.focusRect : null;
  if (focus && !rect) return {focus, pointer: null};
  const round = v => Math.round(v * 100) / 100;
  if (!rect) return {focus, pointer: [round(point[0]), round(point[1])]};
  const x = point[0] - rect[0];
  const y = point[1] - rect[1];
  const scroll = finiteScroll(snapshot.focusScroll);
  if (scroll !== null && inside(rect, point[0], point[1])) return {focus, pointer: [round(CONTENT_SPACE_X + x), round(y + scroll)]};
  return {focus, pointer: [round(x), round(y)]};
}

/** Remote pointer -> local canvas CSS px, or null when it cannot be placed here. */
export function remotePointerPosition(snapshot, remote) {
  const pointer = remote?.pointer;
  if (!Array.isArray(pointer)) return null;
  if (!remote.focus) return [pointer[0], pointer[1]];
  const group = groupForRecord(snapshot, remote.focus);
  if (!group) return null;
  const {rect} = group;
  if (pointer[0] < CONTENT_SPACE_X / 2) return [rect[0] + pointer[0], rect[1] + pointer[1]];
  // Content space: undo the publisher's scroll with ours; hide the cursor
  // while that content is scrolled out of our window.
  const x = rect[0] + pointer[0] - CONTENT_SPACE_X;
  const y = rect[1] + pointer[1] - (group.scroll ?? 0);
  return y >= rect[1] && y < rect[1] + rect[3] ? [x, y] : null;
}

const groupOrder = snapshot => (snapshot?.groups ?? []).map(group => group.id).join(',');
const groupFromObject = id => (typeof id === 'string' && id.length === 18 ? {kind: id.slice(0, 2) === '01' ? 'move' : id.slice(0, 2) === '02' ? 'resize' : null, group: id.slice(2)} : null);

/**
 * Publisher side of window drags: turns successive snapshots into the
 * Start/Move/End/Cancel lifecycle. A released reorder that changed the window
 * order ends (the durable desktop change follows); a released reorder that
 * left the order unchanged cancels. A released resize ends: widths are
 * per-device presentation, so receivers drop its ghost at once.
 */
export function createDragTracker() {
  let current = null;
  return {
    /** `undefined`: nothing to publish; otherwise the `drag` presence field. */
    update(snapshot) {
      const drag = snapshot?.drag ?? null;
      if (drag && current && current.object !== drag.object) current = null;
      if (drag) {
        const phase = current ? 'move' : 'start';
        if (!current) current = {object: drag.object, kind: drag.kind, order: groupOrder(snapshot), size: [drag.rect[2], drag.rect[3]]};
        current.position = [drag.offset[0], drag.offset[1]];
        current.size = [drag.rect[2], drag.rect[3]];
        return {object: {record: null, id: drag.object}, phase, position: current.position, size: current.size};
      }
      if (!current) return undefined;
      const ended = current;
      current = null;
      const phase = ended.kind === 'move' && groupOrder(snapshot) === ended.order ? 'cancel' : 'end';
      return {object: {record: null, id: ended.object}, phase, position: ended.position ?? [0, 0], size: ended.size};
    },
    get active() { return current !== null; },
  };
}

/**
 * Receiver side of window drags: live ghosts follow the interpolated remote
 * drag; a released reorder keeps its ghost at the drop position until the
 * durable order change lands here (the window order changes), a durable
 * change is reported, or lane B's released-drag timeout passes. Cancels and
 * released resizes disappear at once (the durable layout is unchanged).
 */
export function createGhostTracker() {
  const held = new Map(); // `${publisher}\0${object}` -> {order at release, done}
  return {
    ghosts(snapshot, remotes) {
      const out = [];
      const order = groupOrder(snapshot);
      const live = new Set();
      for (const remote of remotes) {
        const drags = [];
        if (remote.drag) drags.push(remote.drag);
        for (const ended of remote.endedDrags ?? []) drags.push(ended);
        for (const drag of drags) {
          const target = groupFromObject(drag.object?.id);
          if (!target?.kind || drag.object?.record) continue;
          const key = `${remote.publisher}\u0000${drag.object.id}`;
          const released = drag.phase === 'end' || drag.phase === 'cancel';
          if (released) {
            if (drag.phase === 'cancel' || target.kind === 'resize') continue;
            live.add(key);
            let entry = held.get(key);
            if (!entry) held.set(key, entry = {order, done: false});
            // The durable order landed (or a durable change was reported):
            // the real window is there now. Stays done while lane B still
            // reports this released drag.
            if (entry.done || entry.order !== order) { entry.done = true; continue; }
          } else {
            live.add(key);
            held.delete(key);
          }
          const group = (snapshot?.groups ?? []).find(row => row.id === target.group);
          if (!group || !validRect(group.rect)) continue;
          const [x, y, w, h] = group.rect;
          const rect = target.kind === 'move'
            ? [x + drag.position[0], y + drag.position[1], w, h]
            : [x, y, Array.isArray(drag.size) ? Math.max(24, drag.size[0]) : w, h];
          out.push({id: key, publisher: remote.publisher, kind: target.kind, rect, released});
        }
      }
      for (const key of held.keys()) if (!live.has(key)) held.delete(key);
      return out;
    },
    /** A durable change landed: release every held ghost. */
    snap() { for (const entry of held.values()) entry.done = true; },
    get held() { return held.size; },
  };
}

/**
 * A pending desktop command as a released window drag. In the browser desktop
 * windows are reordered by command ("Move group left/right"), not by pointer:
 * publishing the destination at submit time lets every other member preview
 * the move one network hop later instead of after the durable commit. Ids are
 * u64 and parsed textually (they may exceed the JavaScript safe range).
 */
export function commandDrag(snapshot, pending) {
  // An optimistic local reorder has already moved the group on this screen;
  // the runtime measured the displacement before the move.
  const preview = pending?.preview;
  if (preview && typeof preview.group === 'string' && /^\d+$/.test(preview.group) && [preview.dx, preview.width, preview.height].every(Number.isFinite)) {
    if (Math.abs(preview.dx) < 1 || preview.width <= 0 || preview.height <= 0) return null;
    const group = BigInt(preview.group).toString(16).padStart(16, '0');
    return {object: {record: null, id: `01${group}`}, phase: 'end', position: [preview.dx, 0], size: [preview.width, preview.height]};
  }
  const text = typeof pending?.command === 'string' ? pending.command : '';
  const match = /"ReorderGroup":\{"strip":(\d+),"group":(\d+),"before":(null|\d+)\}/.exec(text);
  if (!match || !snapshot) return null;
  const id = value => BigInt(value).toString(16).padStart(16, '0');
  const group = id(match[2]);
  const groups = snapshot.groups ?? [];
  const index = groups.findIndex(row => row.id === group);
  if (index < 0 || !validRect(groups[index].rect)) return null;
  const own = groups[index].rect;
  let targetX;
  if (match[3] === 'null') {
    const last = groups.filter(row => row.id !== group && validRect(row.rect)).at(-1);
    if (!last) return null;
    targetX = last.rect[0] + last.rect[2] - own[2];
  } else {
    const before = groups.find(row => row.id === id(match[3]));
    if (!before || !validRect(before.rect)) return null;
    targetX = before.rect[0] > own[0] ? before.rect[0] - own[2] : before.rect[0];
  }
  const dx = Math.round((targetX - own[0]) * 100) / 100;
  if (Math.abs(dx) < 1) return null;
  return {object: {record: null, id: `01${group}`}, phase: 'end', position: [dx, 0], size: [own[2], own[3]]};
}

/** In-flight typing shown beside a remote caret: at most this many chars. */
export const TYPING_MAX_CHARS = 64;
/**
 * A preview older than this is assumed landed (or lost) and dropped. Changes
 * that reach this page by durable replay only (not forwarded as live signed
 * changes) have no per-record landing signal here; the durable path takes
 * ~150-350 ms on LAN..avg-4g (tab-to-tab `writer_key_to_remote`).
 */
export const TYPING_EXPIRE_MS = 600;
/**
 * A forwarded signed change from the typist covers what it typed before the
 * change was committed; chunks previewed within this window before it
 * arrived are kept (typed after that commit).
 */
export const TYPING_LANDED_MARGIN_MS = 60;

/**
 * Remote in-flight typing from `OpsFrame::Preview`, per publisher and
 * record, until the forwarded/durable change lands (de-duplicated by the
 * typist's next signed change for that record) or TYPING_EXPIRE_MS passes.
 */
export function createTypingTracker() {
  const pending = new Map(); // `${publisher}\0${app}\0${record}` -> [{text, at}]
  const key = (publisher, record) => `${publisher}\u0000${record?.app}\u0000${record?.record}`;
  return {
    preview({publisher, record, ops}, at) {
      const id = key(publisher, record);
      const chunks = pending.get(id) ?? [];
      for (const op of Array.isArray(ops) ? ops : []) {
        if (op?.kind === 'insert' && typeof op.text === 'string' && op.text) chunks.push({text: op.text, at});
        else if (op?.kind === 'delete' && Number.isInteger(op.len) && op.len > 0) {
          let remove = op.len;
          while (remove > 0 && chunks.length) {
            const last = chunks.at(-1);
            const chars = [...last.text];
            const cut = Math.min(remove, chars.length);
            last.text = chars.slice(0, chars.length - cut).join('');
            remove -= cut;
            if (!last.text) chunks.pop();
          }
        }
      }
      if (chunks.length) pending.set(id, chunks); else pending.delete(id);
    },
    landed({publisher, record}, at) {
      const id = key(publisher, record);
      const chunks = (pending.get(id) ?? []).filter(chunk => at - chunk.at < TYPING_LANDED_MARGIN_MS);
      if (chunks.length) pending.set(id, chunks); else pending.delete(id);
    },
    /** Text typed by `publisher` into `record` that has not landed, or null. */
    text(publisher, record, at) {
      const id = key(publisher, record);
      const chunks = pending.get(id);
      if (!chunks) return null;
      const live = chunks.filter(chunk => at - chunk.at < TYPING_EXPIRE_MS);
      if (!live.length) { pending.delete(id); return null; }
      if (live.length !== chunks.length) pending.set(id, live);
      const text = live.map(chunk => chunk.text).join('');
      return [...text].slice(-TYPING_MAX_CHARS).join('');
    },
    get size() { return pending.size; },
    clear() { pending.clear(); },
  };
}

/** Selection rows for the runtime; `null` when unchanged since the last call. */
export function createSelectionProjector() {
  let last = '';
  return {
    rows(remotes, identity, typing = () => null) {
      const rows = remotes.map(remote => {
        const row = {
          publisher: remote.publisher,
          name: identity(remote.publisher).label,
          color: identity(remote.publisher).color,
          selection: remote.stale ? null : remote.selection,
        };
        const text = row.selection ? typing(remote) : null;
        if (text) row.typing = text;
        return row;
      }).filter(row => row.selection);
      const key = JSON.stringify(rows);
      if (key === last) return null;
      last = key;
      return rows;
    },
    reset() { last = ''; },
  };
}

const CURSOR_SVG = 'M1 1 L1 17 L5.5 12.8 L8.6 19.6 L11.4 18.3 L8.4 11.7 L14.4 11.4 Z';

/**
 * DOM layer: one absolutely positioned element per remote cursor and ghost,
 * moved by `transform` only (compositor-only; no layout of the page).
 * Writes happen only when a value changed.
 */
export function createOverlayView(root, doc = globalThis.document) {
  const layer = doc.createElement('div');
  layer.className = 'qos-live-overlay';
  layer.setAttribute('aria-hidden', 'true');
  layer.style.cssText = 'position:absolute;inset:0;pointer-events:none;overflow:hidden;contain:strict;z-index:2';
  root.append(layer);
  const cursors = new Map();
  const ghosts = new Map();
  const stats = {writes: 0};
  function cursorNode(id) {
    let entry = cursors.get(id);
    if (entry) return entry;
    const node = doc.createElement('div');
    node.style.cssText = 'position:absolute;left:0;top:0;will-change:transform;display:none';
    const ns = 'http://www.w3.org/2000/svg';
    const svg = doc.createElementNS(ns, 'svg');
    svg.setAttribute('width', '16');
    svg.setAttribute('height', '21');
    svg.setAttribute('viewBox', '0 0 16 21');
    svg.style.cssText = 'display:block;overflow:visible';
    const path = doc.createElementNS(ns, 'path');
    path.setAttribute('d', CURSOR_SVG);
    path.setAttribute('stroke', '#ffffff');
    path.setAttribute('stroke-width', '1.2');
    path.setAttribute('stroke-linejoin', 'round');
    svg.append(path);
    const label = doc.createElement('span');
    label.style.cssText = 'position:absolute;left:14px;top:16px;padding:1px 6px;border-radius:6px;font:600 11px/16px system-ui,sans-serif;white-space:nowrap;box-shadow:0 1px 2px rgba(0,0,0,.25)';
    node.append(svg, label);
    layer.append(node);
    entry = {node, path, label, x: NaN, y: NaN, color: '', text: '', visible: false};
    cursors.set(id, entry);
    return entry;
  }
  function ghostNode(id) {
    let entry = ghosts.get(id);
    if (entry) return entry;
    const node = doc.createElement('div');
    node.style.cssText = 'position:absolute;left:0;top:0;box-sizing:border-box;border:2px solid;border-radius:10px;will-change:transform;transform-origin:0 0';
    const label = doc.createElement('span');
    label.style.cssText = 'position:absolute;left:6px;top:-20px;padding:1px 6px;border-radius:6px;font:600 11px/16px system-ui,sans-serif;white-space:nowrap';
    node.append(label);
    layer.append(node);
    entry = {node, label, rect: '', color: '', text: '', released: null};
    ghosts.set(id, entry);
    return entry;
  }
  return {
    layer,
    stats,
    /** Apply one frame: `cursors` [{id,x,y,color,label}] (absent = hidden), `ghosts` [{id,rect,color,label,released}]. */
    render(frame) {
      const seen = new Set();
      for (const cursor of frame.cursors) {
        seen.add(cursor.id);
        const entry = cursorNode(cursor.id);
        if (!entry.visible) { entry.node.style.display = 'block'; entry.visible = true; stats.writes++; }
        if (Math.abs(entry.x - cursor.x) > 0.05 || Math.abs(entry.y - cursor.y) > 0.05 || !Number.isFinite(entry.x)) {
          entry.x = cursor.x; entry.y = cursor.y;
          entry.node.style.transform = `translate3d(${cursor.x.toFixed(1)}px,${cursor.y.toFixed(1)}px,0)`;
          stats.writes++;
        }
        if (entry.color !== cursor.color) {
          entry.color = cursor.color;
          entry.path.setAttribute('fill', cursor.color);
          entry.label.style.background = cursor.color;
          entry.label.style.color = labelInk(cursor.color);
          stats.writes++;
        }
        if (entry.text !== cursor.label) { entry.text = cursor.label; entry.label.textContent = cursor.label; stats.writes++; }
      }
      for (const [id, entry] of cursors) {
        if (seen.has(id)) continue;
        if (frame.members && !frame.members.has(id)) { entry.node.remove(); cursors.delete(id); continue; }
        if (entry.visible) { entry.node.style.display = 'none'; entry.visible = false; stats.writes++; }
      }
      const seenGhosts = new Set();
      for (const ghost of frame.ghosts) {
        seenGhosts.add(ghost.id);
        const entry = ghostNode(ghost.id);
        const [x, y, w, h] = ghost.rect;
        const rect = `${x.toFixed(1)},${y.toFixed(1)},${w.toFixed(1)},${h.toFixed(1)}`;
        if (entry.rect !== rect) {
          entry.rect = rect;
          entry.node.style.width = `${w.toFixed(1)}px`;
          entry.node.style.height = `${h.toFixed(1)}px`;
          entry.node.style.transform = `translate3d(${x.toFixed(1)}px,${y.toFixed(1)}px,0)`;
          stats.writes++;
        }
        if (entry.color !== ghost.color) {
          entry.color = ghost.color;
          entry.node.style.borderColor = ghost.color;
          entry.node.style.background = `${ghost.color}14`;
          entry.label.style.background = ghost.color;
          entry.label.style.color = labelInk(ghost.color);
          stats.writes++;
        }
        if (entry.released !== ghost.released) { entry.released = ghost.released; entry.node.style.borderStyle = ghost.released ? 'dashed' : 'solid'; stats.writes++; }
        if (entry.text !== ghost.label) { entry.text = ghost.label; entry.label.textContent = ghost.label; stats.writes++; }
      }
      for (const [id, entry] of ghosts) if (!seenGhosts.has(id)) { entry.node.remove(); ghosts.delete(id); stats.writes++; }
    },
    get cursorCount() { let n = 0; for (const entry of cursors.values()) if (entry.visible) n++; return n; },
    get ghostCount() { return ghosts.size; },
    close() { layer.remove(); cursors.clear(); ghosts.clear(); },
  };
}

/** Interpolation delay (<= 34 ms) + extrapolation (<= 80 ms) + margin. */
const SETTLE_MS = 250;

function defaultRequestFrame(callback) {
  if (typeof globalThis.requestAnimationFrame === 'function' && globalThis.document?.visibilityState !== 'hidden') {
    return {raf: globalThis.requestAnimationFrame(callback)};
  }
  return {timer: globalThis.setTimeout(() => callback(globalThis.performance?.now?.() ?? Date.now()), 1000 / 60)};
}
function defaultCancelFrame(handle) {
  if (handle?.raf !== undefined) globalThis.cancelAnimationFrame(handle.raf);
  else if (handle?.timer !== undefined) globalThis.clearTimeout(handle.timer);
}

/**
 * Bind the live layer to a mounted desktop surface.
 *
 * - `live()`: current `session.liveV1` or undefined (the live path is optional).
 * - `snapshot()`: the runtime's `live_view_snapshot` JSON (or null).
 * - `applyRemote(rows)`: hand remote carets/selections to the runtime.
 * - `members()`: `[{device, user, name}]` for labels and colours.
 * - `presented()`: a value that changes whenever the runtime presents a frame.
 */
export function createLiveOverlay({
  surface,
  canvas,
  live,
  snapshot,
  applyRemote = () => false,
  members = () => [],
  presented = () => surface?.dataset?.frames,
  document: doc = globalThis.document,
  now = () => performance.now(),
  requestFrame = defaultRequestFrame,
  cancelFrame = defaultCancelFrame,
  setInterval: every = (callback, ms) => globalThis.setInterval(callback, ms),
  clearInterval: stopEvery = handle => globalThis.clearInterval(handle),
  onError = () => {},
}) {
  const view = createOverlayView(surface, doc);
  const drags = createDragTracker();
  const ghosts = createGhostTracker();
  const selections = createSelectionProjector();
  const typing = createTypingTracker();
  const listeners = new AbortController();
  const stats = {frames: 0, publishes: 0, snapshots: 0, selectionApplies: 0, maxFrameMs: 0, totalFrameMs: 0};
  let api;
  let unsubscribe = [];
  let frame;
  let closed = false;
  let pointer = null; // canvas CSS px
  let inputDirty = false; // something to publish
  let stateDirty = true; // focus/selection/drag may have changed: re-read the snapshot
  let lastPresented;
  let current = null; // parsed snapshot
  let memberRows = new Map();
  let membersAt = -Infinity;
  let membersDirty = true;
  let lastRemoteCount = 0;
  let canvasRect = null;
  const report = error => { try { onError(error); } catch {} };

  function identity(publisher) {
    const member = memberRows.get(publisher);
    return {label: memberLabel(publisher, member), color: memberColor(member?.user ?? publisher)};
  }
  function refreshMembers(force) {
    const t = now();
    if (!force && !membersDirty) return;
    if (!force && t - membersAt < 2000) return;
    membersAt = t;
    membersDirty = false;
    try {
      const rows = members();
      const next = new Map();
      for (const row of Array.isArray(rows) ? rows : []) if (row && typeof row.device === 'string') next.set(row.device, row);
      memberRows = next;
      selections.reset();
    } catch (error) { report(error); }
  }
  function readSnapshot() {
    try {
      const raw = snapshot();
      current = raw ? (typeof raw === 'string' ? JSON.parse(raw) : raw) : null;
      stats.snapshots++;
    } catch (error) { current = null; report(error); }
  }
  function schedule() {
    if (closed || frame !== undefined) return;
    frame = requestFrame(tick);
  }
  function attach() {
    const next = live();
    if (next === api) return;
    for (const off of unsubscribe) { try { off(); } catch {} }
    unsubscribe = [];
    api = next;
    if (!api) return;
    const wake = () => schedule();
    unsubscribe.push(api.onRemotePresence(wake));
    unsubscribe.push(api.onMember(() => { membersDirty = true; membersAt = -Infinity; schedule(); }));
    unsubscribe.push(api.onState(() => { inputDirty = true; stateDirty = true; schedule(); }));
    unsubscribe.push(api.onDurable(() => { ghosts.snap(); schedule(); }));
    if (typeof api.onPreview === 'function') unsubscribe.push(api.onPreview(value => { typing.preview(value, now()); schedule(); }));
    if (typeof api.onSignedChange === 'function') unsubscribe.push(api.onSignedChange(value => { typing.landed(value, now()); schedule(); }));
    inputDirty = true;
    stateDirty = true;
    schedule();
  }
  function publish() {
    if (!api?.available) return;
    const presence = localPointerPresence(current, pointer);
    const drag = drags.update(current);
    const partial = {focus: presence.focus, pointer: presence.pointer, selection: current?.selection ?? null};
    if (drag !== undefined) partial.drag = drag;
    try { if (api.publishPresence(partial)) stats.publishes++; } catch (error) { report(error); }
  }
  function tick(timestamp) {
    frame = undefined;
    if (closed) return;
    const started = now();
    attach();
    const remotes = api?.available ? api.remoteState(started) : [];
    const frameValue = presented();
    const layoutMoved = frameValue !== lastPresented;
    // Pointer motion alone reuses the snapshot until the runtime presents a
    // frame (windows may have moved); keys, clicks and focus re-read it.
    if (!current || stateDirty || drags.active || (layoutMoved && (inputDirty || remotes.length || lastRemoteCount))) {
      lastPresented = frameValue;
      stateDirty = false;
      readSnapshot();
    }
    if (inputDirty || drags.active) publish();
    inputDirty = false;
    if (remotes.some(remote => !memberRows.has(remote.publisher))) membersDirty = true;
    refreshMembers(false);
    const cursors = [];
    for (const remote of remotes) {
      if (remote.stale) continue;
      const position = remotePointerPosition(current, remote);
      if (!position) continue;
      const who = identity(remote.publisher);
      cursors.push({id: remote.publisher, x: position[0], y: position[1], color: who.color, label: who.label});
    }
    const ghostRows = ghosts.ghosts(current, remotes).map(ghost => {
      const who = identity(ghost.publisher);
      return {...ghost, color: who.color, label: who.label};
    });
    view.render({cursors, ghosts: ghostRows, members: new Set(remotes.map(remote => remote.publisher))});
    const rows = selections.rows(remotes, identity, remote => typing.text(remote.publisher, remote.focus, started));
    if (rows) {
      try { applyRemote(rows); stats.selectionApplies++; } catch (error) { report(error); }
    }
    lastRemoteCount = remotes.length;
    const spent = now() - started;
    stats.frames++;
    stats.totalFrameMs += spent;
    if (spent > stats.maxFrameMs) stats.maxFrameMs = spent;
    // Keep animating while remote presence can still move (interpolation and
    // bounded extrapolation settle within a few frames of the last datagram),
    // a ghost is shown or a local drag is in flight; otherwise sleep until a
    // datagram, local input or a presented runtime frame wakes the loop.
    if (remotes.some(remote => started - remote.receivedAtMs < SETTLE_MS) || drags.active || ghostRows.length || typing.size) schedule();
  }
  // ---- Local input ----------------------------------------------------------
  const on = (node, type, callback, options = {}) => node?.addEventListener?.(type, callback, {...options, signal: listeners.signal});
  const rect = () => (canvasRect ??= canvas.getBoundingClientRect());
  const invalidateRect = () => { canvasRect = null; };
  on(surface, 'pointermove', event => {
    const box = rect();
    pointer = [event.clientX - box.left, event.clientY - box.top];
    // Pointer events arrive at display rate: publish now (the publisher
    // coalesces within this task and rate-limits to 120/s) instead of
    // waiting for the next animation frame. The frame tick re-reads the
    // snapshot and republishes if windows moved under the pointer.
    if (current && api?.available && !drags.active) {
      const presence = localPointerPresence(current, pointer);
      try { if (api.publishPresence({focus: presence.focus, pointer: presence.pointer})) stats.publishes++; } catch (error) { report(error); }
    }
    inputDirty = true;
    schedule();
  }, {capture: true, passive: true});
  on(surface, 'pointerleave', () => { pointer = null; inputDirty = true; schedule(); }, {passive: true});
  // In-flight typing preview: what the person types into the focused record
  // leaves at once as `OpsFrame::Preview`, so collaborators see it beside
  // the caret one hop later, before the durable change arrives.
  function previewTyped(op) {
    const focus = current?.focus;
    const at = current?.selection?.anchor;
    if (!focus || !at || !api?.available || typeof api.publishPreview !== 'function') return;
    try { if (api.publishPreview(focus, [{...op, at}])) stats.previews = (stats.previews ?? 0) + 1; } catch (error) { report(error); }
  }
  // Keys are read at keydown (the host may prevent the textarea's default
  // input, so no `input` event follows); composed text arrives once, at
  // compositionend.
  on(surface, 'keydown', event => {
    if (event.isComposing || event.ctrlKey || event.metaKey || event.altKey) return;
    if (event.key === 'Backspace') previewTyped({kind: 'delete', len: 1});
    else if (event.key === 'Enter') previewTyped({kind: 'insert', text: '\n'});
    else if (typeof event.key === 'string' && [...event.key].length === 1) previewTyped({kind: 'insert', text: event.key});
  }, {capture: true, passive: true});
  on(surface, 'compositionend', event => {
    if (typeof event.data === 'string' && event.data) previewTyped({kind: 'insert', text: event.data});
  }, {capture: true, passive: true});
  for (const type of ['pointerdown', 'pointerup', 'keydown', 'keyup', 'input', 'compositionend', 'focusin', 'focusout', 'wheel']) {
    on(surface, type, () => { inputDirty = true; stateDirty = true; schedule(); }, {capture: true, passive: true});
  }
  on(globalThis, 'resize', invalidateRect, {passive: true});
  on(globalThis, 'scroll', invalidateRect, {passive: true, capture: true});
  const observer = typeof globalThis.ResizeObserver === 'function' ? new globalThis.ResizeObserver(invalidateRect) : null;
  observer?.observe(canvas);
  // The live client may appear after the desktop mounts (grant, reconnect).
  const poll = every(() => { attach(); if (api && (inputDirty || lastRemoteCount)) schedule(); }, 1000);
  attach();

  return {
    view,
    stats: () => ({...stats, cursors: view.cursorCount, ghosts: view.ghostCount, members: memberRows.size, attached: Boolean(api), meanFrameMs: stats.frames ? stats.totalFrameMs / stats.frames : 0}),
    /** Frame hook: the runtime presented (windows may have moved under remote cursors). */
    wake() { if (lastRemoteCount) schedule(); },
    /** A desktop command entered submission: preview a window reorder at once. */
    commandPending(pending) {
      if (!api?.available) return;
      readSnapshot();
      const drag = commandDrag(current, pending);
      if (!drag) return;
      try { if (api.publishPresence({drag})) stats.publishes++; } catch (error) { report(error); }
    },
    /** A durable change landed (e.g. the desktop order): release held ghosts. */
    durable() { ghosts.snap(); schedule(); },
    close() {
      if (closed) return;
      closed = true;
      listeners.abort();
      observer?.disconnect();
      stopEvery(poll);
      if (frame !== undefined) cancelFrame(frame);
      for (const off of unsubscribe) { try { off(); } catch {} }
      try { if (api?.available) api.publishPresence({pointer: null, selection: null, drag: null}); } catch {}
      try { applyRemote([]); } catch {}
      view.close();
    },
  };
}
