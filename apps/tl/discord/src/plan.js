// What a Discord event and a change in the workspace mean for both sides.
//
// The planners are pure: they answer the calls to make, each naming the app, the action and its
// arguments, so the same event gives the same calls wherever it is planned and a test can read
// them. Discord's own records keep what joins the two sides; Chat keeps conversations and the
// generic words a bridge uses (a relayed author, an outside id, a service).

/// Chat's app id and this app's: the id its package declares, which the launcher names to every
/// process it starts.
export const CHAT = "so.tilo.chat";
export const SELF = process.env.TILO_PACKAGE_ID || "so.tilo.discord";

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

/// Every mapping of the bridge's record: the account's `map` as the record read answers it (a
/// list), or an object keyed `kind:tilo` as an older answer wrote it.
export function mappings(state) {
  const map = state?.map ?? [];
  if (Array.isArray(map)) return map;
  if (map !== null && typeof map === "object") return Object.values(map);
  return [];
}

/// The mapping of `tilo` as `kind` in the bridge's record.
export function mapping(state, kind, tilo) {
  return mappings(state).find((entry) => entry.kind === kind && entry.tilo === tilo) ?? null;
}

/// The mapping of the Discord `discord` id as `kind`.
export function byDiscord(state, kind, discord) {
  return mappings(state).find((entry) => entry.kind === kind && entry.discord === discord) ?? null;
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

/// Whether this bridge made a change: an app acting by itself, this one. What the bridge wrote
/// is never carried back to Discord, so nothing echoes.
export function byThisBridge(event) {
  return (event?.actor?.app ?? "") === SELF;
}

/// The Chat resources a change names: each with the conversation's record and the item that
/// moved. A watch of the whole workspace names every record of it; one of a path names none,
/// which is not what the bridge follows.
export function chatChanges(event) {
  if (event?.kind !== "change") return [];
  return (event.resources ?? []).filter(
    (resource) =>
      resource !== null &&
      typeof resource === "object" &&
      (resource.app === CHAT || resource.path === "chat") &&
      typeof resource.record === "string" &&
      resource.record !== "" &&
      typeof resource.item === "string" &&
      resource.item !== "",
  );
}

/// What a change of a Chat record says: the conversation it is in and the item in it that
/// changed. `record` is the conversation's record, which may be the conversation itself
/// (`root-…/c1`) or the app's conversations root, with the conversation the item's path before
/// `messages/`; the item is the message's path there, or the message's own id. `null` for a
/// change in a conversation the bridge carries nowhere.
export function changeTarget(state, resource) {
  const record = String(resource?.record ?? "");
  const item = String(resource?.item ?? "");
  if (!record || !item) return null;
  const at = /^(?:(.*?)\/)?messages\/([^/]+)$/.exec(item);
  const conversation = published(at?.[1] ? `${record}/${at[1]}` : record);
  if (!carried(state, conversation)) return null;
  const id = at ? at[2] : item.split("/").pop();
  return {
    conversation,
    message: published(at ? `${record}/${item}` : `${record}/messages/${item}`),
    id,
    record,
    item,
    app: typeof resource.path === "string" && resource.path !== "" ? resource.path : "chat",
  };
}

/// A message as its record reads it, in the words the planners use: what it says, who wrote it,
/// the files it carries, the emoji people gave it and the thread it started. A read answers
/// collections as the entries an action takes them by, `{id, value}`.
export function messageOf(fields, { id = "", writer = null, said = "", at = 0 } = {}) {
  const read = fields !== null && typeof fields === "object" ? fields : {};
  return {
    id,
    text: wordsOf(read.text ?? read.body),
    author: writer,
    by: typeof read.by === "string" && read.by !== "" ? read.by : said,
    written: numberOr(read.written, at),
    attachments: valuesOf(read.attachments).map((file) => ({
      blob: typeof file?.blob === "string" ? file.blob : "",
      name: typeof file?.name === "string" ? file.name : "",
      size: numberOr(file?.size, 0),
    })),
    reactions: reactionsOf(read.reactions),
    thread: referenceOf(read.thread),
  };
}

/// The words of a message's body: its text, or its paragraphs' as a rich text reads.
function wordsOf(value) {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) {
    return value
      .map(wordsOf)
      .filter((words) => words !== "")
      .join("\n");
  }
  if (value !== null && typeof value === "object") {
    for (const key of ["text", "body", "plain", "value"]) {
      if (typeof value[key] === "string") return value[key];
    }
  }
  return "";
}

/// A collection as a read answers it: each entry's value, or the values alone.
function valuesOf(collection) {
  if (!Array.isArray(collection)) return [];
  return collection
    .map((entry) =>
      entry !== null && typeof entry === "object" && "value" in entry ? entry.value : entry,
    )
    .filter((value) => value !== null && value !== undefined);
}

