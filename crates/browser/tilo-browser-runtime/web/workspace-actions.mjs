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
 *   owner lets them in.
 * - `waiting-to-join`: the people who asked, each to let in or turn away
 *   (`membership.requests`, `admit`, `decline`). While the owner is in the
 *   room, each new request is also offered on a banner to let them in.
 *
 * Every step is the host's decision: a refusal is said, never worked
 * around.
 */
export const NEW_WORKSPACE = Object.freeze({id: 'new-workspace', title: 'New workspace',
  subtitle: 'Create a workspace on a machine you set up', source: 'workspace'});
export const INVITE = Object.freeze({id: 'invite-someone', title: 'Invite someone',
  subtitle: 'Copy a link that asks to join this workspace'});
export const WAITING = Object.freeze({id: 'waiting-to-join', title: 'Waiting to join',
  subtitle: 'Let in or turn away people who asked to join'});
const ADMIT = 'admit-request:';
/** How often the owner's page asks who is waiting. */
const WAITING_POLL_MS = 4000;

const hex = bytes => Array.from(bytes ?? [], byte => byte.toString(16).padStart(2, '0')).join('');

/** Whether `peer`'s host serves the person the peer is: their own machine. */
export const ownHost = peer => Boolean(peer?.userPublicKey) && hex(peer.host?.hostCert?.user_public_key) === peer.userPublicKey;

/**
 * The workspace actions of a page whose peer is `peer` (`null` on the
 * person's own desktop). `machines()` lists the origins of the machines
 * this browser set up for the person; `lobby(origin)` connects to one
 * without a room and resolves to that peer. `inviteLink(invitation)` is
 * the room's link carrying it; `open({origin, room, name, hostDevice})` opens
 * a workspace, held by the host whose device key is `hostDevice`.
 * `ui` draws what the page shows: `askWorkspaceName(save)`,
 * `chooseMachine(origins)`, `share({title, body, link})`,
 * `waitingList({load, admit, decline})` and `fail(text)`. Attach the
 * desktop with `attach(mount)`; route its host actions to `perform`.
 */
export function createWorkspaceActions({
  peer = null, machines = () => [], lobby, inviteLink, open, ui,
  every = (run, ms) => { const timer = setInterval(run, ms); return () => clearInterval(timer); },
  onError = error => console.warn('workspaces:', error),
}) {
  let mount;
  let owner = false;
  let workspaceName = null;
  let stop = null;
  const offered = new Set();
  const waiting = new Map();
  const inRoom = Boolean(peer?.room && peer.createInvitation);
  const actions = () => [
    ...(ownHost(peer) || (!peer && machines().length) ? [NEW_WORKSPACE] : []),
    ...(inRoom ? [INVITE] : []),
    ...(owner ? [WAITING] : []),
  ];
  const refresh = () => mount?.setHostActions?.(actions());
  const say = (error, what) => {
    onError(error);
    void ui.fail(`${what}: ${error?.message ?? error}`);
  };

  const askWaiting = async () => {
    let people;
    try { people = await peer.membership.requests(); } catch (error) { onError(error); return; }
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
    let text;
    try { text = await peer.createInvitation({role: 'editor'}); } catch (error) { say(error, 'Could not invite someone'); return; }
    await ui.share({
      title: `Invite someone to “${workspaceName ?? 'this workspace'}”`,
      body: 'Send this link to the person you invite. They ask to join with it, and you let them in here.',
      link: inviteLink(text),
    });
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
    /** Show the actions in `mount`'s desktop; an owner in the room hears
     * who asks to join from now on. */
    async attach(target) {
      mount = target;
      refresh();
      if (!inRoom) return;
      try {
        const room = await peer.membership.list();
        workspaceName = room.name;
        owner = room.members.some(member => member.owner && member.userPublicKey === peer.userPublicKey);
      } catch {
        // Only the owner may list who waits: anyone else invites, if the host lets them.
        owner = false;
      }
      refresh();
      if (owner && !stop) {
        stop = every(() => { void askWaiting(); }, WAITING_POLL_MS);
        await askWaiting();
      }
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
        else if (id === WAITING.id && owner) {
          await ui.waitingList({load: () => peer.membership.requests(), admit, decline: requestId => peer.membership.decline(requestId)});
        } else if (id.startsWith(ADMIT) && owner) await admit(id.slice(ADMIT.length));
      } catch (error) {
        say(error, 'That did not work');
      }
    },
    dispose() { stop?.(); stop = null; },
  };
}
