// Who is here in the desktop's room and what they are doing.
//
// The room's live layer says who is present now: each member's publisher,
// name, the record they focus, and for an agent what it is doing. Every few seconds the page asks the runtime what its own person is
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
 * How long after someone arrives the page keeps asking the host, every
 * tick, for the line that says whose device theirs is: until it has it,
 * the desktop draws them neutral rather than guess a colour.
 */
export const PAIR_WAIT_MS = 30_000;

/**
 * The members of `layer` as the runtime reads them (`desktop_set_people`):
 * `null` while the layer is not live.
 */
export function liveMembers(layer, seen) {
  if (!layer?.available) return null;
  return layer.members().map(publisher => {
    const {participant, focus, place, wave, layout, following} = seen.get(publisher) ?? {};
    return {
      publisher,
      kind: participant?.kind === 'agent' ? 'agent' : 'person',
      label: participant?.label ?? '',
      // The record they focus: the window it shows here, or where no
      // window here shows it (a room whose layout is personal) its app,
      // is where the people panel says they are.
      focus: focus ?? null,
      activity: participant?.activity ?? null,
      // The strip a person shows, for the bar's counts.
      place: participant?.kind === 'agent' ? null : place ?? null,
      // A person's latest wave; the one it is for shows it, once.
      wave: participant?.kind === 'agent' ? null : wave ?? null,
      // Where a person's windows are, for a mini view of their screen.
      layout: participant?.kind === 'agent' ? null : layout ?? null,
      // Whose view a person follows: who follows whom, on every roster.
      following: participant?.kind === 'agent' ? null : following ?? null,
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
  const seen = new Map(); // publisher -> {participant, focus, place, wave, layout, following}
  let stopped = false;
  let timer;
  let peopleKey;
  let postedKey;
  let postedAt = -Infinity;
  let posting = false;
  let watchedAt = -Infinity;
  let saidStatus = null; // the own words last put on the person's presence
  let lines = [];
  let again = false; // asked to check while busy: check once it is done
  const arrivedAt = new Map(); // publisher -> when it joined, while unpaired

  function pushPeople() {
    if (stopped || !active()) return;
    const live = layer();
    // With its person, so the desktop counts this person's other devices
    // (their other tabs) as them rather than as someone else.
    const own = peer?.participant
      ? {publisher: live?.publisherId ?? '', kind: 'person', label: peer.participant.label, ...(peer.userPublicKey ? {user: peer.userPublicKey} : {})}
      : null;
    const json = JSON.stringify({own, members: liveMembers(live, seen)});
    if (json === peopleKey) return;
    peopleKey = json;
    runtime.desktop_set_people(mountId(), json);
  }
  function remember({publisher, frame}) {
    const previous = seen.get(publisher);
    seen.set(publisher, {participant: frame.participant ?? previous?.participant ?? null, focus: frame.focus ?? null, place: frame.place ?? null, wave: frame.wave ?? null, layout: frame.layout ?? null, following: frame.following ?? null});
    pushPeople();
  }
  const live = layer();
  const stops = live ? [
    live.onPresence(remember),
    live.onMember(({publisher, active: present}) => {
      if (!present) seen.delete(publisher);
      else if (!paired(publisher)) arrivedAt.set(publisher, now());
      pushPeople();
      if (present && !paired(publisher)) wake();
    }),
    live.onState?.(() => pushPeople()) ?? (() => {}),
  ] : [];
  for (const remote of live?.remoteState?.() ?? []) remember({publisher: remote.publisher, frame: remote});

  /** Whether the host's last lines say whose device `publisher` is. */
  function paired(publisher) {
    return lines.some(line => line?.device === publisher && line.user);
  }
  /** Someone who arrived lately whose person the host has not named yet. */
  function awaitingPerson(at) {
    for (const [publisher, since] of arrivedAt) {
      const agent = seen.get(publisher)?.participant?.kind === 'agent';
      if (agent || paired(publisher) || at - since >= PAIR_WAIT_MS || !layer()?.members?.().includes(publisher)) arrivedAt.delete(publisher);
    }
    return arrivedAt.size > 0;
  }
  async function post(state, at) {
    posting = true;
    postedAt = at;
    try {
      const answer = await peer.here(state.window, state.actions);
      lines = Array.isArray(answer) ? answer : [];
      if (!stopped && active()) runtime.desktop_set_here(mountId(), JSON.stringify(answer));
    } catch (error) {
      // Not posted: try again on the next tick, not at the next refresh.
      postedKey = undefined;
      postedAt = -Infinity;
      throw error;
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
        // What they say they are doing, in their own words, travels on
        // their presence: everyone here reads it at once.
        const status = typeof state.status === 'string' && state.status ? state.status : null;
        if (status !== saidStatus && typeof peer?.setStatus === 'function') {
          saidStatus = status;
          peer.setStatus(status);
        }
        const key = JSON.stringify([state.window, state.actions.map(action => action.id)]);
        if (typeof peer?.here === 'function' && !posting && (key !== postedKey || at - postedAt >= POST_EVERY_MS || awaitingPerson(at))) {
          postedKey = key;
          await post(state, at);
        }
        if (presenceUrl && at - watchedAt >= WATCHING_EVERY_MS) await watch(at);
      }
    } catch (error) {
      onError(error);
    }
    if (!stopped) timer = setTimer(tick, again ? 0 : TICK_MS);
    again = false;
  }
  function wake() {
    if (stopped) return;
    if (!posting && timer !== undefined) { clearTimer(timer); void tick(); } else again = true;
  }
  void tick();

  return {
    /** Check now, e.g. after the person acted. */
    wake,
    stop() {
      stopped = true;
      if (timer !== undefined) clearTimer(timer);
      for (const stop of stops) { try { stop(); } catch {} }
    },
  };
}
