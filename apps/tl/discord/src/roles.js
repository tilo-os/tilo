// The workspace's roles kept as Discord roles the bridge makes and owns ("Tilo Viewer" … "Tilo
// Owner"), when the bridge is connected to map them. Their definitions are kept in line, and a
// linked account holds the one its person has here. `@everyone` and the server's own roles are
// never changed. Discord roles only add permissions, so what a Viewer and a Commenter may not do
// is denied through an overwrite for that role on every bridged channel.

import { mapping, mappings } from "./plan.js";

/// The workspace's roles, lowest first.
export const TILO_ROLES = ["viewer", "commenter", "editor", "moderator", "owner"];

const KICK_MEMBERS = 1n << 1n;
const BAN_MEMBERS = 1n << 2n;
const ADD_REACTIONS = 1n << 6n;
const VIEW_CHANNEL = 1n << 10n;
const SEND_MESSAGES = 1n << 11n;
const MANAGE_MESSAGES = 1n << 13n;
const ATTACH_FILES = 1n << 15n;
const READ_MESSAGE_HISTORY = 1n << 16n;
const CONNECT = 1n << 20n;
const SPEAK = 1n << 21n;
const USE_APPLICATION_COMMANDS = 1n << 31n;
const SEND_MESSAGES_IN_THREADS = 1n << 38n;
const MODERATE_MEMBERS = 1n << 40n;

/// What an Editor may do in Discord; the other roles are told apart from it.
const EVERYONE =
  VIEW_CHANNEL |
  READ_MESSAGE_HISTORY |
  SEND_MESSAGES |
  SEND_MESSAGES_IN_THREADS |
  ADD_REACTIONS |
  ATTACH_FILES |
  CONNECT |
  SPEAK |
  USE_APPLICATION_COMMANDS;
const MODERATION = MANAGE_MESSAGES | MODERATE_MEMBERS | KICK_MEMBERS | BAN_MEMBERS;
const WRITING = SEND_MESSAGES | SEND_MESSAGES_IN_THREADS | ATTACH_FILES;

const COLORS = {
  viewer: 0x95a5a6,
  commenter: 0x3498db,
  editor: 0x2ecc71,
  moderator: 0xe67e22,
  owner: 0xe74c3c,
};

/// The Discord permissions of a workspace role.
export function permissions(role) {
  switch (role) {
    case "viewer":
      return EVERYONE & ~(WRITING | ADD_REACTIONS);
    case "commenter":
      return EVERYONE & ~WRITING;
    case "moderator":
    case "owner":
      return EVERYONE | MODERATION;
    default:
      return EVERYONE;
  }
}

/// What a role's overwrite denies on each bridged channel.
export function channelDeny(role) {
  return EVERYONE & ~permissions(role) & ~VIEW_CHANNEL;
}

/// The Discord name of the bridge's role for the workspace's `role`.
export function roleName(role) {
  return `Tilo ${role.charAt(0).toUpperCase()}${role.slice(1)}`;
}

function bits(text) {
  try {
    return BigInt(String(text ?? "0") || "0");
  } catch {
    return 0n;
  }
}

/// Whether the bridge keeps the server's roles: mapping them is on and it bridges the whole
/// server.
export function keepsRoles(state) {
  return Boolean(state?.map_roles) && Boolean(state?.guild_id) && !state?.only_channel;
}

/// What keeps the bridge's roles in line with the workspace's, against `layout`, the roles and
/// channels Discord last showed: `apply` the roles to make or correct (each an `apply-role` op),
/// and `overwrites` the channel overwrites to set for the roles already mapped. `asked` holds
/// the overwrites already sent and not yet seen, which are not asked again: the layouts Discord
/// sent before one took effect still arrive.
export function rolesPlan(state, layout, asked = new Set()) {
  const plan = { apply: [], overwrites: [] };
  // Before Discord's layout is in, the bridge knows none of the server's roles, not even the
  // ones it made: every server has `@everyone`, so none means none observed.
  const roles = layout?.roles ?? [];
  if (!keepsRoles(state) || roles.length === 0) return plan;
  const vanished = new Set(state.vanished ?? []);
  for (const role of TILO_ROLES) {
    const mapped = mapping(state, "role", role);
    const current = mapped ? roles.find((held) => held.id === mapped.discord) ?? null : null;
    // Mapped and not seen yet: just made, Discord's echo not in.
    if (mapped && !current && !vanished.has(mapped.discord)) continue;
    const want = {
      name: roleName(role),
      permissions: permissions(role).toString(),
      color: COLORS[role],
    };
    if (
      current &&
      current.name === want.name &&
      bits(current.permissions) === permissions(role) &&
      Number(current.color) === want.color
    ) {
      continue;
    }
    plan.apply.push({ op: "apply-role", id: current?.id ?? null, tilo: role, ...want });
  }
  plan.overwrites = overwritesOf(state, layout, asked);
  return plan;
}

/// The overwrites the Viewer's and Commenter's roles want on each bridged channel, of those not
/// already in place or asked for.
export function overwritesOf(state, layout, asked = new Set()) {
  const wanted = [];
  if (!keepsRoles(state)) return wanted;
  const channels = layout?.channels ?? [];
  for (const role of ["viewer", "commenter"]) {
    const target = mapping(state, "role", role)?.discord;
    if (!target) continue;
    const deny = channelDeny(role);
    for (const entry of mappings(state).filter((m) => m.kind === "channel")) {
      const channel = channels.find((held) => held.id === entry.discord);
      if (!channel) continue;
      const current = (channel.overwrites ?? []).find((o) => o.id === target && Number(o.kind) === 0);
      if (current && bits(current.deny) === deny && bits(current.allow) === 0n) continue;
      const op = {
        op: "set-overwrite",
        channel: channel.id,
        target,
        target_kind: "role",
        allow: "0",
        deny: deny.toString(),
      };
      if (asked.has(JSON.stringify(op))) continue;
      wanted.push(op);
    }
  }
  return wanted;
}

/// What brings a linked member's bridge roles in line with `role`, their role here: the one they
/// should hold added, the bridge's others taken off. `current` is their Discord roles when an
/// event said them; without it every other bridge role is taken off, which Discord does nothing
/// for when they hold none. `null` when the account is not linked or nothing changes.
export function memberRoles(state, user, role, current = null) {
  if (!keepsRoles(state)) return null;
  if (!(state.links ?? []).some((link) => link.discord_user === user)) return null;
  const want = role ? mapping(state, "role", role)?.discord ?? null : null;
  const holds = (id) => current === null || current.includes(id);
  const add = want && (current === null || !current.includes(want)) ? [want] : [];
  const remove = TILO_ROLES.map((held) => mapping(state, "role", held)?.discord)
    .filter((id) => id && id !== want && holds(id));
  if (add.length === 0 && remove.length === 0) return null;
  return { op: "set-member-roles", user, add, remove };
}
