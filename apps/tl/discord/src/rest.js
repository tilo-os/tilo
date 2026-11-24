// Discord's REST calls the bridge makes, each through the workspace's vault: the request names
// the vault item holding the bot token and the vault adds it server-side, so the token is never
// an argument, a record or a log line here.
//
// A webhook's own token is a secret too: the bridge keeps it as a vault item and names it, and
// a send through the webhook goes through the vault the same way.

export const API = "https://discord.com/api/v10";

/// The Discord method paths the bridge uses, as the service names them.
export function userGuilds(tokenItem, reason) {
  return {
    url: `${API}/users/@me/guilds`,
    key: tokenItem,
    method: "GET",
    reason,
  };
}

/// Sends a message through a webhook, answering the message it made there.
export function webhookSend({ webhook, tokenItem }, op) {
  const query = "?wait=true";
  return {
    url: `${API}/webhooks/${webhook}${query}`,
    key: tokenItem,
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      username: op.username,
      avatar_url: op.avatar_url ?? undefined,
      content: op.content,
      thread_id: op.thread ?? undefined,
      attachments: op.attachments.map((file, index) => ({
        id: index,
        filename: file.name,
      })),
    }),
    reason: "a message written in the workspace",
  };
}

/// Edits a message the bridge sent.
export function webhookEdit({ webhook, tokenItem }, op) {
  return {
    url: `${API}/webhooks/${webhook}/messages/${op.message}`,
    key: tokenItem,
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ content: op.content }),
    reason: "an edit made in the workspace",
  };
}

/// Deletes a message the bridge sent.
export function webhookDelete({ webhook, tokenItem }, op) {
  return {
    url: `${API}/webhooks/${webhook}/messages/${op.message}`,
    key: tokenItem,
    method: "DELETE",
    reason: "a deletion made in the workspace",
  };
}

/// Puts the bot's reaction on a message, or takes it back.
export function react({ tokenItem }, op) {
  const emoji = encodeURIComponent(op.emoji);
  const suffix = op.op === "react" ? "" : "/@me";
  return {
    url: `${API}/channels/${op.channel}/messages/${op.message}/reactions/${emoji}${suffix}`,
    key: tokenItem,
    method: op.op === "react" ? "PUT" : "DELETE",
    reason: "a reaction made in the workspace",
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
