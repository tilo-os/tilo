// Discord's REST calls the bridge makes, each through the workspace's vault: the request names
// the vault item holding the bot token and the vault adds it server-side, so the token is never
// an argument, a record or a log line here.
//
// A webhook's own token is a secret too: the bridge keeps it as a vault item and names it, and
// a send through the webhook goes through the vault the same way.

import { CHAT } from "./plan.js";

export const API = "https://discord.com/api/v10";

/// The API `origin` names, or Discord's own: a test's fake server, which the account record
/// names so a sandbox that clears its environment can still be pointed at one.
function at(origin) {
  return String(origin || API);
}

/// The name of the webhooks the bridge makes: one per bridged text channel.
export const WEBHOOK_NAME = "Tilo";

/// The webhooks of a channel. Discord answers a webhook's token only for the ones this bot made,
/// so the bridge finds its own there again after a restart and keeps no token anywhere.
export function channelWebhooks({ tokenItem, origin = API }, channel) {
  return {
    url: `${at(origin)}/channels/${channel}/webhooks`,
    key: tokenItem,
    method: "GET",
    reason: "the webhook messages from the workspace are sent through",
  };
}

/// Makes the bridge's webhook in a channel: what messages from the workspace are sent through,
/// each under its author's name and picture.
export function createWebhook({ tokenItem, origin = API }, channel) {
  return {
    url: `${at(origin)}/channels/${channel}/webhooks`,
    key: tokenItem,
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name: WEBHOOK_NAME }),
    reason: "the webhook messages from the workspace are sent through",
  };
}

/// How many of a channel's newest messages the bridge reads when it meets the server: what was
/// said there before, and what was said while it was away.
export const HISTORY = 50;

/// A channel's newest messages, newest first, as Discord answers them.
export function channelMessages({ tokenItem, origin = API }, channel, limit = HISTORY) {
  return {
    url: `${at(origin)}/channels/${channel}/messages?limit=${limit}`,
    key: tokenItem,
    method: "GET",
    reason: "what was said in a bridged channel before the bridge saw it",
  };
}

/// Sends a message through a webhook, answering the message it made there. `webhook` is the
/// webhook's `id/token`, the address Discord executes it at.
///
/// Files go as a multipart body: `payload_json` and one `files[n]` per file, each naming the
/// content the workspace keeps by its BLAKE3 address. The vault reads those bytes from the
/// workspace's store, so an attachment never passes through this process.
export function webhookSend({ webhook, tokenItem, origin = API }, op) {
  const query = "?wait=true";
  const payload = {
    username: op.username,
    avatar_url: op.avatar_url ?? undefined,
    content: op.content,
    // Only a user written as `<@id>` is pinged: never `@everyone`, `@here` or a role, whatever
    // the workspace's text says.
    allowed_mentions: ONLY_USERS,
    thread_id: op.thread ?? undefined,
    attachments: op.attachments.map((file, index) => ({
      id: index,
      filename: file.name,
    })),
  };
  const call = {
    url: `${at(origin)}/webhooks/${webhook}${query}`,
    key: tokenItem,
    method: "POST",
    reason: "a message written in the workspace",
  };
  if (op.attachments.length === 0) {
    return {
      ...call,
      headers: { "content-type": "application/json" },
      body: JSON.stringify(payload),
    };
  }
  return {
    ...call,
    bodyParts: [
      { name: "payload_json", json: payload },
      ...op.attachments.map((file, index) => ({
        name: `files[${index}]`,
        content: {
          app: CHAT,
          hash: file.blob,
          filename: file.name,
          contentType: file.media || "application/octet-stream",
        },
      })),
    ],
  };
}

/// What a message from the workspace may ping in Discord: the users it names, nobody else.
const ONLY_USERS = { parse: ["users"] };

