// The Discord gateway: the bridge's held connection, in the connection process alone. A process
// started for a call never opens it; such a process writes records the holder watches.
//
// The protocol is this app's own code, not a library the host carries: one WebSocket to Discord's
// gateway, Discord's identify/heartbeat/resume, and the events the planners read. The network is
// the package's `[sandbox.net]` grant, reached through the launcher; the token arrives from the
// workspace's vault and stays in this process's memory.

/// The gateway events the bridge needs: guilds, members, moderation, emoji, voice states,
/// messages, reactions, and message content.
/// `GUILDS|GUILD_MEMBERS|GUILD_MODERATION|GUILD_EMOJIS_AND_STICKERS|GUILD_VOICE_STATES|
/// GUILD_MESSAGES|GUILD_MESSAGE_REACTIONS|MESSAGE_CONTENT`.
export const INTENTS = 34_447;

/// Discord's gateway for a fresh session. `DISCORD_GATEWAY_URL` names a fake for a test.
export const GATEWAY_URL = "wss://gateway.discord.gg/?v=10&encoding=json";

/// How long to wait before reconnecting: doubling from the first to the last.
const RECONNECT_MIN_MS = 1_000;
const RECONNECT_MAX_MS = 60_000;

/// Close codes Discord uses to say the bot's token is not accepted, which reconnecting cannot fix.
const REFUSED_CLOSE = new Set([4004, 4010, 4011, 4012, 4013, 4014]);

/// Connect `client` to Discord as `token`, handing every gateway event to `onEvent`.
///
/// `make`: the client factory, given so a test can replace the gateway.
export async function connect({ make, token, onEvent, onVoice, watch, url, log = () => {} }) {
  const client = make ? make() : new Gateway({ log });
  if (onVoice) {
    // What Discord hands the bot when it is moved into a voice channel: the session and token a
    // voice connection is opened with. The gateway process is the only holder, so it passes
    // them on rather than a second process opening the gateway.
    client.on("voiceServer", (update) => onVoice(update));
    client.on("voiceState", (state) => onVoice({ state }));
  }
  client.on("messageCreate", (message) => onEvent(discordMessage("MESSAGE_CREATED", message)));
  client.on("messageUpdate", (_, message) => onEvent(discordMessage("MESSAGE_UPDATED", message)));
  client.on("messageDelete", (message) => onEvent(discordMessage("MESSAGE_DELETED", message)));
  client.on("messageReactionAdd", (reaction, user) =>
    onEvent(discordReaction("REACTION_ADDED", reaction, user)),
  );
  client.on("messageReactionRemove", (reaction, user) =>
    onEvent(discordReaction("REACTION_REMOVED", reaction, user)),
  );
  client.on("guildMemberUpdate", (_, member) => onEvent(discordMember(member)));
  client.on("layout", (guild) => onEvent(discordLayout(guild)));
  await client.login(token, url);
  if (watch) await watch();
  return client;
}

/// A message of the gateway as the planners read it.
export function discordMessage(source, message) {
  return {
    source,
    payload: message
      ? {
          id: message.id,
          channel_id: message.channelId,
          guild_id: message.guildId,
          content: message.content ?? "",
          timestamp_ms: message.createdTimestamp ?? 0,
          webhook_id: message.webhookId ?? null,
          author: {
            id: message.author?.id ?? "",
            name: message.author?.username ?? "Someone",
            avatar: message.author?.avatar ?? null,
            bot: Boolean(message.author?.bot),
          },
          reference: message.reference ?? null,
        }
      : null,
  };
}

/// A reaction of the gateway as the planners read it.
export function discordReaction(source, reaction, user) {
  return {
    source,
    payload: {
      message_id: reaction?.message?.id ?? "",
      channel_id: reaction?.message?.channelId ?? "",
      emoji: { name: reaction?.emoji?.name ?? "", id: reaction?.emoji?.id ?? "" },
      user_id: user?.id ?? "",
      user_name: user?.username ?? "Someone",
    },
  };
}

/// A member change as the planners read it.
export function discordMember(member) {
  return {
    source: "MEMBER_CHANGED",
    payload: {
      guild_id: member?.guild?.id ?? "",
      user_id: member?.id ?? "",
      roles: [...(member?.roles?.cache?.keys?.() ?? [])],
      banned: false,
      left: false,
      user: { name: member?.user?.username ?? "Someone" },
      avatar: member?.user?.avatar ?? null,
    },
  };
}

