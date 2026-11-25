import { test, expect } from "bun:test";
import {
  fromDiscord,
  toDiscord,
  carried,
  fit,
  bareBlob,
  avatarUrl,
  mapping,
  threadToDiscord,
  byThisBridge,
  chatChanges,
  changeTarget,
  messageOf,
  sizeOf,
  importOf,
  sidebarOf,
  SELF as SELF_APP,
} from "../src/plan.js";
import { callsOf } from "../main.js";
import { looksLikeToken, webhookSend } from "../src/rest.js";

/// The person `ada`, as a record keeps an actor's id: the text of its JSON.
const ADA = JSON.stringify({ member: { person: "ada" } });

/// The bridge's record as the workspace holds it.
function state(overrides = {}) {
  return {
    guild_id: "123",
    server: "Server",
    bot_user: "900",
    channels: [
      { id: "456", kind: "text", name: "design" },
      { id: "457", kind: "text", name: "random" },
    ],
    map: {
      "channel:tl:so.tilo.chat:design": {
        kind: "channel",
        tilo: "tl:so.tilo.chat:design",
        discord: "456",
      },
      "webhook:tl:so.tilo.chat:design": {
        kind: "webhook",
        tilo: "tl:so.tilo.chat:design",
        discord: "555",
      },
      "message:tl:so.tilo.chat:m1": {
        kind: "message",
        tilo: "tl:so.tilo.chat:m1",
        discord: "777",
        persona: "3001",
      },
    },
    links: [
      {
        discord_user: "3001",
        person: ADA,
        user_name: "ada",
        display_name: "Ada",
        avatar: "abc",
      },
    ],
    blocked_personas: ["3002"],
    emojis: { party: "22" },
    ...overrides,
  };
}

test("a message written on Discord is relayed as its author, with their picture and service", () => {
  const calls = fromDiscord(state(), {
    source: "MESSAGE_CREATED",
    payload: {
      id: "777",
      channel_id: "456",
      content: "look at this :party:!",
      timestamp_ms: 1000,
      author: { id: "3001", name: "ada", avatar: "abc" },
    },
  });
  expect(calls).toHaveLength(1);
  const [call] = calls;
  expect(call.app).toBe("so.tilo.chat");
  expect(call.action).toBe("relay");
  expect(call.args.to).toBe("tl:so.tilo.chat:design");
  expect(call.args.by).toBe("ada");
  expect(call.args.service).toBe("Discord");
  // The account ada linked wears her face and colour here.
  expect(call.args.member).toEqual({ member: { person: "ada" } });
  expect(call.args.avatar).toBe("https://cdn.discordapp.com/avatars/3001/abc.png?size=128");
  expect(call.args.outside_emoji).toEqual([
    { name: "party", picture: "https://cdn.discordapp.com/emojis/22.png" },
  ]);
  expect(call.map).toEqual({ kind: "message", discord: "777", origin: "discord", persona: "3001" });
});

test("a message's files come over for the program to keep, not as empty text", () => {
  const [call] = fromDiscord(state(), {
    source: "MESSAGE_CREATED",
    payload: {
      id: "777",
      channel_id: "456",
      content: "look",
      author: { id: "3001", name: "ada" },
      attachments: [
        { url: "https://cdn.discordapp.com/attachments/456/1/shot.png", name: "shot.png", media: "image/png", size: 3 },
      ],
    },
  });
  expect(call.attachments).toEqual([
    { url: "https://cdn.discordapp.com/attachments/456/1/shot.png", name: "shot.png", media: "image/png", size: 3 },
  ]);
  // Nothing in the relay's own arguments names a URL: the program keeps the bytes first.
  expect(call.args.attachments).toBeUndefined();
});

test("a thread started here is started on Discord under the message it answers", () => {
  const call = threadToDiscord(
    state(),
    "tl:so.tilo.chat:design",
    { id: "m1", text: "hello" },
    "777",
    "topic",
    "tl:so.tilo.chat:topic",
  );
  expect(call).toEqual([
    {
      op: "start-thread",
      channel: "456",
      message: "777",
      name: "topic",
      tilo: "tl:so.tilo.chat:topic",
    },
  ]);
  expect(
    threadToDiscord(state(), "tl:so.tilo.chat:random", { id: "m2" }, "778", "x", "t"),
  ).toEqual([]);
});

test("an account nobody linked carries the name Discord shows alone", () => {
  const unlinked = state({ links: [] });
  const [call] = fromDiscord(unlinked, {
    source: "MESSAGE_CREATED",
    payload: { id: "778", channel_id: "456", content: "hi", author: { id: "4001", name: "zoe" } },
  });
  expect(call.args.by).toBe("zoe");
  expect(call.args.member).toBeNull();
});