/// Edits a message the bridge sent.
export function webhookEdit({ webhook, tokenItem, origin = API }, op) {
  return {
    url: `${at(origin)}/webhooks/${webhook}/messages/${op.message}`,
    key: tokenItem,
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ content: op.content, allowed_mentions: ONLY_USERS }),
    reason: "an edit made in the workspace",
  };
}

/// Deletes a message the bridge sent.
export function webhookDelete({ webhook, tokenItem, origin = API }, op) {
  return {
    url: `${at(origin)}/webhooks/${webhook}/messages/${op.message}`,
    key: tokenItem,
    method: "DELETE",
    reason: "a deletion made in the workspace",
  };
}

/// Puts the bot's reaction on a message, or takes it back.
export function react({ tokenItem, origin = API }, op) {
  const emoji = encodeURIComponent(op.emoji);
  const suffix = op.op === "react" ? "" : "/@me";
  return {
    url: `${at(origin)}/channels/${op.channel}/messages/${op.message}/reactions/${emoji}${suffix}`,
    key: tokenItem,
    method: op.op === "react" ? "PUT" : "DELETE",
    reason: "a reaction made in the workspace",
  };
}

/// Starts a thread under a message: a public thread of the channel, named as the thread here is.
/// Its answer is the thread, whose id joins the two sides.
export function startThread({ tokenItem, origin = API }, op) {
  return {
    url: `${at(origin)}/channels/${op.channel}/messages/${op.message}/threads`,
    key: tokenItem,
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name: op.name }),
    reason: "a thread started in the workspace",
  };
}

/// Shows `status` as a voice channel's status, the line Discord shows under its name: who is on
/// the workspace's side of the call carried there. An empty status clears it.
export function voiceStatus({ tokenItem, origin = API }, channel, status) {
  return {
    url: `${at(origin)}/channels/${channel}/voice-status`,
    key: tokenItem,
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ status: status ? status.slice(0, 500) : null }),
    reason: "who is in the workspace's side of a call",
  };
}

/// Sets the bot's nickname in `guild` to `nick`, where a voice channel's status cannot say who
/// is on the workspace's side. An empty nick gives it back its own name.
export function nickname({ tokenItem, origin = API }, guild, nick) {
  return {
    url: `${at(origin)}/guilds/${guild}/members/@me`,
    key: tokenItem,
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ nick: nick ? [...nick].slice(0, 32).join("") : null }),
    reason: "who is in the workspace's side of a call",
  };
}

/// Kicks, bans or unbans a linked member of `guild`, as the workspace did to their person. The
/// reason goes to the server's audit log.
export function member({ tokenItem, origin = API }, guild, op) {
  const ban = op.op === "ban" || op.op === "unban";
  const headers = op.reason ? { "x-audit-log-reason": encodeURIComponent(op.reason) } : {};
  if (op.op === "ban") headers["content-type"] = "application/json";
  return {
    url: `${at(origin)}/guilds/${guild}/${ban ? "bans" : "members"}/${op.user}`,
    key: tokenItem,
    method: op.op === "ban" ? "PUT" : "DELETE",
    headers,
    ...(op.op === "ban" ? { body: "{}" } : {}),
    reason: "a member the workspace removed, banned or let back",
  };
}

/// Makes one of the bridge's roles in `guild`, or brings it back in line when `op.id` names it.
/// Its answer is the role, whose id joins it to the workspace's role.
export function applyRole({ tokenItem, origin = API }, guild, op) {
  return {
    url: `${at(origin)}/guilds/${guild}/roles${op.id ? `/${op.id}` : ""}`,
    key: tokenItem,
    method: op.id ? "PATCH" : "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name: op.name, permissions: op.permissions, color: op.color }),
    reason: "the workspace's roles, kept in the server",
  };
}

/// Sets a channel's permission overwrite for one of the bridge's roles.
export function setOverwrite({ tokenItem, origin = API }, op) {
  return {
    url: `${at(origin)}/channels/${op.channel}/permissions/${op.target}`,
    key: tokenItem,
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ allow: op.allow, deny: op.deny, type: op.target_kind === "member" ? 1 : 0 }),
    reason: "what a workspace role may not do in a bridged channel",
  };
}