/// The channels, roles and custom emoji a server holds, as the bridge observes them.
export function discordLayout(guild) {
  const list = (value) => {
    if (!value) return [];
    if (Array.isArray(value)) return value;
    if (typeof value.values === "function") return [...value.values()];
    return Object.values(value);
  };
  return {
    source: "LAYOUT_CHANGED",
    payload: {
      bot_user: guild?.botUser ?? guild?.bot_user ?? "",
      channels: list(guild?.channels).map((channel) => ({
        id: channel.id,
        kind: channelKind(channel),
        name: channel.name ?? "",
        parent: channel.parentId ?? channel.parent_id ?? channel.parent ?? "",
        position: channel.position ?? 0,
        topic: channel.topic ?? "",
      })),
      roles: list(guild?.roles).map((role) => ({
        id: role.id,
        name: role.name ?? "",
        permissions: String(role.permissions ?? ""),
        color: role.color ?? 0,
        position: role.position ?? 0,
        managed: Boolean(role.managed),
      })),
      emojis: list(guild?.emojis).map((emoji) => ({
        id: emoji.id,
        name: emoji.name ?? "",
        animated: Boolean(emoji.animated),
      })),
    },
  };
}

/// Discord's channel type numbers, as the bridge names a channel's kind.
function channelKind(channel) {
  if (typeof channel.kind === "string") return channel.kind;
  switch (channel.type) {
    case 2:
    case 13:
      return "voice";
    case 4:
      return "category";
    case 10:
    case 11:
    case 12:
      return "thread";
    default:
      return "text";
  }
}

/// The gateway's client: one session, kept by reconnecting and resuming until the device stops
/// it. Tests hand in a fake factory instead, as `connect`'s `make`.
export class Gateway {
  #handlers = new Map();
  #socket = null;
  #token = "";
  #url = GATEWAY_URL;
  #seq = null;
  #session = null;
  #beat = null;
  #firstBeat = null;
  #acked = true;
  #attempt = 0;
  #stopped = false;
  #ready = null;
  #log;
  user = null;
  /// Where the bot is in voice, by guild: the channel it was moved into, and the session and
  /// token Discord answers with.
  voice = new Map();
  #joining = new Map();

  constructor({ log = () => {} } = {}) {
    this.#log = log;
  }

  /// Subscribe to a gateway event.
  on(name, handler) {
    const handlers = this.#handlers.get(name) ?? [];
    handlers.push(handler);
    this.#handlers.set(name, handlers);
    return this;
  }

  #emit(name, ...args) {
    for (const handler of this.#handlers.get(name) ?? []) handler(...args);
  }

