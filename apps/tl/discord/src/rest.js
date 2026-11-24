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

/// Sends a message through a webhook, answering the message it made there.
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

/// Edits a message the bridge sent.
export function webhookEdit({ webhook, tokenItem, origin = API }, op) {
  return {
    url: `${at(origin)}/webhooks/${webhook}/messages/${op.message}`,
    key: tokenItem,
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ content: op.content }),
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
