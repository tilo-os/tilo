// Other members' pointers over a mounted surface, from a room's live layer
// (`peer.live`). Local pointer moves over the surface are published as the
// presence `pointer` (surface CSS pixels); remote pointers are drawn as
// labelled markers on a layer above the canvas.
//
// Frames are requested only while a remote pointer may still be moving (a
// frame arrived within the interpolation and extrapolation window), so an
// idle room costs no animation frames.

/** Keep sampling this long after the newest remote frame. */
export const SETTLE_MS = 250;

import {colorOf} from './peer.mjs';

/**
 * A marker's text: the member's label, and for an agent what it is doing
 * (`participant.activity`), so a watcher sees which control it works on.
 */
export function markerText(publisher, participant) {
  const label = participant?.label ?? publisher.slice(0, 8);
  return participant?.kind === 'agent' && participant.activity ? `${label} · ${participant.activity}` : label;
}

/**
 * Inline style of a marker. A person's is a filled tag; an agent's is the
 * same tag in its colour with a white ring and a square pointing corner, so
 * agents read as agents at a glance. Pages restyle through
 * `.tilo-live-cursor[data-kind="agent"]`.
 */
export function markerStyle(kind, color) {
  const base = `position:absolute;left:0;top:0;padding:1px 6px;font:11px system-ui;color:#fff;background:${color};white-space:nowrap;will-change:transform;transition:transform 180ms ease-out`;
  return kind === 'agent'
    ? `${base};border-radius:0 8px 8px 8px;box-shadow:0 0 0 2px #fff,0 1px 4px rgba(0,0,0,.35);font-weight:600;max-width:320px;overflow:hidden;text-overflow:ellipsis`
    : `${base};border-radius:0 8px 8px 8px`;
}

/** A sent cursor-chat message stays this long, then fades. */
export const CHAT_LINGER_MS = 5000;
/** How long a sent message takes to fade at the end of its linger. */
export const CHAT_FADE_MS = 300;
/** How long a reaction floats up from the pointer before it is gone. */
export const REACTION_MS = 1200;
/** How far a reaction rises over its life, in surface pixels. */
const REACTION_RISE_PX = 48;

/**
 * How visible a member's cursor-chat bubble is at `at` (0 hidden, 1 fully
 * shown): fully while it is being typed, then for {@link CHAT_LINGER_MS}
 * after it was sent, fading over the last {@link CHAT_FADE_MS}.
 */
export function chatOpacity(chat, sentAt, at) {
  if (!chat) return 0;
  if (!chat.sent) return chat.text ? 1 : 0;
  if (sentAt == null) return 0;
  const left = sentAt + CHAT_LINGER_MS - at;
  return left <= 0 ? 0 : Math.min(1, left / CHAT_FADE_MS);
}

/** Inline style of a cursor-chat bubble, in the member's colour. */
export function chatStyle(color) {
  return `position:absolute;left:0;top:0;max-width:240px;padding:4px 10px;font:13px/18px system-ui;color:#fff;background:${color};border-radius:4px 12px 12px 12px;white-space:pre-wrap;overflow-wrap:anywhere;box-shadow:0 2px 8px rgba(0,0,0,.25);will-change:transform,opacity`;
}

/** The desktop's own record, a member's focus while no window has it. */
const DESKTOP_APP = 'so.tilo.browser-desktop';

/** The shown window of the record `focus` names (`{app, record}`, as the
 * live layer names it: `''` for an app's room record). */
function windowOf(focus, windows) {
  return windows?.find(candidate => candidate.app === focus.app && candidate.record === focus.record);
}

/**
 * Where to draw a remote pointer, in surface pixels. A pointer with a
 * `focus` is in that record's own surface (an agent points at the control
 * it drives in the record's window): it is drawn inside the window that
 * shows the record, kept within its bounds, and not at all while no window
 * here shows it. A member focused on the desktop itself points in the
 * surface's coordinates. `windows` lists `{app, record, rect: [x0, y0, x1,
 * y1]}`; without it (a single application) every pointer is in the
 * surface's coordinates.
 */
export function placePointer(pointer, focus, windows) {
  if (!focus || !windows || focus.app === DESKTOP_APP) return pointer;
  const window = windowOf(focus, windows);
  if (!window) return null;
  const [x0, y0, x1, y1] = window.rect;
  const clamp = (value, low, high) => Math.min(Math.max(value, low), Math.max(low, high));
  return [clamp(x0 + pointer[0], x0, x1 - 1), clamp(y0 + pointer[1], y0, y1 - 1)];
}

/**
 * The local pointer as presence publishes it: inside the window of the
 * record this member is focused on, relative to that window, the way
 * {@link placePointer} reads it back; in surface pixels otherwise.
 */
export function publishedPointer(point, focus, windows) {
  const window = focus && focus.app !== DESKTOP_APP && windowOf(focus, windows);
  return window ? [point[0] - window.rect[0], point[1] - window.rect[1]] : point;
}

