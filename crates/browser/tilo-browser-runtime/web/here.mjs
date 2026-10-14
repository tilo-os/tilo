// Who is here in the desktop's room and what they are doing.
//
// The room's live layer says who is present now: each member's publisher,
// name, colour, and for an agent the record it works in and what it is
// doing. Every few seconds the page asks the runtime what its own person is
// doing (`desktop_here_tick`: the window they focus and the titles of their
// own recent steps, never content) and posts it to the host's `/here`
// (`peer.here`), which answers with everyone's one-line status and recent
// summary. A host's room service also counts its spectators. All of it goes
// to the runtime as ambient presence; none of it is a notification.

/** How often the page checks what its person is doing. */
export const TICK_MS = 4_000;
/** The longest a page goes without posting, so the host keeps it listed. */
export const POST_EVERY_MS = 20_000;
/** How far back a person's own steps count as what they are doing. */
export const RECENT_MS = 10 * 60_000;
/** How often the spectator count is read; the service caches it 5 s. */
export const WATCHING_EVERY_MS = 15_000;

/**
 * The members of `layer` as the runtime reads them (`desktop_set_people`):
 * `null` while the layer is not live.
 */
export function liveMembers(layer, seen) {
  if (!layer?.available) return null;
  return layer.members().map(publisher => {
    const {participant, focus, place} = seen.get(publisher) ?? {};
    return {
      publisher,
      kind: participant?.kind === 'agent' ? 'agent' : 'person',
      label: participant?.label ?? '',
      color: participant?.color ?? null,
      focus: participant?.kind === 'agent' ? focus ?? null : null,
      activity: participant?.activity ?? null,
      // The strip a person shows, for the bar's counts.
      place: participant?.kind === 'agent' ? null : place ?? null,
    };
  });
}

/**
 * Keep the runtime's view of who is here current while `active()`.
 *
 * @param options.runtime      the page runtime
 * @param options.mountId      the desktop mount's id
 * @param options.active       whether the mount is still shown
 * @param options.layer        the room's live layer, or undefined
 * @param options.peer         the certified peer: `here(window, actions)`, `participant`
 * @param options.presenceUrl  the room service's spectator count, if any
 */
export function createHere({runtime, mountId, active, layer, peer, presenceUrl,
  fetch = globalThis.fetch, now = () => Date.now(), setTimer = setTimeout, clearTimer = clearTimeout,
  visible = () => globalThis.document?.visibilityState !== 'hidden', onError = () => {}}) {
  const seen = new Map(); // publisher -> {participant, focus, place}
  let stopped = false;
  let timer;
  let peopleKey;
  let postedKey;
  let postedAt = -Infinity;
  let posting = false;
  let watchedAt = -Infinity;

  function pushPeople() {
    if (stopped || !active()) return;
    const live = layer();
    const own = peer?.participant ? {publisher: live?.publisherId ?? '', kind: 'person', ...peer.participant} : null;
    const json = JSON.stringify({own, members: liveMembers(live, seen)});
    if (json === peopleKey) return;
    peopleKey = json;
    runtime.desktop_set_people(mountId(), json);
  }
  function remember({publisher, frame}) {
    const previous = seen.get(publisher);
    seen.set(publisher, {participant: frame.participant ?? previous?.participant ?? null, focus: frame.focus ?? null, place: frame.place ?? null});
    pushPeople();
  }
  const live = layer();
  const stops = live ? [
    live.onPresence(remember),
    live.onMember(({publisher, active: present}) => { if (!present) seen.delete(publisher); pushPeople(); }),
    live.onState?.(() => pushPeople()) ?? (() => {}),
  ] : [];
  for (const remote of live?.remoteState?.() ?? []) remember({publisher: remote.publisher, frame: remote});

  async function post(state, at) {
    posting = true;
    postedAt = at;
    try {
      const lines = await peer.here(state.window, state.actions);
      if (!stopped && active()) runtime.desktop_set_here(mountId(), JSON.stringify(lines));
    } finally {
      posting = false;
    }
  }
  async function watch(at) {
    watchedAt = at;
    const response = await fetch(presenceUrl, {credentials: 'omit'});
    if (!response.ok) return;
    const {watching} = await response.json();
    if (!stopped && active() && Number.isInteger(watching) && watching >= 0) runtime.desktop_set_watching(mountId(), watching);
  }
  async function tick() {
    timer = undefined;
    if (stopped) return;
    try {
      pushPeople();
      if (active() && visible()) {
        const at = now();
        const state = JSON.parse(runtime.desktop_here_tick(mountId(), at - RECENT_MS));
        const key = JSON.stringify([state.window, state.actions.map(action => action.id)]);
        if (typeof peer?.here === 'function' && !posting && (key !== postedKey || at - postedAt >= POST_EVERY_MS)) {
          postedKey = key;
          await post(state, at);
        }
        if (presenceUrl && at - watchedAt >= WATCHING_EVERY_MS) await watch(at);
      }
    } catch (error) {
      onError(error);
    }
    if (!stopped) timer = setTimer(tick, TICK_MS);
  }
  void tick();

  return {
    /** Check now, e.g. after the person acted. */
    wake() { if (!stopped && !posting && timer !== undefined) { clearTimer(timer); void tick(); } },
    stop() {
      stopped = true;
      if (timer !== undefined) clearTimer(timer);
      for (const stop of stops) { try { stop(); } catch {} }
    },
  };
}
