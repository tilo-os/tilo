/**
 * Workspaces on the person's machines, as the desktop's host actions:
 * making one, inviting someone into it, and letting in who asks.
 *
 * - `new-workspace`: a workspace on a machine the person set up
 *   (`machine-setup.mjs`): the one this page is on when its host is theirs,
 *   else one this browser set up for them. The host creates it and admits
 *   this device as its owner (`workspaces.create`, `enter`); the page then
 *   opens it.
 * - `invite-someone` (also the desktop's own Invite): an invitation to the
 *   room this page is in (`createInvitation`), presented as a link with its
 *   `invite` secret to copy and send. Whoever opens it asks to join; the
 *   owner lets them in. A public workspace (anyone may join it as a guest)
 *   needs no invitation: inviting copies its own link and says so.
 * - `waiting-to-join`: the people who asked, each to let in or turn away
 *   (`membership.requests`, `admit`, `decline`), for the owner and anyone
 *   else the host lets see who asks (whoever it lets invite). While they
 *   are in the room, each new request is also offered on a banner to let
 *   them in. A room that takes people by invitation also lists the
 *   addresses people left at its door (`membership.waitList`): each is
 *   answered with an invitation of the inviter's own, a link they send to
 *   that address, made for the entry so it shows as invited and leaves the
 *   list once someone joins with it; or taken off the list by hand
 *   (`unlist`). They are nobody to let in, so they get no banner.
 *
 * Every step is the host's decision: a refusal is said, never worked
 * around.
 */
export const NEW_WORKSPACE = Object.freeze({id: 'new-workspace', title: 'New workspace',
  subtitle: 'Create a workspace on a machine you set up', source: 'workspace'});
export const INVITE = Object.freeze({id: 'invite-someone', title: 'Invite someone',
  subtitle: 'Copy a link that asks to join this workspace'});
/** Invite people in a public workspace: its link is the invitation. */
export const INVITE_PUBLIC = Object.freeze({...INVITE, title: 'Invite people',
  subtitle: 'Copy the link: anyone can join this public workspace with it'});
export const WAITING = Object.freeze({id: 'waiting-to-join', title: 'Waiting to join',
  subtitle: 'Let in people who asked to join, or invite those on the wait list'});
const ADMIT = 'admit-request:';
/** How often the owner's page asks who is waiting. */
const WAITING_POLL_MS = 4000;

const hex = bytes => Array.from(bytes ?? [], byte => byte.toString(16).padStart(2, '0')).join('');

/** Whether `peer`'s host serves the person the peer is: their own machine. */
/** How a wait-list entry invited at `atMs` says so at `now`, "Invited 2
 * hours ago", so whoever reads the list does not invite it twice. */
export function invitedLine(atMs, now = Date.now(), locale = undefined) {
  const seconds = Math.round((atMs - now) / 1000);
  const relative = new Intl.RelativeTimeFormat(locale, {numeric: 'auto'});
  for (const [unit, size] of [['day', 86_400], ['hour', 3600], ['minute', 60]]) {
    if (Math.abs(seconds) >= size) return `Invited ${relative.format(Math.round(seconds / size), unit)}`;
  }
  return 'Invited just now';
}

export const ownHost = peer => Boolean(peer?.userPublicKey) && hex(peer.host?.hostCert?.user_public_key) === peer.userPublicKey;

/**
 * The workspace actions of a page whose peer is `peer` (`null` on the
 * person's own desktop). `machines()` lists the origins of the machines
 * this browser set up for the person; `lobby(origin)` connects to one
 * without a room and resolves to that peer. `inviteLink(invitation)` is
 * the room's link carrying it; `open({origin, room, name, hostDevice})` opens
 * a workspace, held by the host whose device key is `hostDevice`.
 * `ui` draws what the page shows: `askWorkspaceName(save)`,
 * `chooseMachine(origins)`, `share({title, body, link})`, `copy(text)`
 * (resolves whether it reached the clipboard),
 * `waitingList({load, admit, decline, invite, unlist})` (`load()` resolves
 * to `{people, waitList}`; `invite(entry)` makes the invitation for a
 * wait-list address and has the host mail it, else shares it, resolving to
 * `{mailed, link}`) and `fail(text)`. `publicLink()` is
 * the link anyone joins the room in front with when it is a public
 * workspace, else `null`. Attach the desktop with `attach(mount)`; route its
 * host actions to `perform`.
 */