test("a link that names a plain person, as an older record may, is taken as that person", () => {
  const [call] = fromDiscord(state({ links: [{ discord_user: "3001", person: "ada" }] }), {
    source: "MESSAGE_CREATED",
    payload: { id: "780", channel_id: "456", content: "hi", author: { id: "3001", name: "ada" } },
  });
  expect(call.args.member).toEqual({ member: { person: "ada" } });
});

test("a blocked account, a bot and its own webhook say nothing here", () => {
  const blocked = fromDiscord(state(), {
    source: "MESSAGE_CREATED",
    payload: { id: "1", channel_id: "456", content: "hi", author: { id: "3002" } },
  });
  expect(blocked).toEqual([]);
  const bot = fromDiscord(state(), {
    source: "MESSAGE_CREATED",
    payload: { id: "2", channel_id: "456", content: "hi", author: { id: "900", bot: true } },
  });
  expect(bot).toEqual([]);
  const webhook = fromDiscord(state(), {
    source: "MESSAGE_CREATED",
    payload: { id: "3", channel_id: "456", content: "hi", webhook_id: "555", author: { id: "9" } },
  });
  expect(webhook).toEqual([]);
});

test("edits, deletions and reactions follow their mappings", () => {
  const edit = fromDiscord(state(), {
    source: "MESSAGE_UPDATED",
    payload: { id: "777", content: "changed" },
  });
  expect(edit[0].action).toBe("edit");
  expect(edit[0].args.m).toBe("tl:so.tilo.chat:m1");
  const del = fromDiscord(state(), { source: "MESSAGE_DELETED", payload: { id: "777" } });
  expect(del.map((call) => call.action)).toEqual(["delete", "unmap"]);
  const react = fromDiscord(state(), {
    source: "REACTION_ADDED",
    payload: { message_id: "777", user_id: "3001", user_name: "ada", emoji: { name: "🎉" } },
  });
  expect(react[0].action).toBe("relay_reaction");
  expect(react[0].args.given).toBe(true);
  expect(react[0].args.m).toBe("tl:so.tilo.chat:m1");
});

test("a channel Discord has is made here and joined to it", () => {
  const calls = fromDiscord(state(), {
    source: "LAYOUT_CHANGED",
    payload: { bot_user: "900", channels: [{ id: "458", kind: "text", name: "notes" }], roles: [] },
  });
  expect(calls.map((call) => call.action)).toEqual(["observe_layout"]);
  const made = importOf(
    state(),
    { channels: [{ id: "458", kind: "text", name: "notes" }] },
    { categories: [], channels: [] },
  ).find((call) => call.action === "new_channel");
  expect(made.args).toEqual({ name: "notes", category: null });
  expect(made.map).toEqual({ kind: "channel", discord: "458", origin: "discord" });
});

const ID = (id) => `tl:so.tilo.chat:${id}`;

/// The server's layout: a "Text" category holding the bridged #design, #notes and a voice
/// channel, and a "Voice" category of its own with #lobby.
const LAYOUT = {
  channels: [
    { id: "11", kind: "category", name: "Voice", position: 1 },
    { id: "10", kind: "category", name: "Text", position: 0 },
    { id: "456", kind: "text", name: "design", parent: "10", position: 0 },
    { id: "458", kind: "text", name: "notes", parent: "10", position: 1 },
    { id: "459", kind: "voice", name: "Call", parent: "10", position: 2 },
    { id: "460", kind: "text", name: "lobby", parent: "11", position: 0 },
  ],
};

