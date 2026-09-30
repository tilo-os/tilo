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

/**
 * Where to draw a remote pointer, in surface pixels. A pointer with a
 * `focus` is in that record's own surface (an agent points at the control
 * it drives in the record's window): it is drawn inside the window that
 * shows the record, kept within its bounds, and not at all while no window
 * here shows it. `windows` lists `{record, rect: [x0, y0, x1, y1]}`; without
 * it (a single application) every pointer is in the surface's coordinates.
 */
export function placePointer(pointer, focus, windows) {
  if (!focus || !windows) return pointer;
  const window = windows.find(candidate => candidate.record === focus.record);
  if (!window) return null;
  const [x0, y0, x1, y1] = window.rect;
  const clamp = (value, low, high) => Math.min(Math.max(value, low), Math.max(low, high));
  return [clamp(x0 + pointer[0], x0, x1 - 1), clamp(y0 + pointer[1], y0, y1 - 1)];
}

export function createLiveCursors({
  layer, surface, document: doc = globalThis.document, windows = () => undefined,
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
  }
  function render() {
    frame = undefined;
    if (closed) return;
    const at = now();
    const seen = new Set();
    const shownWindows = windows();
    for (const remote of layer.remoteState(at)) {
      if (!remote.pointer || remote.stale) continue;
      const placed = placePointer(remote.pointer, remote.focus, shownWindows);
      if (!placed) continue;
      seen.add(remote.publisher);
      const [x, y] = placed;
      marker(remote.publisher, remote.participant).style.transform = `translate(${x}px,${y}px)`;
    }
    for (const publisher of [...markers.keys()]) if (!seen.has(publisher)) drop(publisher);
    if (at - lastFrameAt < SETTLE_MS) schedule();
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
    layer.publishPresence({pointer: [event.clientX - rect.left, event.clientY - rect.top]});
  };
  const leave = () => layer.publishPresence({pointer: null});
  surface.addEventListener('pointermove', move, {passive: true});
  surface.addEventListener('pointerleave', leave, {passive: true});
  schedule();

  return {
    /** Publishers whose marker is on screen. */
    shown: () => [...markers.keys()],
    close() {
      if (closed) return;
      closed = true;
      if (frame !== undefined) cancelFrame(frame);
      for (const stop of unsubscribe) stop();
      surface.removeEventListener('pointermove', move);
      surface.removeEventListener('pointerleave', leave);
      overlay.remove();
      markers.clear();
    },
  };
}
