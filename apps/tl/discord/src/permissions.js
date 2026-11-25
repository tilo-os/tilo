// The bot's permissions in one channel, computed the way Discord does from what the gateway
// already knows of the server: the owner has all, then `@everyone` and the bot's roles,
// Administrator meaning all, then the channel's overwrites for `@everyone`, for the bot's roles
// together, and for the bot itself.

export const VIEW_CHANNEL = 1n << 10n;
export const CONNECT = 1n << 20n;
export const SPEAK = 1n << 21n;
export const ADMINISTRATOR = 1n << 3n;
export const CHANGE_NICKNAME = 1n << 26n;
export const SET_VOICE_CHANNEL_STATUS = 1n << 48n;

/// What joining a voice channel and speaking there takes, by the names Discord's client gives.
export const TO_SPEAK = [
  [VIEW_CHANNEL, "View Channel"],
  [CONNECT, "Connect"],
  [SPEAK, "Speak"],
];

const ALL = (1n << 64n) - 1n;
const OVERWRITE_ROLE = 0;
const OVERWRITE_MEMBER = 1;

function bits(text) {
  try {
    return BigInt(String(text ?? "0") || "0");
  } catch {
    return 0n;
  }
}

/// What the gateway knows of a server that decides the bot's permissions: its owner, its
/// roles' permissions, the bot's roles, and each channel's overwrites. `data` is GUILD_CREATE's.
export function standingOf(data, bot) {
  const members = data?.members ?? [];
  const own = members.find((member) => String(member?.user?.id ?? "") === String(bot));
  return {
    id: String(data?.id ?? ""),
    owner: String(data?.owner_id ?? ""),
    bot: String(bot ?? ""),
    roles: new Map((data?.roles ?? []).map((role) => [String(role.id), String(role.permissions ?? "0")])),
    botRoles: (own?.roles ?? []).map(String),
    overwrites: new Map(
      (data?.channels ?? []).map((channel) => [String(channel.id), channel.permission_overwrites ?? []]),
    ),
  };
}

/// The bot's permissions in `channel` of the server `standing` describes.
export function inChannel(standing, channel) {
  if (standing.owner && standing.owner === standing.bot) return ALL;
  const role = (id) => bits(standing.roles.get(String(id)));
  let base = role(standing.id);
  for (const id of standing.botRoles) base |= role(id);
  if (base & ADMINISTRATOR) return ALL;
  const overwrites = standing.overwrites.get(String(channel));
  if (!overwrites) return base;
  const everyone = overwrites.find(
    (o) => Number(o.type) === OVERWRITE_ROLE && String(o.id) === standing.id,
  );
  if (everyone) {
    base &= ~bits(everyone.deny);
    base |= bits(everyone.allow);
  }
  let allow = 0n;
  let deny = 0n;
  for (const o of overwrites) {
    if (Number(o.type) === OVERWRITE_ROLE && standing.botRoles.includes(String(o.id))) {
      allow |= bits(o.allow);
      deny |= bits(o.deny);
    }
  }
  base &= ~deny;
  base |= allow;
  const member = overwrites.find(
    (o) => Number(o.type) === OVERWRITE_MEMBER && String(o.id) === standing.bot,
  );
  if (member) {
    base &= ~bits(member.deny);
    base |= bits(member.allow);
  }
  return base & ALL;
}

/// The names of what the bot lacks to join `channel` and speak there.
export function missingToSpeak(standing, channel) {
  const have = inChannel(standing, channel);
  return TO_SPEAK.filter(([bit]) => !(have & bit)).map(([, name]) => name);
}

/// `["Connect", "Speak"]` as "Connect and Speak".
export function andList(names) {
  if (names.length <= 1) return names.join("");
  return `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;
}