test("adopting a server joins the workspace's categories and channels by name and makes the rest", () => {
  const sidebar = sidebarOf({
    channels: [
      { id: "k1", value: { name: "text", channels: [] } },
      { id: "c9", value: { name: "Notes", topic: "", messages: [] } },
      { id: "k2", value: { name: "chat", channels: [{ id: "c1", value: { name: "general", topic: "", messages: [] } }] } },
    ],
  });
  expect(sidebar.categories).toEqual([
    { id: ID("k1"), name: "text" },
    { id: ID("k2"), name: "chat" },
  ]);
  expect(sidebar.channels.map((channel) => [channel.id, channel.category])).toEqual([
    [ID("c9"), null],
    [ID("c1"), ID("k2")],
  ]);
  const calls = importOf(state({ ownership: "importing" }), LAYOUT, sidebar);
  expect(calls.map((call) => [call.action, call.args, call.categoryFrom ?? null, call.map ?? null])).toEqual([
    // The workspace's "text" category is Discord's "Text"; "Voice" is made.
    ["map", { kind: "category", tilo: ID("k1"), discord: "10", origin: "discord" }, null, null],
    ["new_category", { name: "Voice" }, null, { kind: "category", discord: "11", origin: "discord" }],
    // #notes is joined and moves into its Discord channel's category.
    ["map", { kind: "channel", tilo: ID("c9"), discord: "458", origin: "discord" }, null, null],
    ["move_channel", { ch: ID("c9"), category: ID("k1") }, null, null],
    ["new_voice_channel", { name: "Call", category: ID("k1") }, null, { kind: "channel", discord: "459", origin: "discord" }],
    // #lobby waits for the category its call makes.
    ["new_channel", { name: "lobby", category: null }, "11", { kind: "channel", discord: "460", origin: "discord" }],
  ]);
  // Planned again once everything is joined, the import finishes: #general and its category had
  // no match there and stay the workspace's own.
  const joined = state({
    ownership: "importing",
    map: [
      { kind: "channel", tilo: ID("design"), discord: "456" },
      { kind: "category", tilo: ID("k1"), discord: "10" },
      { kind: "category", tilo: ID("k3"), discord: "11" },
      { kind: "channel", tilo: ID("c9"), discord: "458" },
      { kind: "channel", tilo: ID("v1"), discord: "459" },
      { kind: "channel", tilo: ID("c2"), discord: "460" },
    ],
  });
  expect(importOf(joined, LAYOUT, sidebar)).toEqual([
    { app: SELF_APP, action: "finish_import", args: { tilo_only: [ID("k2"), ID("c1")] } },
  ]);
  expect(importOf({ ...joined, ownership: "adopted" }, LAYOUT, sidebar)).toEqual([]);
});

test("a bridge of one channel joins that one alone, outside any category", () => {
  const calls = importOf(state({ only_channel: "458" }), LAYOUT, { categories: [], channels: [] });
  expect(calls.map((call) => [call.action, call.args])).toEqual([
    ["new_channel", { name: "notes", category: null }],
  ]);
});

test("a workspace message goes through the webhook, cut where Discord takes it", () => {
  const ops = toDiscord(state(), "tl:so.tilo.chat:design", {
    id: "m9",
    text: "word ".repeat(600),
    author: ADA,
    written: 1_700_000_000_123,
    attachments: [{ blob: "tilo-blob://abc123", name: "a.png", size: 10 }],
  });
  expect(ops).toHaveLength(1);
  const [op] = ops;
  expect(op.channel).toBe("456");
  expect(op.webhook).toBe("555");
  expect(op.username).toBe("Ada");
  expect(op.attachments).toEqual([
    { blob: "abc123", name: "a.png", media: "", size: 10 },
  ]);
  expect([...op.content].length).toBe(2000);
  expect(op.content.endsWith("…")).toBe(true);
  expect(op.tilo).toBe("tl:so.tilo.chat:m9");
  // An absent thread and blob are none rather than empty text, and the time the message was
  // written here goes with it.
  expect(op.thread).toBeNull();
  expect(op.avatar_blob).toBeNull();
  expect(op.created_ms).toBe(1_700_000_000_123);
});

test("a conversation is carried through its channel and webhook, a thread through its root", () => {
  const there = carried(state(), "tl:so.tilo.chat:design");
  expect(there).toEqual({ channel: "456", webhook: "555", thread: null });
  const threaded = state({
    map: {
      ...state().map,
      "thread:tl:so.tilo.chat:t1": {
        kind: "thread",
        tilo: "tl:so.tilo.chat:t1",
        discord: "888",
        root: "tl:so.tilo.chat:design",
      },
    },
  });
  expect(carried(threaded, "tl:so.tilo.chat:t1")).toEqual({
    channel: "888",
    webhook: "555",
    thread: "888",
  });
});

test("a call keeps what it made joined to the Discord id it was", async () => {
  const done = [];
  const ws = {
    async do(address, args) {
      done.push({ address, args });
      return { id: "made1" };
    },
  };
  const calls = callsOf(ws);
  await calls.do({
    app: "so.tilo.chat",
    action: "relay",
    args: { to: "tl:so.tilo.chat:design", body: "hi" },
    map: { kind: "message", discord: "777", origin: "discord", persona: "3001" },
  });
  expect(done[0].address).toBe("so.tilo.chat#relay");
  expect(done[1].address).toBe("so.tilo.discord.preview#map");
  expect(done[1].args.tilo).toBe("tl:so.tilo.chat:made1");
  expect(done[1].args.discord).toBe("777");
});

