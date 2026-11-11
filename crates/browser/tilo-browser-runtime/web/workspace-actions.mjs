/**
 * Workspaces on the person's machines, as the desktop's host actions:
 * making one, and inviting someone into it.
 *
 * - `new-workspace`: a workspace on a machine the person set up
 *   (`machine-setup.mjs`): the one this page is on when its host is theirs,
 *   else one this browser set up for them. The host creates it and admits
 *   this device as its owner (`workspaces.create`, `enter`); the page then
 *   opens it.
 * Inviting someone is the desktop's own invite sheet (`invite.rs`): it makes
 * and lists the workspace's invitations through its host, so the page carries
 * no invite action of its own.
 *
 * - `waiting-to-join`: the addresses people left at the door of a workspace
 *   that takes people by invitation (`membership.waitList`), for the owner
 *   and anyone else the host lets invite. Each is answered with an invitation
 *   of the inviter's own, a short link to that address, made for the entry so
 *   it shows as invited and leaves the list once someone joins with it; or
 *   taken off the list by hand (`unlist`). An invitation admits by itself, so
 *   nobody waits to be let in.
 *
 * Every step is the host's decision: a refusal is said, never worked
 * around.
 */
export const NEW_WORKSPACE = Object.freeze({id: 'new-workspace', title: 'New workspace',
  subtitle: 'Create a workspace on a machine you set up', source: 'workspace'});
/** Join a workspace a site lists: the desktop's directory sheet, which the
 * desktop opens itself (it needs nothing of the page but its site). */
export const JOIN_WORKSPACE = Object.freeze({id: 'join-workspace', title: 'Join a workspace',
  label: 'Join a workspace…', subtitle: 'Browse the workspaces this site lists', source: 'workspace'});
export const WAITING = Object.freeze({id: 'waiting-to-join', title: 'Waiting to join',
  subtitle: 'Invite people who left their address at the door'});

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
 * without a workspace and resolves to that peer. `inviteLink(invitation)`
 * resolves to the short link the workspace's site keeps the invitation
 * under, `https://<site>/i/<handle>#<key>`; `open({origin, room, name,
 * hostDevice})` opens a workspace, held by the host whose device key is
 * `ui` draws what the page shows: `askWorkspaceName(save)`,
 * `chooseMachine(origins)`, `share({title, body, link})`, `copy(text)`
 * (resolves whether it reached the clipboard),
 * `waitingList({load, invite, unlist})` (`load()` resolves to the wait-list
 * entries; `invite(entry)` makes the invitation for an address and has the
 * host mail it, else shares it, resolving to `{mailed, link}`) and
 * `fail(text)`. Attach the desktop with `attach(mount)`; route its host
 * actions to `perform`.
 */
export function createWorkspaceActions({
  peer = null, machines = () => [], lobby, inviteLink, open, ui,
  onError = error => console.warn('workspaces:', error),
}) {
  let mount;
  let lettingIn = false;
  let workspaceName = null;
  const inWorkspace = Boolean(peer?.workspace && peer.createInvitation);
  const actions = () => [
    ...(ownHost(peer) || machines().length ? [NEW_WORKSPACE] : []),
    JOIN_WORKSPACE,
    ...(lettingIn ? [WAITING] : []),
  ];
  const refresh = () => mount?.setHostActions?.(actions());
  const say = (error, what) => {
    onError(error);
    void ui.fail(`${what}: ${error?.message ?? error}`);
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

  /** An invitation for the address `entry` left at the door: mailed there
   * by the host where it sends mail, else shared for the inviter to send.
   * Resolves to `{mailed, link}`; the entry leaves the list once someone
   * joins with it. */
  const inviteContact = async entry => {
    const text = await peer.createInvitation({role: 'editor', waitListEntry: entry.requestId});
    const link = await inviteLink(text);
    try {
      await peer.membership.mailInvite(entry.requestId, link);
      return {mailed: true, link};
    } catch (refusal) {
      // A host that sends no mail: the inviter sends the link.
      console.info('wait list: the invitation was not mailed:', refusal?.message ?? refusal);
    }
    await ui.share({
      title: `Invite ${entry.contact}`,
      body: `Send this link to ${entry.contact}. Following it brings them into ${workspaceName ? `“${workspaceName}”` : 'this workspace'} as an editor.`,
      link,
    });
    return {mailed: false, link};
  };

  return {
    actions,
    /** Whether `id` is one of these actions. */
    handles: id => [NEW_WORKSPACE.id, WAITING.id].includes(id),
    /** Show the actions in `mount`'s desktop; whoever the host lets invite
     * is offered the wait list. */
    async attach(target) {
      mount = target;
      refresh();
      if (!inWorkspace) return;
      try {
        workspaceName = (await peer.membership.list()).name;
      } catch {
        // The host may keep its member list to the owner.
      }
      // The host shows the wait list to the owner and to members of a
      // workspace that has its members invite people; a host that keeps it
      // from this person refuses, which is nobody to invite, not a failure.
      lettingIn = await peer.membership.waitList().then(() => true, () => false);
      refresh();
    },
    /** Make a workspace on the machine at `origin` (one just set up). */
    async createOn(origin) {
      try { await newWorkspace(origin); } catch (error) { say(error, 'Could not create the workspace'); }
    },
    /** Run the action `id`; resolves when done (failures are said). */
    async perform(id) {
      try {
        if (id === NEW_WORKSPACE.id) await newWorkspace();
        else if (id === WAITING.id && lettingIn) {
          // The addresses left at the workspace's door, where the workspace
          // keeps a wait list: a workspace without one lists none.
          await ui.waitingList({load: () => peer.membership.waitList(), invite: inviteContact,
            unlist: requestId => peer.membership.unlist(requestId)});
        }
      } catch (error) {
        say(error, 'That did not work');
      }
    },
  };
}
