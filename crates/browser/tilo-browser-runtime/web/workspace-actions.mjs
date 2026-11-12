/**
 * Workspaces on the person's machines, as the desktop's host actions:
 * making one, and inviting someone into it.
 *
 * - `new-workspace`: a workspace of the person's own. On a machine they set
 *   up (`machine-setup.mjs`) it is the one this page is on when its host is
 *   theirs, else one this browser set up for them: the host creates it and
 *   admits this device as its owner (`workspaces.create`, `enter`). On a host
 *   that keeps workspaces of people's own for the people it lets in
 *   (`can_own_workspaces`), it is asked for by name and entered as theirs
 *   (`enterOwnWorkspace`), however many they ask for. The page then opens it.
 * - `copy-invite-link`: one press, and a link to this workspace is on the
 *   clipboard — the sheet is for choosing what someone gets, not for handing
 *   a link over. The link is made when the page attaches (a clipboard write
 *   has to be inside the press, and the host's answer is not), and another is
 *   made after each is handed over. Whoever wants a role chosen, or the
 *   invitation listed and taken back, opens the sheet (`invite.rs`).
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
/** One press, and the link is on the clipboard: the invite sheet is for
 * choosing what someone gets, not for handing a link over. */
export const INVITE_LINK = Object.freeze({id: 'copy-invite-link', title: 'Copy an invite link',
  subtitle: 'Anyone with it joins as an editor', source: 'workspace'});
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

/** Whether the host this page is in keeps workspaces of people's own for the
 * people it lets in, so this person's next workspace is one of theirs even
 * though the host is not. */
export const keepsOwn = peer => Boolean(peer?.workspace) && Boolean(peer?.host?.hostCert?.capabilities?.can_own_workspaces);

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
/** How long a link made ahead of the press is handed over for: an
 * invitation is good for an hour, and one older than this is made again
 * rather than handed on half spent. */
const READY_LINK_MS = 30 * 60_000;

export function createWorkspaceActions({
  peer = null, machines = () => [], lobby, inviteLink, open, ui,
  onError = error => console.warn('workspaces:', error),
}) {
  let mount;
  let lettingIn = false;
  let workspaceName = null;
  const inWorkspace = Boolean(peer?.workspace && peer.createInvitation);
  // A link made before it is asked for: copying it is then the press itself,
  // which is what lets the browser take it (a clipboard write outside a
  // gesture is a write the page may not make). One is made when the page
  // attaches, and another after each is handed over.
  let ready = null;
  const actions = () => [
    ...(ownHost(peer) || machines().length || keepsOwn(peer) ? [NEW_WORKSPACE] : []),
    ...(lettingIn ? [INVITE_LINK] : []),
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
    // A host that keeps workspaces of people's own keeps this person's next
    // one beside theirs: it is asked for by name and entered as theirs, with
    // no machine of this person's in between.
    const kept = !own && keepsOwn(peer) && (!on || on === peer.origin);
    const choices = own || kept ? [peer.origin] : on ? [on] : machines();
    const origin = choices.length > 1 ? await ui.chooseMachine(choices) : choices[0];
    if (!origin) return;
    const created = await ui.askWorkspaceName(async name => {
      if (kept) {
        const made = await peer.enterOwnWorkspace({name});
        return {room: made.workspace, name: made.name ?? name, hostDevice: hex(peer.host?.hostCert?.device_public_key)};
      }
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

  /** Make the next link this page hands over, and keep it ready. A failure
   * is not the person's to read yet: pressing the action says it. */
  const armInviteLink = async () => {
    if (ready) return ready;
    try {
      const text = await peer.createInvitation({role: 'editor'});
      ready = {link: await inviteLink(text), madeAt: Date.now()};
    } catch (refusal) {
      onError(refusal);
      return null;
    }
    return ready;
  };
  /** Hand over a link in one press: the one kept ready when it is still
   * good, else one made now (which the share card shows, having no press to
   * copy inside). */
  const copyInviteLink = async () => {
    const now = Date.now();
    const held = ready && now - ready.madeAt < READY_LINK_MS ? ready : null;
    const made = held ?? await armInviteLink();
    if (!made) {
      say(new Error('the workspace did not answer with an invitation'), 'Could not make an invite link');
      return;
    }
    ready = null;
    if (held && await ui.copy?.(made.link)) {
      void armInviteLink();
      return;
    }
    await ui.share({title: 'Invite someone to join',
      body: 'Anyone with this link joins as an editor. Take it back whenever you like, in the invite sheet.',
      link: made.link, copied: true});
    void armInviteLink();
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
    handles: id => [NEW_WORKSPACE.id, INVITE_LINK.id, WAITING.id].includes(id),
    /** Show the actions in `mount`'s desktop; whoever the host lets invite
     * is offered a link and the wait list. */
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
      // workspace that has its members invite people. Whether this person is
      // one is asked first, so a page never asks for a list its person may
      // not see (every guest of a public workspace would be refused, each
      // refusal a warning on the host); a list the host still refuses is
      // nobody to invite, not a failure.
      lettingIn = await peer.membership.mayInvite().catch(() => false)
        && await peer.membership.waitList().then(() => true, () => false);
      refresh();
      // Whoever may invite is handed a link without waiting for one to be
      // made: it is made now, while nothing is being asked of them.
      if (lettingIn) void armInviteLink();
    },
    /** Make a workspace on the machine at `origin` (one just set up). */
    async createOn(origin) {
      try { await newWorkspace(origin); } catch (error) { say(error, 'Could not create the workspace'); }
    },
    /** Run the action `id`; resolves when done (failures are said). */
    async perform(id) {
      try {
        if (id === NEW_WORKSPACE.id) await newWorkspace();
        else if (id === INVITE_LINK.id && lettingIn) await copyInviteLink();
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