  #send(payload) {
    this.#socket?.send(JSON.stringify(payload));
  }

  /// Open the session as `token` and answer once Discord has identified it. A token Discord
  /// refuses rejects; a dropped connection reconnects behind the answer.
  login(token, url = GATEWAY_URL) {
    this.#token = token;
    this.#url = url;
    this.#stopped = false;
    const ready = new Promise((resolve, reject) => {
      this.#ready = { resolve, reject };
    });
    this.#open();
    return ready;
  }

  /// Move the bot into `channel` of `guild`. Answers once Discord said both the session and the
  /// voice server, or rejects with the reason it said instead.
  joinVoice(guild, channel) {
    const key = String(guild);
    const wanted = String(channel);
    const previous = this.#joining.get(key);
    if (previous) {
      clearTimeout(previous.timer);
      previous.reject(new Error("another call was asked for first"));
      this.#joining.delete(key);
    }
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#joining.delete(key);
        reject(new Error("Discord did not answer the call in time"));
      }, 15_000);
      this.#joining.set(key, { channel: wanted, resolve, reject, timer });
      // Op 4: join the channel with the bot's microphone and speakers on.
      this.#send({ op: 4, d: { guild_id: key, channel_id: wanted, self_mute: false, self_deaf: false } });
    });
  }

  /// Move the bot out of the call it is in in `guild`.
  leaveVoice(guild) {
    const key = String(guild);
    const joining = this.#joining.get(key);
    if (joining) {
      clearTimeout(joining.timer);
      joining.reject(new Error("the call was left before it opened"));
      this.#joining.delete(key);
    }
    if (!this.voice.has(key)) return;
    this.#send({ op: 4, d: { guild_id: key, channel_id: null, self_mute: false, self_deaf: false } });
  }

  /// End the session for good.
  destroy() {
    this.#stopped = true;
    this.#stopBeat();
    this.#socket?.close(1000);
    this.#socket = null;
  }

  #open() {
    // The one gateway the package's `[sandbox.net]` grant names: a resumed session comes back to
    // it, where Discord may also name another of its addresses.
    const url = this.#url;
    let socket;
    try {
      socket = new WebSocket(url);
    } catch (error) {
      this.#log(`the Discord gateway is unreachable: ${error?.message ?? error}`);
      this.#again();
      return;
    }
    this.#socket = socket;
    socket.onmessage = (event) => this.#heard(event.data);
    socket.onerror = () => {};
    socket.onclose = (event) => {
      if (this.#socket !== socket) return;
      this.#socket = null;
      this.#stopBeat();
      this.#closed(event.code ?? 0, event.reason ?? "");
    };
  }

  #again() {
    if (this.#stopped) return;
    const wait = Math.min(RECONNECT_MAX_MS, RECONNECT_MIN_MS * 2 ** this.#attempt);
    this.#attempt += 1;
    setTimeout(() => {
      if (!this.#stopped) this.#open();
    }, wait);
  }

  #closed(code, reason) {
    if (this.#stopped) return;
    if (REFUSED_CLOSE.has(code)) {
      this.#stopped = true;
      this.#ready?.reject(
        new Error(`Discord refused the bot's token (${code}${reason ? `: ${reason}` : ""})`),
      );
      this.#ready = null;
      return;
    }
    this.#again();
  }

  #stopBeat() {
    if (this.#beat !== null) clearInterval(this.#beat);
    if (this.#firstBeat !== null) clearTimeout(this.#firstBeat);
    this.#beat = null;
    this.#firstBeat = null;
  }

  #beatNow() {
    if (!this.#acked) {
      // No answer to the last heartbeat: the connection is dead, so start it again.
      this.#socket?.close(4000);
      return;
    }
    this.#acked = false;
    this.#send({ op: 1, d: this.#seq });
  }

  #startBeat(interval) {
    this.#stopBeat();
    this.#acked = true;
    // Discord says to wait some part of the interval before the first beat, then keep its rhythm.
    this.#firstBeat = setTimeout(() => {
      this.#firstBeat = null;
      this.#beatNow();
      this.#beat = setInterval(() => this.#beatNow(), interval);
    }, Math.floor(Math.random() * interval));
  }

  #identify() {
    this.#send({
      op: 2,
      d: {
        token: this.#token,
        intents: INTENTS,
        properties: { os: "linux", browser: "tilo", device: "tilo" },
      },
    });
  }

  #resume() {
    this.#send({ op: 6, d: { token: this.#token, session_id: this.#session, seq: this.#seq } });
  }

  #heard(raw) {
    let frame;
    try {
      frame = JSON.parse(typeof raw === "string" ? raw : String(raw));
    } catch {
      return;
    }
    if (frame.s !== null && frame.s !== undefined) this.#seq = frame.s;
    switch (frame.op) {
      case 10:
        this.#startBeat(frame.d?.heartbeat_interval ?? 41_250);
        if (this.#session !== null && this.#seq !== null) this.#resume();
        else this.#identify();
        return;
      case 11:
        this.#acked = true;
        return;
      case 1:
        this.#beatNow();
        return;
      case 7:
        this.#socket?.close(4000);
        return;
      case 9:
        // Invalid session: a resumable one retries the resume, a fresh one identifies.
        this.#session = frame.d === true ? this.#session : null;
        if (this.#session === null) this.#seq = null;
        setTimeout(() => this.#socket?.close(4000), 1_000 + Math.floor(Math.random() * 4_000));
        return;
      case 0:
        this.#dispatch(frame.t, frame.d);
        return;
      default:
        return;
    }
  }

  #dispatch(type, data) {
    switch (type) {
      case "READY":
        this.#session = data?.session_id ?? null;
        this.user = data?.user ?? null;
        this.#attempt = 0;
        this.#ready?.resolve(this);
        this.#ready = null;
        return;
      case "RESUMED":
        this.#attempt = 0;
        this.#ready?.resolve(this);
        this.#ready = null;
        return;
      case "MESSAGE_CREATE":
        this.#emit("messageCreate", messageOf(data));
        return;
      case "MESSAGE_UPDATE":
        this.#emit("messageUpdate", null, messageOf(data));
        return;
      case "MESSAGE_DELETE":
        this.#emit("messageDelete", {
          id: data?.id ?? "",
          channelId: data?.channel_id ?? "",
          guildId: data?.guild_id ?? null,
        });
        return;
      case "MESSAGE_REACTION_ADD":
        this.#emit("messageReactionAdd", reactionOf(data), userOf(data));
        return;
      case "MESSAGE_REACTION_REMOVE":
        this.#emit("messageReactionRemove", reactionOf(data), userOf(data));
        return;
      case "GUILD_MEMBER_UPDATE":
        this.#emit("guildMemberUpdate", null, memberOf(data));
        return;
      case "GUILD_CREATE":
      case "GUILD_UPDATE":
        this.#emit("layout", {
          ...data,
          botUser: this.user?.id ?? "",
        });
        // A resumed session's call is where Discord still has it: the guild's voice states say
        // so before any new event does.
        for (const state of data?.voice_states ?? []) {
          this.#voiceState(data?.id ?? "", state);
        }
        return;
      case "VOICE_STATE_UPDATE":
        this.#voiceState(data?.guild_id ?? "", data);
        return;
      case "VOICE_SERVER_UPDATE":
        this.#voiceServer(data?.guild_id ?? "", data);
        return;
      default:
        return;
    }
  }

  /// A voice state: who is in which call. The bot's own state says when Discord moved it, so a
  /// call the person ended in Discord ends the bridge's side here too.
  #voiceState(guild, state) {
    const key = String(guild);
    const read = voiceStateOf(key, state);
    if (!read.guild) return;
    const held = this.voice.get(key);
    if (read.user === this.user?.id) {
      if (read.channel) {
        this.voice.set(key, { ...held, ...read });
      } else if (held) {
        this.voice.delete(key);
        const joining = this.#joining.get(key);
        if (joining) {
          clearTimeout(joining.timer);
          joining.reject(new Error("Discord ended the call"));
          this.#joining.delete(key);
        }
      }
    } else if (held?.channel === read.channel) {
      this.voice.set(key, { ...held, people: { ...(held.people ?? {}), [read.user]: read } });
    }
    this.#emit("voiceState", read);
  }

  /// The voice server Discord handed the bot for `guild`: the endpoint, token and session a
  /// voice connection opens with. Answers the waiting join once both halves are here.
  #voiceServer(guild, data) {
    const key = String(guild);
    if (data?.user_id && this.user?.id && String(data.user_id) !== this.user.id) return;
    const held = this.voice.get(key) ?? {};
    const state = held.channel ? held : null;
    const merged = {
      ...held,
      ...(state ?? {}),
      guild: key,
      endpoint: String(data?.endpoint ?? ""),
      token: String(data?.token ?? ""),
      // The voice session Discord's VOICE_STATE_UPDATE named for the bot, never the gateway's.
      session: String(held.session ?? ""),
    };
    this.voice.set(key, merged);
    this.#emit("voiceServer", merged);
    const joining = this.#joining.get(key);
    if (!joining) return;
    if (!merged.channel || !merged.endpoint || !merged.token) return;
    clearTimeout(joining.timer);
    this.#joining.delete(key);
    if (joining.channel !== merged.channel) {
      joining.reject(new Error("Discord moved the bot to another call"));
      return;
    }
    joining.resolve(merged);
  }
}