export function createLiveCursors({
  layer, surface, document: doc = globalThis.document, windows = () => undefined,
  /** The record this member is focused on (`{app, record}`), or `null`. */
  localFocus = () => null,
  now = () => performance.now(),
  requestFrame = callback => globalThis.requestAnimationFrame(callback),
  cancelFrame = handle => globalThis.cancelAnimationFrame(handle),
}) {
  const overlay = doc.createElement('div');
  overlay.className = 'tilo-live-cursors';
  overlay.setAttribute('aria-hidden', 'true');
  overlay.style.cssText = 'position:absolute;inset:0;pointer-events:none;overflow:hidden';
  surface.append(overlay);
  const markers = new Map();
  const bubbles = new Map();
  const reactions = new Map();
  let frame;
  let lastFrameAt = -Infinity;
  let closed = false;

  function marker(publisher, participant) {
    let node = markers.get(publisher);
    if (!node) {
      node = doc.createElement('div');
      node.className = 'tilo-live-cursor';
      node.dataset.publisher = publisher;
      overlay.append(node);
      markers.set(publisher, node);
    }
    const text = markerText(publisher, participant);
    if (node.textContent !== text) node.textContent = text;
    const kind = participant?.kind === 'agent' ? 'agent' : 'person';
    const color = participant?.color ?? colorOf(publisher);
    if (node.dataset.kind !== kind || node.dataset.color !== color) {
      node.dataset.kind = kind;
      node.dataset.color = color;
      node.style.cssText = markerStyle(kind, color);
    }
    return node;
  }
  function drop(publisher) {
    markers.get(publisher)?.remove();
    markers.delete(publisher);
    dropBubble(publisher);
    reactions.get(publisher)?.node.remove();
    reactions.delete(publisher);
  }
  function dropBubble(publisher) {
    bubbles.get(publisher)?.remove();
    bubbles.delete(publisher);
  }
  // What the member is saying, under its name tag. Returns whether it is
  // still on screen (a sent one fades on this clock, so frames continue).
  function bubble(publisher, remote, color, x, y, at) {
    const opacity = chatOpacity(remote.chat, remote.chatSentAt, at);
    if (opacity <= 0) {
      dropBubble(publisher);
      return false;
    }
    let node = bubbles.get(publisher);
    if (!node) {
      node = doc.createElement('div');
      node.className = 'tilo-live-chat';
      node.dataset.publisher = publisher;
      overlay.append(node);
      bubbles.set(publisher, node);
    }
    if (node.dataset.color !== color) {
      node.dataset.color = color;
      node.style.cssText = chatStyle(color);
    }
    if (node.textContent !== remote.chat.text) node.textContent = remote.chat.text;
    node.style.opacity = String(opacity);
    node.style.transform = `translate(${x + 12}px,${y + 22}px)`;
    return remote.chat.sent;
  }
  // The member's latest reaction, floating up from its pointer once.
  // Returns whether it is still rising.
  function reaction(publisher, remote, x, y, at) {
    const current = remote.reaction;
    const age = current ? at - current.at : Infinity;
    let shown = reactions.get(publisher);
    if (!current || age >= REACTION_MS) {
      shown?.node.remove();
      reactions.delete(publisher);
      return false;
    }
    if (!shown || shown.seq !== current.seq) {
      shown?.node.remove();
      const node = doc.createElement('div');
      node.className = 'tilo-live-reaction';
      node.style.cssText = 'position:absolute;left:0;top:0;font:28px/1 system-ui;will-change:transform,opacity';
      node.textContent = current.emoji;
      overlay.append(node);
      shown = {seq: current.seq, node};
      reactions.set(publisher, shown);
    }
    const t = Math.max(0, age) / REACTION_MS;
    // Decelerating rise; fades over the second half of its life.
    const rise = REACTION_RISE_PX * (1 - (1 - t) * (1 - t));
    shown.node.style.transform = `translate(${x - 14}px,${y - 32 - rise}px)`;
    shown.node.style.opacity = String(t < 0.5 ? 1 : (1 - t) * 2);
    return true;
  }
  function render() {
    frame = undefined;
    if (closed) return;
    const at = now();
    const seen = new Set();
    const shownWindows = windows();
    let animating = false;
    for (const remote of layer.remoteState(at)) {
      if (!remote.pointer || remote.stale) continue;
      const placed = placePointer(remote.pointer, remote.focus, shownWindows);
      if (!placed) continue;
      seen.add(remote.publisher);
      const [x, y] = placed;
      const node = marker(remote.publisher, remote.participant);
      node.style.transform = `translate(${x}px,${y}px)`;
      animating = bubble(remote.publisher, remote, node.dataset.color, x, y, at) || animating;
      animating = reaction(remote.publisher, remote, x, y, at) || animating;
    }
    for (const publisher of [...markers.keys()]) if (!seen.has(publisher)) drop(publisher);
    if (animating || at - lastFrameAt < SETTLE_MS) schedule();
  }
  function schedule() {
    if (frame === undefined && !closed) frame = requestFrame(render);
  }
  const unsubscribe = [
    layer.onPresence(() => { lastFrameAt = now(); schedule(); }),
    layer.onMember(({publisher, active}) => { if (!active) drop(publisher); schedule(); }),
  ];
  const move = event => {
    const rect = surface.getBoundingClientRect();
    const point = [event.clientX - rect.left, event.clientY - rect.top];
    layer.publishPresence({pointer: publishedPointer(point, localFocus(), windows())});
  };
  const leave = () => layer.publishPresence({pointer: null});
  surface.addEventListener('pointermove', move, {passive: true});
  surface.addEventListener('pointerleave', leave, {passive: true});
  schedule();

  return {
    /** Publishers whose marker is on screen. */
    shown: () => [...markers.keys()],
    /** Publishers whose cursor-chat bubble is on screen, with its text. */
    chats: () => [...bubbles].map(([publisher, node]) => [publisher, node.textContent]),
    close() {
      if (closed) return;
      closed = true;
      if (frame !== undefined) cancelFrame(frame);
      for (const stop of unsubscribe) stop();
      surface.removeEventListener('pointermove', move);
      surface.removeEventListener('pointerleave', leave);
      overlay.remove();
      markers.clear();
      bubbles.clear();
      reactions.clear();
    },
  };
}