/// The emoji a message's people gave it, each once, for the reactions a map of emoji to them
/// reads as.
function reactionsOf(reactions) {
  if (reactions === null || typeof reactions !== "object" || Array.isArray(reactions)) return [];
  return Object.entries(reactions)
    .filter(([emoji, people]) => emoji !== "" && (Array.isArray(people) ? people.length > 0 : people != null))
    .map(([emoji]) => emoji);
}

/// The id a reference field reads as, or `null`. The record keeps it as the item it names, whose
/// id is the store's (`root-…/c1/threads/t1`) or the reference's own.
function referenceOf(value) {
  if (typeof value === "string") return value === "" ? null : value;
  if (value === null || typeof value !== "object") return null;
  const id = value.id ?? value.target?.id ?? null;
  return typeof id === "string" && id !== "" ? id : null;
}

/// `value` as a number, or `fallback` where it is not one.
function numberOr(value, fallback) {
  const number = Number(value);
  return Number.isFinite(number) && number !== 0 ? number : fallback;
}

/// A channel there by its id, as the bridge last observed it.
export function observed(state, id) {
  return (state.channels ?? []).find((channel) => channel.id === id) ?? null;
}

/// The linked account of the workspace person `person`.
export function linkOfPerson(state, person) {
  return (state.links ?? []).find((link) => link.person === person) ?? null;
}

/// The person a link names, as an action takes one: the record keeps the person's own id, whose
/// text is the actor's JSON (`{"member":{"person":"ada"}}`). A link that names a plain person, as
/// an older record may, is taken as that person.
export function actorOf(person) {
  if (!person) return null;
  try {
    const actor = JSON.parse(person);
    if (actor && typeof actor === "object") return actor;
  } catch {
    // Not an actor's JSON: a plain person's id, below.
  }
  return { member: { person } };
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
    // An account linked to a member here wears that member's face and colour; one nobody linked
    // is the account's own face alone.
    const linked = linkOfUser(state, payload.author.id);
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
      member: actorOf(linked?.person),
    });
    // The files it carries come over as the bytes Discord serves: the program fetches each and
    // keeps it in the workspace, then hands the message on with them.
    call.attachments = payload.attachments ?? [];
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
    media: file.media ?? "",
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

/// The call carrying a thread started here on a message the bridge sent: Discord starts it under
/// the same message, so its replies stay in one place on both sides.
export function threadToDiscord(state, conversation, message, there, name, tilo) {
  const carriedTo = carried(state, conversation);
  if (!carriedTo || !there) return [];
  return [
    {
      op: "start-thread",
      channel: carriedTo.channel,
      message: there,
      name: name || "topic",
      tilo: published(tilo ?? message?.id ?? ""),
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

// ---- voice -------------------------------------------------------------------------------

/// Every paired voice channel the bridge reaches: a Discord voice channel and the workspace's
/// voice channel it is one call with, by the Discord channel's id.
export function voicePairs(state) {
  const pairs = [];
  for (const m of Object.values(state.map ?? {})) {
    if (m?.kind !== "channel") continue;
    const seen = observed(state, m.discord);
    if (seen?.kind !== "voice") continue;
    if (state.only_channel && state.only_channel !== m.discord) continue;
    pairs.push({ discord: m.discord, tilo: m.tilo });
  }
  pairs.sort((a, b) => (a.discord < b.discord ? -1 : a.discord > b.discord ? 1 : 0));
  return pairs;
}

/// Who Discord says is in `channel` of `guild`, the bridge's own account left out: their account
/// id, the name Discord shows, where their avatar is served and the hash of it in the
/// workspace's content store when the bridge kept it, and whether their microphone or ear is
/// off. What a call's bridged participants are made of.
export function callPeople(states, { guild, channel, self, avatars }) {
  const people = [];
  for (const state of states ?? []) {
    if (String(state.guild) !== String(guild)) continue;
    if (String(state.channel) !== String(channel)) continue;
    if (!state.user || state.user === self) continue;
    const avatar_url = state.avatar
      ? `https://cdn.discordapp.com/avatars/${state.user}/${state.avatar}.png?size=128`
      : "";
    const person = {
      account: String(state.user),
      name: state.name || "Someone",
      avatar_url,
      muted: Boolean(state.mute) || Boolean(state.self_mute),
      deafened: Boolean(state.deaf) || Boolean(state.self_deaf),
    };
    const hash = avatar_url && avatars?.get(avatar_url);
    if (hash) person.avatar = hash;
    people.push(person);
  }
  people.sort((a, b) => (a.account < b.account ? -1 : a.account > b.account ? 1 : 0));
  return people;
}