test("the token reaches nothing but the gateway's release", async () => {
  const token = "MTIz.GxYz.abcdefghij0123456789AB";
  expect(looksLikeToken(token)).toBe(true);
  const done = [];
  const released = [];
  const ws = {
    async do(address, args) {
      done.push(JSON.stringify({ address, args }));
      return { id: "x" };
    },
    vault: {
      async release(item) {
        released.push(item);
        return token;
      },
      async webCall(request) {
        done.push(JSON.stringify(request));
        return { status: 200, body: "{}" };
      },
    },
  };
  const calls = callsOf(ws);
  const item = "vault.item:discord-token";
  expect(await calls.release(item)).toBe(token);
  for (const call of fromDiscord(state(), {
    source: "MESSAGE_CREATED",
    payload: {
      id: "777",
      channel_id: "456",
      content: "hi",
      author: { id: "3001", name: "ada", avatar: "abc" },
    },
  })) {
    await calls.do(call);
  }
  for (const op of toDiscord(state(), "tl:so.tilo.chat:design", {
    id: "m9",
    text: "hi",
    author: ADA,
  })) {
    await calls.webCall({ url: "https://discord.com/api/v10/x", key: item, method: "POST", body: JSON.stringify(op) });
  }
  expect(released).toEqual([item]);
  for (const written of done) {
    expect(written.includes(token)).toBe(false);
  }
  expect(JSON.stringify(state()).includes(token)).toBe(false);
});

test("the helpers cut to Discord's bounds", () => {
  expect(fit("short")).toBe("short");
  expect([...fit("x".repeat(2500))].length).toBe(2000);
  expect(bareBlob("tilo-blob://ABC")).toBe("ABC");
  expect(bareBlob("plain")).toBe("plain");
  expect(
    webhookSend(
      { webhook: "1", tokenItem: "v", origin: "http://discord.test/api/v10" },
      { attachments: [], content: "hi" },
    ).url,
  ).toBe("http://discord.test/api/v10/webhooks/1?wait=true");
  expect(
    avatarUrl({ discord_user: "1", avatar: "h" }),
  ).toBe("https://cdn.discordapp.com/avatars/1/h.png?size=128");
  expect(avatarUrl(null)).toBe("");
  expect(mapping(state(), "channel", "tl:so.tilo.chat:design").discord).toBe("456");
});

test("a change names the message it moved and who made it, and a bridged one is carried", () => {
  const event = {
    kind: "change",
    path: "chat",
    actor: { by: { member: { person: "ada" } }, app: "so.tilo.chat" },
    resources: [
      { path: "chat", app: "so.tilo.chat", record: "root-lmdoxbnn", item: "c1/messages/m1" },
      { path: "notes", app: "so.tilo.notes", record: "notes" },
    ],
  };
  const bridged = state({
    map: {
      ...state().map,
      "channel:tl:so.tilo.chat:root-lmdoxbnn/c1": {
        kind: "channel",
        tilo: "tl:so.tilo.chat:root-lmdoxbnn/c1",
        discord: "458",
      },
      "webhook:tl:so.tilo.chat:root-lmdoxbnn/c1": {
        kind: "webhook",
        tilo: "tl:so.tilo.chat:root-lmdoxbnn/c1",
        discord: "559",
      },
    },
  });
  expect(byThisBridge(event)).toBe(false);
  const changes = chatChanges(event);
  expect(changes).toHaveLength(1);
  expect(changeTarget(bridged, changes[0])).toEqual({
    conversation: "tl:so.tilo.chat:root-lmdoxbnn/c1",
    message: "tl:so.tilo.chat:root-lmdoxbnn/c1/messages/m1",
    id: "m1",
    record: "root-lmdoxbnn",
    item: "c1/messages/m1",
    app: "chat",
  });
  // A record the conversation is its own document of reads the same way.
  const own = {
    kind: "change",
    path: "chat",
    resources: [
      { path: "chat", app: "so.tilo.chat", record: "root-lmdoxbnn/c1", item: "messages/m1" },
    ],
  };
  expect(changeTarget(bridged, chatChanges(own)[0]).conversation).toBe(
    "tl:so.tilo.chat:root-lmdoxbnn/c1",
  );
  // The item may be the message's own id, under the conversation's record.
  expect(
    changeTarget(bridged, {
      path: "chat",
      app: "so.tilo.chat",
      record: "root-lmdoxbnn/c1",
      item: "m2",
    }),
  ).toMatchObject({
    conversation: "tl:so.tilo.chat:root-lmdoxbnn/c1",
    message: "tl:so.tilo.chat:root-lmdoxbnn/c1/messages/m2",
    id: "m2",
  });
  // A message in a conversation the bridge carries nowhere is no change of its own.
  expect(
    changeTarget(state(), {
      path: "chat",
      app: "so.tilo.chat",
      record: "root-lmdoxbnn",
      item: "c9/messages/m1",
    }),
  ).toBe(null);
});