/// What one voice state says: who, in which channel, and whether they may be heard.
export function voiceStateOf(guild, data) {
  return {
    guild: String(guild ?? ""),
    user: String(data?.user_id ?? ""),
    channel: data?.channel_id ? String(data.channel_id) : "",
    session: String(data?.session_id ?? ""),
    self_mute: Boolean(data?.self_mute),
    self_deaf: Boolean(data?.self_deaf),
    mute: Boolean(data?.mute),
    deaf: Boolean(data?.deaf),
    suppress: Boolean(data?.suppress),
    bot: Boolean(data?.member?.user?.bot),
    name: String(data?.member?.user?.username ?? data?.member?.nick ?? "Someone"),
    avatar: data?.member?.user?.avatar ?? null,
  };
}

/// A gateway message as the client's callers read it.
function messageOf(data) {
  return {
    id: data?.id ?? "",
    channelId: data?.channel_id ?? "",
    guildId: data?.guild_id ?? null,
    content: data?.content ?? "",
    createdTimestamp: Date.parse(data?.timestamp ?? "") || 0,
    webhookId: data?.webhook_id ?? null,
    author: {
      id: data?.author?.id ?? "",
      username: data?.author?.username ?? "Someone",
      avatar: data?.author?.avatar ?? null,
      bot: Boolean(data?.author?.bot),
    },
    reference: data?.message_reference?.message_id
      ? { message_id: data.message_reference.message_id }
      : data?.referenced_message?.id
        ? { message_id: data.referenced_message.id }
        : null,
  };
}

function reactionOf(data) {
  return {
    message: { id: data?.message_id ?? "", channelId: data?.channel_id ?? "" },
    emoji: { name: data?.emoji?.name ?? "", id: data?.emoji?.id ?? "" },
  };
}

function userOf(data) {
  return {
    id: data?.user_id ?? "",
    username: data?.member?.user?.username ?? data?.user?.username ?? "Someone",
  };
}

function memberOf(data) {
  return {
    guild: { id: data?.guild_id ?? "" },
    id: data?.user?.id ?? "",
    roles: { cache: { keys: () => [...(data?.roles ?? [])] } },
    user: { username: data?.user?.username ?? "Someone", avatar: data?.user?.avatar ?? null },
  };
}
