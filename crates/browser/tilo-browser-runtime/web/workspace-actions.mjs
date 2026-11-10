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
 * Every step is the host's decision: a refusal is said, never worked
 * around.
 */
export const NEW_WORKSPACE = Object.freeze({id: 'new-workspace', title: 'New workspace',
  subtitle: 'Create a workspace on a machine you set up', source: 'workspace'});
/** Join a workspace a site lists: the desktop's directory sheet, which the
 * desktop opens itself (it needs nothing of the page but its site). */
export const JOIN_WORKSPACE = Object.freeze({id: 'join-workspace', title: 'Join a workspace',
  label: 'Join a workspace…', subtitle: 'Browse the workspaces this site lists', source: 'workspace'});

const hex = bytes => Array.from(bytes ?? [], byte => byte.toString(16).padStart(2, '0')).join('');

/** Whether `peer`'s host serves the person the peer is: their own machine. */
export const ownHost = peer => Boolean(peer?.userPublicKey) && hex(peer.host?.hostCert?.user_public_key) === peer.userPublicKey;

/**
 * The workspace actions of a page whose peer is `peer` (`null` on the
 * person's own desktop). `machines()` lists the origins of the machines
 * this browser set up for the person; `lobby(origin)` connects to one
 * without a workspace and resolves to that peer; `open({origin, room, name,
 * hostDevice})` opens a workspace, held by the host whose device key is
 * `ui` draws what the page shows: `askWorkspaceName(save)`,
 * `chooseMachine(origins)` and `fail(text)`. Attach the desktop with `attach(mount)`; route its host
 * actions to `perform`.
 */
export function createWorkspaceActions({
  peer = null, machines = () => [], lobby, open, ui,
  onError = error => console.warn('workspaces:', error),
}) {
  let mount;
  const actions = () => [
    ...(ownHost(peer) || machines().length ? [NEW_WORKSPACE] : []),
    JOIN_WORKSPACE,
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

  return {
    actions,
    /** Whether `id` is one of these actions. */
    handles: id => id === NEW_WORKSPACE.id,
    /** Show the actions in `mount`'s desktop. */
    attach(target) {
      mount = target;
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
      } catch (error) {
        say(error, 'That did not work');
      }
    },
  };
}
