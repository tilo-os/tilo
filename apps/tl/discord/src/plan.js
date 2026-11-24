// What a Discord event and a change in the workspace mean for both sides.
//
// The planners are pure: they answer the calls to make, each naming the app, the action and its
// arguments, so the same event gives the same calls wherever it is planned and a test can read
// them. Discord's own records keep what joins the two sides; Chat keeps conversations and the
// generic words a bridge uses (a relayed author, an outside id, a service).

/// Chat's app id and this app's: the id its package declares, which the launcher names to every
/// process it starts.
export const CHAT = "so.tilo.chat";
export const SELF = process.env.TILO_PACKAGE_ID || "so.tilo.discord.preview";

/// Discord takes a message of 2000 characters; the rest is said by the cut.
export const MAX_CONTENT = 2000;

/// The largest file Discord takes from a webhook, and the most one message carries.
export const MAX_UPLOAD_BYTES = 10 * 1024 * 1024;
export const MAX_UPLOADS = 10;

/// `text` as Discord takes it: 2000 characters at most, the rest said by the cut.
export function fit(text) {
  const chars = [...(text ?? "")];
  return chars.length <= MAX_CONTENT ? (text ?? "") : `${chars.slice(0, MAX_CONTENT - 1).join("")}…`;
}

/// A file's bytes as Discord's worker reads them: the bare blake3 hex of the content address.
export function bareBlob(address) {
  const scheme = "tilo-blob://";
  return typeof address === "string" && address.startsWith(scheme) ? address.slice(scheme.length) : (address ?? "");
}

/// The published id of a workspace resource, as the API addresses it.
export function published(id) {
  const text = String(id ?? "");
  return text.startsWith("tl:") ? text : `tl:${CHAT}:${text}`;
}

/// The key a mapping stands under: `kind:tilo`.
export function mapKey(kind, tilo) {
  return `${kind}:${tilo}`;
}

/// The mapping of `tilo` as `kind` in the bridge's record.
export function mapping(state, kind, tilo) {
  return state.map?.[mapKey(kind, tilo)] ?? null;
}

/// The mapping of the Discord `discord` id as `kind`.
export function byDiscord(state, kind, discord) {
  for (const entry of Object.values(state.map ?? {})) {
    if (entry.kind === kind && entry.discord === discord) return entry;
  }
  return null;
}

/// The Discord side a conversation is carried to: its channel, its webhook, and the thread it is.
export function carried(state, conversation) {
  const channel = mapping(state, "channel", conversation);
  if (channel) {
    const hook = mapping(state, "webhook", conversation);
    if (hook) return { channel: channel.discord, webhook: hook.discord, thread: null };
    const thread = mapping(state, "thread", conversation);
    if (thread) {
      const root = mapping(state, "webhook", thread.root);
      if (root) return { channel: thread.discord, webhook: root.discord, thread: thread.discord };
    }
    return null;
  }
  const thread = mapping(state, "thread", conversation);
  if (thread) {
    const hook = mapping(state, "webhook", thread.root);
    if (hook) return { channel: thread.discord, webhook: hook.discord, thread: thread.discord };
  }
  return null;
}

/// A channel there by its id, as the bridge last observed it.
export function observed(state, id) {
  return (state.channels ?? []).find((channel) => channel.id === id) ?? null;
}

/// The linked account of the workspace person `person`.
export function linkOfPerson(state, person) {
  return (state.links ?? []).find((link) => link.person === person) ?? null;
}

/// The linked account of the Discord account `user`.
export function linkOfUser(state, user) {
  return (state.links ?? []).find((link) => link.discord_user === user) ?? null;
}

/// Where Discord serves a linked account's avatar, empty when they carry none.
export function avatarUrl(link) {
  return link?.avatar
    ? `https://cdn.discordapp.com/avatars/${link.discord_user}/${link.avatar}.png?size=128`
    : "";
}

/// Who wrote a message in the workspace on Discord: the account they linked, else the name the
/// message carries.
export function authorOf(state, person, said) {
  const link = person ? linkOfPerson(state, person) : null;
  if (link) {
    return { name: link.display_name || link.user_name, avatar: avatarUrl(link) };
  }
  return { name: said || "Someone", avatar: "" };
}

/// Whether the workspace blocks the Discord account `user`'s words.
export function blocked(state, user) {
  return (state.blocked_personas ?? []).includes(user);
}

/// The custom emoji a message's words name, each with the address of its picture.
export function emojiIn(state, text) {
  const found = [];
  for (const match of String(text ?? "").matchAll(/:([a-zA-Z0-9_]{2,32}):/g)) {
    const id = state.emojis?.[match[1]];
    if (id) found.push({ name: match[1], picture: `https://cdn.discordapp.com/emojis/${id}.png` });
  }
  return found;
}

/// A call to Chat's generically bridged words.
function chat(action, args) {
  return { app: CHAT, action, args };
}

/// A call to the bridge's own record.
function self(action, args) {
  return { app: SELF, action, args };
}