export function createWorkspaceActions({
  peer = null, machines = () => [], lobby, inviteLink, publicLink = () => null, open, ui,
  every = (run, ms) => { const timer = setInterval(run, ms); return () => clearInterval(timer); },
  onError = error => console.warn('workspaces:', error),
}) {
  let mount;
  let lettingIn = false;
  let workspaceName = null;
  let stop = null;
  const offered = new Set();
  const waiting = new Map();
  const inRoom = Boolean(peer?.room && peer.createInvitation);
  const actions = () => [
    ...(ownHost(peer) || (!peer && machines().length) ? [NEW_WORKSPACE] : []),
    ...(inRoom ? [publicLink() ? INVITE_PUBLIC : INVITE] : []),
    ...(lettingIn ? [WAITING] : []),
  ];
  const refresh = () => mount?.setHostActions?.(actions());
  const say = (error, what) => {
    onError(error);
    void ui.fail(`${what}: ${error?.message ?? error}`);
  };

  const askWaiting = async () => {
    let people;
    try { people = await peer.membership.requests(); } catch (error) { onError(error); return false; }
    const now = new Set(people.map(person => person.requestId));
    for (const id of offered) if (!now.has(id)) offered.delete(id);
    for (const person of people) {
      waiting.set(person.requestId, person);
      if (offered.has(person.requestId)) continue;
      offered.add(person.requestId);
      mount?.offerHostAction?.({action: {id: `${ADMIT}${person.requestId}`, title: `Let ${person.name} in`},
        title: `${person.name} asks to join`,
        body: `${person.name} wants to join “${workspaceName ?? 'this workspace'}” as ${person.role}. Let in only someone you invited.`});
    }
    return true;
  };

  /** Hear who asks to join from now on, and offer the list of them. */
  const letPeopleIn = async () => {
    if (stop) return;
    // Someone the host does not show who asks hears nothing more.
    if (!await askWaiting()) return;
    lettingIn = true;
    refresh();
    stop = every(() => { void askWaiting(); }, WAITING_POLL_MS);
  };

  const newWorkspace = async (on = null) => {
    const own = ownHost(peer) && (!on || on === peer.origin);
    const choices = own ? [peer.origin] : on ? [on] : machines();
    const origin = choices.length > 1 ? await ui.chooseMachine(choices) : choices[0];
    if (!origin) return;
    const created = await ui.askWorkspaceName(async name => {
      const host = own ? peer : await lobby(origin);
      try {
        const workspace = await host.workspaces.create(name);
        await host.workspaces.enter(workspace.room);
        // The host that holds it, which the browser remembers it under.
        return {...workspace, hostDevice: hex(host.host?.hostCert?.device_public_key)};
      } finally {
        if (!own) await host.dispose?.();
      }
    });
    if (created) await open({origin, room: created.room, name: created.name, hostDevice: created.hostDevice});
  };

  const invite = async () => {
    if (!inRoom) return;
    const open = publicLink();
    if (open) {
      // Anyone may join a public workspace: its link is the invitation, and
      // no one is asked to let them in.
      const named = workspaceName ? `“${workspaceName}”` : 'this workspace';
      if (await ui.copy?.(open)) {
        mount?.acknowledge?.(`Link copied: anyone can join ${named} with it`);
        return;
      }
      await ui.share({title: `Invite people to ${named}`,
        body: 'This workspace is public: anyone with this link can join it.', link: open});
      return;
    }
    let text;
    try { text = await peer.createInvitation({role: 'editor'}); } catch (error) { say(error, 'Could not invite someone'); return; }
    // Whom they invite asks to join, and they let them in here: the host
    // that let them invite shows them who asks.
    if (!lettingIn) void letPeopleIn();
    await ui.share({
      title: `Invite someone to “${workspaceName ?? 'this workspace'}”`,
      body: 'Send this link to the person you invite. They ask to join with it, and you let them in here.',
      link: inviteLink(text),
    });
  };

  /** An invitation for the address `entry` left at the door: mailed there
   * by the host where it sends mail, else shared for the inviter to send.
   * Resolves to `{mailed, link}`; the entry leaves the list once someone
   * joins with it. */
  const inviteContact = async entry => {
    const text = await peer.createInvitation({role: 'editor', waitListEntry: entry.requestId});
    const link = inviteLink(text);
    try {
      await peer.membership.mailInvite(entry.requestId, link);
      return {mailed: true, link};
    } catch (refusal) {
      // A host that sends no mail: the inviter sends the link.
      console.info('wait list: the invitation was not mailed:', refusal?.message ?? refusal);
    }
    await ui.share({
      title: `Invite ${entry.contact}`,
      body: `Send this link to ${entry.contact}. They join ${workspaceName ? `“${workspaceName}”` : 'this workspace'} with it, and you let them in here. They leave the wait list once they join.`,
      link,
    });
    return {mailed: false, link};
  };

  const admit = async requestId => {
    const person = waiting.get(requestId);
    await peer.membership.admit(requestId);
    waiting.delete(requestId);
    mount?.offerHostAction?.({action: WAITING, title: `${person?.name ?? 'They'} joined`,
      body: `${person?.name ?? 'The person'} is in “${workspaceName ?? 'this workspace'}” now.`});
  };

  return {
    actions,
    /** Whether `id` is one of these actions. */
    handles: id => [NEW_WORKSPACE.id, INVITE.id, WAITING.id].includes(id) || id.startsWith(ADMIT),
    /** Show the actions in `mount`'s desktop; someone who lets people in
     * hears who asks to join from now on. */
    async attach(target) {
      mount = target;
      refresh();
      if (!inRoom) return;
      let owner = false;
      try {
        const room = await peer.membership.list();
        workspaceName = room.name;
        owner = room.members.some(member => member.owner && member.userPublicKey === peer.userPublicKey);
      } catch {
        // The host may keep its member list to the owner.
      }
      // The host shows who asks to join to whoever it lets invite, and
      // they let in whom they invited: the owner, and members of a room
      // that has its members invite people.
      if (owner || await peer.membership.requests().then(() => true, () => false)) await letPeopleIn();
      else refresh();
    },
    /** Make a workspace on the machine at `origin` (one just set up). */
    async createOn(origin) {
      try { await newWorkspace(origin); } catch (error) { say(error, 'Could not create the workspace'); }
    },
    /** Run the action `id`; resolves when done (failures are said). */
    async perform(id) {
      try {
        if (id === NEW_WORKSPACE.id) await newWorkspace();
        else if (id === INVITE.id) await invite();
        else if (id === WAITING.id && lettingIn) {
          // The wait list beside them, where the room keeps one: a room
          // without one, or a host that keeps it from this person, lists none.
          const load = async () => {
            const [people, waitList] = await Promise.all([peer.membership.requests(), peer.membership.waitList().catch(() => [])]);
            return {people, waitList};
          };
          await ui.waitingList({load, admit, decline: requestId => peer.membership.decline(requestId),
            invite: inviteContact, unlist: requestId => peer.membership.unlist(requestId)});
        } else if (id.startsWith(ADMIT) && lettingIn) await admit(id.slice(ADMIT.length));
      } catch (error) {
        say(error, 'That did not work');
      }
    },
    dispose() { stop?.(); stop = null; },
  };
}