test("the bridge's own writes and changes that are no change at all are not carried", () => {
  expect(
    byThisBridge({ actor: { by: { app: "so.tilo.discord.preview" }, app: "so.tilo.discord.preview" } }),
  ).toBe(true);
  expect(byThisBridge({ actor: { by: { member: { person: "ada" } }, app: "so.tilo.chat" } })).toBe(
    false,
  );
  expect(byThisBridge({ kind: "presence", here: [] })).toBe(false);
  expect(chatChanges({ kind: "presence" })).toEqual([]);
  // A change of something that is not a message of a carried conversation is no message.
  expect(
    changeTarget(state(), {
      path: "chat",
      app: "so.tilo.chat",
      record: "root-lmdoxbnn",
      item: "c1",
    }),
  ).toBe(null);
});

test("a message reads in the words the planners use", () => {
  const message = messageOf(
    {
      body: "hello there",
      by: "Ada",
      written: 1000,
      attachments: [{ id: "a1", value: { name: "shot.png", media: "image/png", size: 3, blob: "tilo-blob://abc" } }],
      reactions: { "🎉": [ADA], "👀": [] },
      thread: { id: "root-lmdoxbnn/c1/threads/t1" },
    },
    { id: "root-lmdoxbnn/c1/messages/m1", writer: ADA, at: 2000 },
  );
  expect(message.text).toBe("hello there");
  expect(message.author).toBe(ADA);
  expect(message.by).toBe("Ada");
  expect(message.written).toBe(1000);
  expect(message.attachments).toEqual([{ blob: "tilo-blob://abc", name: "shot.png", size: 3 }]);
  expect(message.reactions).toEqual(["🎉"]);
  expect(message.thread).toBe("root-lmdoxbnn/c1/threads/t1");
});

test("a webhook post with files names workspace content as parts, never the bytes", () => {
  const op = {
    op: "send",
    webhook: "555",
    thread: null,
    username: "Ada",
    avatar_url: null,
    content: "look",
    attachments: [
      { blob: "abc123", name: "a.png", media: "image/png", size: 10 },
    ],
  };
  const origin = "http://discord.test/api/v10";
  const call = webhookSend({ webhook: "555", tokenItem: "v", origin }, op);
  expect(call.url).toBe(`${origin}/webhooks/555?wait=true`);
  expect(call.method).toBe("POST");
  expect(call.body).toBeUndefined();
  expect(call.headers).toBeUndefined();
  expect(call.bodyParts[0].name).toBe("payload_json");
  expect(call.bodyParts[0].json.content).toBe("look");
  expect(call.bodyParts[0].json.attachments).toEqual([
    { id: 0, filename: "a.png" },
  ]);
  expect(call.bodyParts[1]).toEqual({
    name: "files[0]",
    content: {
      app: "so.tilo.chat",
      hash: "abc123",
      filename: "a.png",
      contentType: "image/png",
    },
  });
  // No file: the same call as before, its payload as JSON.
  const plain = webhookSend({ webhook: "555", tokenItem: "v", origin }, {
    ...op,
    attachments: [],
  });
  expect(plain.bodyParts).toBeUndefined();
  expect(plain.headers["content-type"]).toBe("application/json");
  expect(JSON.parse(plain.body).content).toBe("look");
  // Nothing the workspace writes pings `@everyone`, `@here` or a role there.
  expect(JSON.parse(plain.body).allowed_mentions).toEqual({ parse: ["users"] });
  expect(call.bodyParts[0].json.allowed_mentions).toEqual({ parse: ["users"] });
});

test("a file Discord does not take goes as its name and size, and the message still goes", () => {
  const files = Array.from({ length: 11 }, (_, n) => ({
    blob: `tilo-blob://f${n}`,
    name: `f${n}.png`,
    size: 10,
  }));
  files[2] = { blob: "tilo-blob://big", name: "talk.mp4", size: 123_400_000 };
  const [op] = toDiscord(state(), "tl:so.tilo.chat:design", {
    id: "m9",
    text: "the recording",
    author: ADA,
    attachments: files,
  });
  expect(op.attachments.map((file) => file.name)).toEqual(
    ["f0", "f1", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10"].map((name) => `${name}.png`),
  );
  expect(op.content).toBe("the recording\n📎 talk.mp4 (123 MB), in the workspace");
  expect(sizeOf(943)).toBe("943 B");
  expect(sizeOf(1_400_000)).toBe("1.4 MB");
});