/// The calls a Discord event means. Each is `{app, action, args, map?}`: `map` joins the resource
/// the call makes to the Discord id it is, written after the call answers.
export function fromDiscord(state, event) {
  if (event.source === "MESSAGE_CREATED") {
    const payload = event.payload;
    if (payload.webhook_id || payload.author?.bot) return [];
    if (blocked(state, payload.author.id)) return [];
    const channel = byDiscord(state, "channel", payload.channel_id);
    if (!channel) return [];
    const call = chat("relay", {
      to: channel.tilo,
      body: payload.content ?? "",
      by: payload.author.name,
      written_in: "Discord",
      written: payload.timestamp_ms ?? 0,
      avatar: payload.author.avatar
        ? `https://cdn.discordapp.com/avatars/${payload.author.id}/${payload.author.avatar}.png?size=128`
        : "",
      service: "Discord",
      outside_emoji: emojiIn(state, payload.content),
      reply_to: payload.reference?.message_id
        ? byDiscord(state, "message", payload.reference.message_id)?.tilo ?? null
        : null,
    });
    call.map = {
      kind: "message",
      discord: payload.id,
      origin: "discord",
      persona: payload.author.id,
    };
    return [call];
  }
  if (event.source === "MESSAGE_UPDATED") {
    const payload = event.payload;
    const found = byDiscord(state, "message", payload.id);
    if (!found) return [];
    return [chat("edit", { m: found.tilo, body: payload.content ?? "" })];
  }
  if (event.source === "MESSAGE_DELETED") {
    const payload = event.payload;
    const found = byDiscord(state, "message", payload.id);
    if (!found) return [];
    return [
      chat("delete", { m: found.tilo, to: null }),
      self("unmap", { kind: "message", tilo: found.tilo }),
    ];
  }
  if (event.source === "REACTION_ADDED" || event.source === "REACTION_REMOVED") {
    const payload = event.payload;
    const found = byDiscord(state, "message", payload.message_id);
    if (!found || blocked(state, payload.user_id)) return [];
    return [
      chat("relay_reaction", {
        m: found.tilo,
        emoji: payload.emoji?.name ?? payload.emoji?.id ?? "",
        persona: payload.user_id,
        by: payload.user_name ?? "Someone",
        given: event.source === "REACTION_ADDED",
      }),
    ];
  }
  if (event.source === "LAYOUT_CHANGED") {
    const payload = event.payload;
    const calls = [
      self("observe_layout", {
        bot_user: payload.bot_user ?? "",
        channels: payload.channels ?? [],
        roles: payload.roles ?? [],
        emojis: payload.emojis ?? [],
      }),
    ];
    // A channel Discord has and the workspace has not: Chat makes a conversation, then the
    // bridge joins the two. A channel gone from Discord is marked vanished by `observe_layout`.
    for (const channel of payload.channels ?? []) {
      if (byDiscord(state, "channel", channel.id) || channel.kind === "category") continue;
      const call = chat(channel.kind === "voice" ? "new_voice_channel" : "new_channel", {
        name: channel.name,
        category: channel.parent ? byDiscord(state, "category", channel.parent)?.tilo ?? null : null,
      });
      call.map = { kind: "channel", discord: channel.id, origin: "discord" };
      calls.push(call);
    }
    return calls;
  }
  return [];
}

/// The REST calls a change in Chat means, for a message written in a conversation the bridge
/// carries. `message` is the message as the workspace holds it, `person` its author.
export function toDiscord(state, conversation, message) {
  const there = carried(state, conversation);
  if (!there) return [];
  const author = authorOf(state, message.author, message.by);
  const files = (message.attachments ?? []).map((file) => ({
    blob: bareBlob(file.blob),
    name: file.name,
    size: file.size ?? 0,
  }));
  if (files.some((file) => file.size > MAX_UPLOAD_BYTES) || files.length > MAX_UPLOADS) return [];
  return [
    {
      op: "send",
      channel: there.channel,
      webhook: there.webhook,
      thread: there.thread,
      username: author.name,
      avatar_url: author.avatar || null,
      avatar_blob: null,
      content: fit(message.text ?? ""),
      attachments: files,
      created_ms: message.written ?? 0,
      tilo: published(message.id),
    },
  ];
}

/// The REST calls carrying a change to a message the bridge sent.
export function editToDiscord(state, conversation, message, there) {
  const carriedTo = carried(state, conversation);
  if (!carriedTo) return [];
  return [
    {
      op: "edit",
      channel: carriedTo.channel,
      webhook: carriedTo.webhook,
      message: there,
      thread: carriedTo.thread,
      content: fit(message.text ?? ""),
    },
  ];
}

/// The REST calls carrying a deletion.
export function deleteToDiscord(state, conversation, there, webhook) {
  const carriedTo = carried(state, conversation);
  if (!carriedTo) return [];
  return [
    {
      op: "delete",
      channel: carriedTo.channel,
      webhook: webhook || carriedTo.webhook,
      message: there,
      thread: carriedTo.thread,
    },
  ];
}

/// The REST calls carrying a reaction, or taking it back.
export function reactToDiscord(state, conversation, message, emoji, added, there) {
  if (!carried(state, conversation)) return [];
  return [
    {
      op: added ? "react" : "unreact",
      channel: carried(state, conversation).channel,
      message: there,
      emoji,
    },
  ];
}
