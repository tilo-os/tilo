// Other members' pointers over a mounted surface, from a room's live layer
// (`peer.live`). Local pointer moves over the surface are published as the
// presence `pointer`, relative to what is under it (`anchor`, the runtime's
// `pointer_anchor`): a point in the window showing a record, or over the
// empty desktop, beside the nearest window. Members' windows differ in size,
// each scrolls a strip on their own, and in a room whose layout is personal
// every member's strips are their own: a point in one page's surface pixels,
// or in its strip, would land somewhere else on every other page, or nowhere.
// Remote pointers are handed to the runtime as they came (`setPointers`),
// which places each in this page's layout, leaves out any over a window or
// strip this page does not show, and draws them in the frame itself: the
// shell's arrow, a name tag in its typeface that stays on screen, and the
// colour the person's avatar wears.
//
// Frames are requested only while a remote pointer may still be moving (a
// frame arrived within the interpolation and extrapolation window), or a
// chat or reaction beside it is still animating, so an idle room costs no
// animation frames. A timer takes a pointer whose publisher went silent off
// screen even when no frame arrives to notice it.

import {PRESENCE_KEEPALIVE_MS} from './live-presence.mjs';

/** Keep sampling this long after the newest remote frame. */
export const SETTLE_MS = 250;
/**
 * A pointer whose publisher sent nothing for this long is no longer shown.
 * A present member restates its presence every keepalive, so a silence past
 * one keepalive and a second means it left (its page closed, its link
 * dropped) before the room noticed, or it hid its page.
 */
export const POINTER_STALE_MS = PRESENCE_KEEPALIVE_MS + 1000;

/**
 * A marker's text: the member's label, and for an agent what it is doing
 * (`participant.activity`), so a watcher sees which control it works on.
 */
export function markerText(publisher, participant) {
  const label = participant?.label ?? publisher.slice(0, 8);
  return participant?.kind === 'agent' && participant.activity ? `${label} · ${participant.activity}` : label;
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

/**
 * A member's latest reaction at `at`: `{emoji, rise, opacity}` while it
 * floats up from the pointer (decelerating, fading over the second half of
 * its life), `null` before and after.
 */
export function reactionAt(reaction, at) {
  const age = reaction ? at - reaction.at : Infinity;
  if (!(age >= 0 && age < REACTION_MS)) return null;
  const t = age / REACTION_MS;
  return {emoji: reaction.emoji, rise: REACTION_RISE_PX * (1 - (1 - t) * (1 - t)), opacity: t < 0.5 ? 1 : (1 - t) * 2};
}

export function createLiveCursors({
  layer, surface,
  /** Hands the runtime the pointers to draw: `[{publisher, pointer, name?,
   * agent?, activity?, chat?, reaction?}]` (`remote_pointers`). */
  setPointers,
  /** This person's pointer at surface point `(x, y)`, relative to what it
   * is over (`pointer_anchor`), or `null` over nothing. */
  anchor = () => null,
  now = () => performance.now(),
  requestFrame = callback => globalThis.requestAnimationFrame(callback),
  cancelFrame = handle => globalThis.cancelAnimationFrame(handle),
  setTimer = (callback, delay) => globalThis.setTimeout(callback, delay),
  clearTimer = handle => globalThis.clearTimeout(handle),
}) {
  let frame;
  let timer;
  let lastFrameAt = -Infinity;
  let closed = false;
  let shownKey = '[]';
  let shown = [];

  function show(pointers) {
    const key = JSON.stringify(pointers);
    if (key === shownKey) return;
    shownKey = key;
    shown = pointers;
    setPointers(pointers);
  }
  function render() {
    frame = undefined;
    if (closed) return;
    const at = now();
    const pointers = [];
    let animating = false;
    // The soonest a shown pointer's publisher counts as gone.
    let staleAt = Infinity;
    for (const remote of layer.remoteState(at)) {
      if (!remote.pointer || remote.stale) continue;
      const heard = Number.isFinite(remote.receivedAtMs) ? remote.receivedAtMs : at;
      if (at - heard > POINTER_STALE_MS) continue;
      staleAt = Math.min(staleAt, heard + POINTER_STALE_MS);
      const pointer = {publisher: remote.publisher, pointer: remote.pointer};
      const {participant} = remote;
      // The member's own name and kind, for a runtime that does not know
      // them yet; it names and colours each one as the room does.
      if (participant?.label) pointer.name = participant.label;
      if (participant?.kind === 'agent') {
        pointer.agent = true;
        if (participant.activity) pointer.activity = participant.activity;
      }
      const opacity = chatOpacity(remote.chat, remote.chatSentAt, at);
      if (opacity > 0) {
        pointer.chat = {text: remote.chat.text, typing: !remote.chat.sent, opacity};
        // A sent one fades on this clock, so frames continue.
        animating ||= remote.chat.sent;
      }
      const reaction = reactionAt(remote.reaction, at);
      if (reaction) {
        pointer.reaction = reaction;
        animating = true;
      }
      pointers.push(pointer);
    }
    show(pointers);
    if (timer !== undefined) clearTimer(timer);
    timer = Number.isFinite(staleAt) ? setTimer(() => { timer = undefined; schedule(); }, Math.max(0, staleAt - at) + 1) : undefined;
    if (animating || at - lastFrameAt < SETTLE_MS) schedule();
  }
  function schedule() {
    if (frame === undefined && !closed) frame = requestFrame(render);
  }
  const unsubscribe = [
    layer.onPresence(() => { lastFrameAt = now(); schedule(); }),
    // A member who left is gone at once: its last frame is dropped with it.
    layer.onMember(() => schedule()),
  ];
  const move = event => {
    const rect = surface.getBoundingClientRect();
    layer.publishPresence({pointer: anchor(event.clientX - rect.left, event.clientY - rect.top)});
  };
  const leave = () => layer.publishPresence({pointer: null});
  surface.addEventListener('pointermove', move, {passive: true});
  surface.addEventListener('pointerleave', leave, {passive: true});
  schedule();

  return {
    /** Publishers whose pointer is handed to the runtime to place. */
    shown: () => shown.map(pointer => pointer.publisher),
    /** Publishers whose cursor-chat bubble is shown, with its text. */
    chats: () => shown.filter(pointer => pointer.chat).map(pointer => [pointer.publisher, pointer.chat.text]),
    /** Wake to hand the pointers over again. */
    wake: schedule,
    close() {
      if (closed) return;
      closed = true;
      if (frame !== undefined) cancelFrame(frame);
      if (timer !== undefined) clearTimer(timer);
      for (const stop of unsubscribe) stop();
      surface.removeEventListener('pointermove', move);
      surface.removeEventListener('pointerleave', leave);
      show([]);
    },
  };
}