/// Gives a member of `guild` one role, or takes it back: one request each.
export function memberRole({ tokenItem, origin = API }, guild, user, role, given) {
  return {
    url: `${at(origin)}/guilds/${guild}/members/${user}/roles/${role}`,
    key: tokenItem,
    method: given ? "PUT" : "DELETE",
    reason: "a linked member's role in the workspace",
  };
}

/// A bot token's shape: three non-empty dot-separated parts of base64url characters.
export function looksLikeToken(token) {
  const parts = String(token ?? "").split(".");
  return (
    parts.length === 3 &&
    parts.every((part) => part.length > 0 && /^[A-Za-z0-9_-]+$/.test(part))
  );
}

/// The place a bot's token belongs: what the vault keeps it for, and where it is sent.
export const ORIGIN = "https://discord.com";

/// How the vault sends a bot's token: Discord reads `Authorization: Bot <token>`.
export const SENT_IN = "header:Authorization:Bot";

/// Discord's path for a server's channels: what the bridge reads to choose the ones it brings
/// in.
export function guildChannels(guild) {
  return `/guilds/${guild}/channels`;
}

/// The servers the bot is in, from Discord's answer to `users/@me/guilds`: each one's id and
/// name, by name. A server the answer names with no id a server has is left out.
export function serversOf(answer) {
  return listed(answer, (guild) => ({
    id: String(guild?.id ?? ""),
    name: String(guild?.name ?? ""),
  })).sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
}

/// The channels of a server, from Discord's answer to `guilds/<id>/channels`, as the app's
/// record keeps them: text, voice, category and thread channels sit in their order.
export function channelsOf(answer) {
  return listed(answer, (channel) => ({
    id: String(channel?.id ?? ""),
    kind: kindOf(channel),
    name: String(channel?.name ?? ""),
    parent: channel?.parentId ? String(channel.parentId) : null,
    position: Number(channel?.position ?? 0),
    topic: String(channel?.topic ?? ""),
    overwrites: [],
  }));
}

/// What kind of place `channel` is, as the app's record names it: Discord numbers categories 4,
/// voice channels 2 and threads 10 to 12.
function kindOf(channel) {
  if (channel?.type === 4) return "category";
  if (channel?.type === 2) return "voice";
  if (channel?.type === 10 || channel?.type === 11 || channel?.type === 12) return "thread";
  return "text";
}

/// The list Discord answered, each entry read by `read`; a refusal or an answer that is no list
/// is said in words a person reads.
function listed(answer, read) {
  const body = bodyOf(answer);
  if (!Array.isArray(body)) {
    throw new Error(whyRefused(answer));
  }
  return body.filter((entry) => entry && String(entry.id ?? "") !== "").map(read);
}

/// What Discord answered, as its JSON.
function bodyOf(answer) {
  if (!answer) return null;
  let body = answer.body ?? answer;
  if (typeof body === "string") {
    try {
      return JSON.parse(body);
    } catch {
      return null;
    }
  }
  return body;
}

/// Why Discord did not answer a list: what its status says, in words a person reads.
export function whyRefused(answer) {
  const status = Number(answer?.status ?? 0);
  if (status === 401) {
    return "Discord did not accept the bot's token. Check it in your vault and paste the right one.";
  }
  if (status === 403) {
    return "The bot may not read this server. Give it the permission in Discord first.";
  }
  if (status === 404) {
    return "Discord found no such server for this bot.";
  }
  if (status === 429) {
    return "Discord is asking the bridge to slow down. Try again in a moment.";
  }
  if (status === 0) {
    return "Discord could not be reached. Check this device's network.";
  }
  return `Discord answered ${status}, so nothing was read.`;
}
