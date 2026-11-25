// A workspace bridged to a fake Discord: a message written on Discord reaches the linked
// conversation as its author, a message written here reaches the webhook under the linked
// account's name, and an edit, a deletion and a reaction follow it there.

import { test, expect } from "bun:test";
import { adoptServer, readBack, run } from "../main.js";

/// Wait until `check()` is true, or fail the test after `timeout`.
async function until(check, what, timeout = 3_000) {
  const end = Date.now() + timeout;
  while (!check()) {
    if (Date.now() > end) throw new Error(`waited for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

/// The person `ada`, as a record keeps an actor's id: the text of its JSON.
const ADA = JSON.stringify({ member: { person: "ada" } });

/// The bridged conversation, and the record one of its messages is kept in.
const CONVERSATION = "tl:so.tilo.chat:root-lmdoxbnn/c1";
const RECORD = "root-lmdoxbnn/c1";

/// The bridge's record as the workspace holds it: one server, its one channel and webhook, and
/// the account ada linked.
function account(overrides = {}) {
  return {
    guild_id: "123",
    server: "Server",
    bot_user: "900",
    token_item: "vault.item:discord-token",
    channels: [{ id: "456", kind: "text", name: "design" }],
    map: [
      { kind: "channel", tilo: CONVERSATION, discord: "456" },
      { kind: "webhook", tilo: CONVERSATION, discord: "555" },
    ],
    links: [
      {
        discord_user: "3001",
        person: ADA,
        user_name: "ada",
        display_name: "Ada",
        avatar: "abc",
      },
    ],
    ...overrides,
  };
}

/// One change as the SDK's watch reports it: generic words, the record it is of and the item in
/// it that moved, and who made it.
function change({ record = RECORD, item, actor = { by: JSON.parse(ADA) }, time = 1000, what } = {}) {
  return {
    kind: "change",
    ...(what ? { what } : {}),
    path: "chat",
    change: `change-${item}`,
    sentence: "a message moved",
    time_ms: time,
    actor,
    resources: [{ path: "chat", app: "so.tilo.chat", record, item }],
  };
}

/// The fields a message reads as.
function messageFields({
  body = "hello",
  by = "",
  written = 0,
  attachments = [],
  reactions = {},
  thread = null,
} = {}) {
  return { body, by, written, attachments, reactions, thread };
}

/// A workspace as the bridge's process sees it: the accounts record, the messages it reads by
/// path, the calls it makes and the web requests it sends. `watch` answers the events the test
/// pushes, each branch of the watch on its own: the accounts record, the setup, the workspace's
/// changes.
function fakeWorkspace({
  accounts = [account()],
  messages = {},
  webAnswer = null,
  webhooks = [{ id: "555", name: "Tilo", token: "tok" }],
} = {}) {
  const done = [];
  const requests = [];
  // The channel's webhooks Discord lists and makes, apart from the messages sent through them.
  const hooks = [];
  const released = [];
  const secret = "MTIz.GxYz.abcdefghij0123456789AB";
  const held = typeof accounts === "function" ? accounts : () => accounts;
  const reads = new Map(Object.entries(messages));
  const queues = new Map();
  const wakers = new Set();
  let closed = false;
  const tell = () => {
    for (const wake of wakers) wake();
    wakers.clear();
  };
  const ws = {
    async read(path) {
      if (path === "so.tilo.discord.preview/accounts") return { accounts: held() };
      const fields = reads.get(path);
      if (!fields) throw new Error(`no message at ${path}`);
      return { fields };
    },
    async do(address, args) {
      done.push({ address, args });
      return { id: "made1" };
    },
    async *watch(path) {
      const key = path ?? "";
      for (;;) {
        const queue = queues.get(key) ?? [];
        while (queue.length) yield queue.shift();
        if (closed) return;
        await new Promise((wake) => wakers.add(wake));
      }
    },
    files: {
      async upload() {
        return "tilo.content:abc123";
      },
    },
    vault: {
      async release(item) {
        released.push(item);
        return secret;
      },
      async webCall(request) {
        if (/\/channels\/[^/]+\/webhooks$/.test(request.url)) {
          hooks.push(request);
          if (request.method === "GET") return { status: 200, body: JSON.stringify(webhooks) };
          return { status: 200, body: JSON.stringify({ id: "556", name: "Tilo", token: "made" }) };
        }
        requests.push(request);
        if (webAnswer) return webAnswer(request);
        return { status: 200, body: JSON.stringify({ id: "9001", channel_id: "456" }) };
      },
    },
  };
  const push = (key, event) => {
    const queue = queues.get(key) ?? [];
    queue.push(event);
    queues.set(key, queue);
    tell();
  };
  return {
    tilo: { workspace: () => ws },
    done,
    requests,
    hooks,
    released,
    reads,
    /// A change of the whole workspace's records.
    say: (event) => push("", event),
    /// An event of a watched path: the accounts record, or the app's own.
    said: (path, event) => push(path, event),
    /// What a message reads as from now on.
    message: (path, fields) => reads.set(path, fields),
    finish() {
      closed = true;
      tell();
    },
  };
}

/// A fake gateway client: the bridge wires its handlers, and the test says what arrives.
function fakeGateway() {
  const handlers = new Map();
  const client = {
    on(name, handler) {
      handlers.set(name, [...(handlers.get(name) ?? []), handler]);
      return this;
    },
    /// Whether the bridge has wired `name`, so a test emits only once something listens.
    has(name) {
      return handlers.has(name);
    },
    async login() {
      return this;
    },
    emit(name, ...args) {
      for (const handler of handlers.get(name) ?? []) handler(...args);
    },
  };
  return client;
}

test("a message goes each way, and an edit, a deletion and a reaction follow", async () => {
  const gateway = fakeGateway();
  const workspace = fakeWorkspace();
  const running = run({
    tilo: workspace.tilo,
    makeClient: () => gateway,
    log: () => {},
  });

  // A message written on Discord reaches the linked conversation as its author.
  await until(() => gateway.has("messageCreate"), "the gateway");
  gateway.emit("messageCreate", {
    id: "777",
    channelId: "456",
    guildId: "123",
    content: "look at this :party:!",
    createdTimestamp: 1000,
    webhookId: null,
    author: { id: "3001", username: "ada", avatar: "abc", bot: false },
    reference: null,
  });
  await until(
    () => workspace.done.some((call) => call.address === "so.tilo.chat#relay"),
    "the relay",
  );
  const relay = workspace.done.find((call) => call.address === "so.tilo.chat#relay");
  expect(relay.args.to).toBe(CONVERSATION);
  expect(relay.args.by).toBe("ada");
  expect(relay.args.service).toBe("Discord");
  expect(relay.args.avatar).toBe("https://cdn.discordapp.com/avatars/3001/abc.png?size=128");
  expect(
    workspace.done.some(
      (call) => call.address === "so.tilo.discord.preview#map" && call.args.discord === "777",
    ),
  ).toBe(true);

  // A message written here reaches the webhook, and what it made is joined to the message. The
  // bridge's own action takes it first, so Discord hears only what its guards allow.
  const message = `chat/${RECORD}/messages/m1`;
  workspace.message(message, messageFields({ body: "hello" }));
  workspace.say(change({ item: "messages/m1" }));
  await until(() => workspace.requests.length >= 1, "the send");
  const carried = workspace.done.find((call) => call.address === "so.tilo.discord.preview#send_there");
  expect(carried?.args.op).toMatchObject({ op: "send", channel: "456", content: "hello" });
  const send = workspace.requests[0];
  expect(send.url).toBe("https://discord.com/api/v10/webhooks/555/tok?wait=true");
  expect(send.method).toBe("POST");
  const sent = JSON.parse(send.body);
  expect(sent.username).toBe("Ada");
  expect(sent.avatar_url).toBe("https://cdn.discordapp.com/avatars/3001/abc.png?size=128");
  expect(sent.content).toBe("hello");
  await until(
    () => workspace.done.some((call) => call.address === "so.tilo.discord.preview#map" &&
      call.args.discord === "9001"),
    "the mapping",
  );
  const mapped = workspace.done.find(
    (call) => call.address === "so.tilo.discord.preview#map" && call.args.discord === "9001",
  );
  expect(mapped.args.tilo).toBe(`${CONVERSATION}/messages/m1`);
  expect(mapped.args.kind).toBe("message");

  // An edit follows the message it is about.
  workspace.message(message, messageFields({ body: "hello again" }));
  workspace.say(change({ item: "messages/m1", time: 2000 }));
  await until(() => workspace.requests.length >= 2, "the edit");
  expect(
    workspace.done.some((call) => call.address === "so.tilo.discord.preview#edit_there"),
  ).toBe(true);
  const edit = workspace.requests[1];
  expect(edit.url).toBe("https://discord.com/api/v10/webhooks/555/tok/messages/9001");
  expect(edit.method).toBe("PATCH");
  expect(JSON.parse(edit.body).content).toBe("hello again");

  // A reaction follows it too.
  workspace.message(message, messageFields({ body: "hello again", reactions: { "🎉": [ADA] } }));
  workspace.say(change({ item: "messages/m1", time: 3000 }));
  await until(() => workspace.requests.length >= 3, "the reaction");
  expect(
    workspace.done.some((call) => call.address === "so.tilo.discord.preview#react_there"),
  ).toBe(true);
  const reacted = workspace.requests[2];
  expect(reacted.method).toBe("PUT");
  expect(reacted.url).toBe(
    `https://discord.com/api/v10/channels/456/messages/9001/reactions/${encodeURIComponent("🎉")}`,
  );

  // And a deletion, once the change says the message is gone from the workspace: no read is
  // needed for that, even while the record may still read.
  workspace.say(change({ item: "messages/m1", time: 4000, what: "removed" }));
  await until(() => workspace.requests.length >= 4, "the deletion");
  const deleted = workspace.requests[3];
  expect(deleted.method).toBe("DELETE");
  expect(deleted.url).toBe("https://discord.com/api/v10/webhooks/555/tok/messages/9001");
  expect(
    workspace.done.some(
      (call) => call.address === "so.tilo.discord.preview#unmap" &&
        call.args.tilo === `${CONVERSATION}/messages/m1`,
    ),
  ).toBe(true);

  // The token was released to the gateway alone, never into a record or a request.
  expect(workspace.released).toEqual(["vault.item:discord-token"]);
  for (const request of workspace.requests) {
    expect(JSON.stringify(request)).not.toContain("MTIz.GxYz");
  }
  for (const call of workspace.done) {
    expect(JSON.stringify(call)).not.toContain("MTIz.GxYz");
  }

  workspace.finish();
  await running;
});

test("a generic change sends a new message once, and the bridge's own relay sends nothing back", async () => {
  const workspace = fakeWorkspace();
  const running = run({
    tilo: workspace.tilo,
    makeClient: () => fakeGateway(),
    log: () => {},
  });
  await until(() => workspace.released.length > 0 || workspace.done.length > 0, "the bridge");

  // A person writes here: one change, one send.
  workspace.message(`chat/${RECORD}/messages/m1`, messageFields({ body: "hello there" }));
  workspace.say(change({ item: "messages/m1" }));
  await until(() => workspace.requests.length >= 1, "the first send");
  expect(workspace.requests).toHaveLength(1);
  expect(JSON.parse(workspace.requests[0].body).content).toBe("hello there");

  // The bridge relays a message Discord wrote into this conversation: a change of the same
  // record the app made itself. It is not carried back.
  workspace.message(`chat/${RECORD}/messages/m2`, messageFields({ body: "said on Discord", by: "Ada" }));
  workspace.say(
    change({
      item: "messages/m2",
      actor: { by: { app: "so.tilo.discord.preview" }, app: "so.tilo.discord.preview" },
      time: 2000,
    }),
  );

  // Another person writes here: exactly one more send, and it is theirs.
  workspace.message(`chat/${RECORD}/messages/m3`, messageFields({ body: "written here" }));
  workspace.say(change({ item: "messages/m3", time: 3000 }));
  await until(() => workspace.requests.length >= 2, "the second send");
  expect(workspace.requests).toHaveLength(2);
  expect(workspace.requests.map((request) => JSON.parse(request.body).content)).toEqual([
    "hello there",
    "written here",
  ]);
  expect(
    workspace.done.filter((call) => call.address === "so.tilo.discord.preview#send_there"),
  ).toHaveLength(2);

  workspace.finish();
  await running;
});

test("a carried account finds the bot's token in the vault instead of asking for it", async () => {
  const found = [];
  const released = [];
  let finish;
  const finished = new Promise((resolve) => (finish = resolve));
  // A server the move carried over: its record names no token item, because the token was an
  // old keeper entry the device's vault adopted under the app and purpose it was kept for.
  const carried = { ...account(), token_item: "" };
  const ws = {
    async read() {
      return { accounts: [carried] };
    },
    async do() {
      return { id: "made1" };
    },
    async *watch() {
      await finished;
    },
    vault: {
      async find(tags) {
        found.push(tags);
        return "vault.item:adopted-token";
      },
      async release(item) {
        released.push(item);
        return "MTIz.GxYz.abcdefghij0123456789AB";
      },
      async webCall() {
        return { status: 200, body: "{}" };
      },
    },
  };
  const gateway = fakeGateway();
  const running = run({ tilo: { workspace: () => ws }, makeClient: () => gateway, log: () => {} });
  await until(() => released.length > 0, "the token");
  expect(found).toEqual([{ app: "so.tilo.discord", purpose: "bot-token" }]);
  expect(released).toEqual(["vault.item:adopted-token"]);
  expect(carried.token_item).toBe("vault.item:adopted-token");
  finish();
  await running;
});

test("a file posted on Discord is kept here and the message carries it", async () => {
  const fetched = [];
  const workspace = fakeWorkspace();
  const fetch = async (url) => {
    fetched.push(url);
    return {
      ok: true,
      status: 200,
      async arrayBuffer() {
        return new Uint8Array([1, 2, 3]).buffer;
      },
    };
  };
  const gateway = fakeGateway();
  const running = run({
    tilo: workspace.tilo,
    makeClient: () => gateway,
    fetch,
    log: () => {},
  });
  await until(() => gateway.has("messageCreate"), "the gateway");
  gateway.emit("messageCreate", {
    id: "779",
    channelId: "456",
    guildId: "123",
    content: "look",
    createdTimestamp: 1000,
    webhookId: null,
    author: { id: "3001", username: "ada", avatar: "abc", bot: false },
    reference: null,
    attachments: [
      { url: "https://cdn.discordapp.com/attachments/456/1/shot.png", name: "shot.png", contentType: "image/png", size: 3 },
    ],
  });
  await until(
    () => workspace.done.some((call) => call.address === "so.tilo.chat#relay" && call.args.attachments?.length),
    "the relay with its file",
  );
  const relay = workspace.done.find(
    (call) => call.address === "so.tilo.chat#relay" && call.args.attachments?.length,
  );
  expect(fetched).toEqual(["https://cdn.discordapp.com/attachments/456/1/shot.png"]);
  expect(relay.args.attachments).toEqual([
    { file: { name: "shot.png", media: "image/png", size: 3, content: "tilo.content:abc123" } },
  ]);
  workspace.finish();
  await running;
});

test("a thread started here is started on Discord under the message it answers", async () => {
  const workspace = fakeWorkspace({
    accounts: [
      account({
        map: [
          { kind: "channel", tilo: CONVERSATION, discord: "456" },
          { kind: "webhook", tilo: CONVERSATION, discord: "555" },
          { kind: "message", tilo: `${CONVERSATION}/messages/m1`, discord: "9001", said: "hello" },
        ],
      }),
    ],
    messages: {
      [`chat/${RECORD}/messages/m1`]: messageFields({
        body: "hello",
        thread: { id: `${RECORD}/threads/t1` },
      }),
      [`chat/${RECORD}/threads/t1`]: { name: "topic" },
    },
    webAnswer: () => ({ status: 200, body: JSON.stringify({ id: "7001" }) }),
  });
  const running = run({
    tilo: workspace.tilo,
    makeClient: () => fakeGateway(),
    log: () => {},
  });
  workspace.say(change({ item: "messages/m1" }));
  await until(() => workspace.requests.length > 0, "the thread");
  expect(
    workspace.done.some(
      (call) => call.address === "so.tilo.discord.preview#start_thread_there" &&
        call.args.op.message === "9001" &&
        call.args.op.name === "topic",
    ),
  ).toBe(true);
  expect(workspace.requests[0].url).toBe(
    "https://discord.com/api/v10/channels/456/messages/9001/threads",
  );
  expect(JSON.parse(workspace.requests[0].body).name).toBe("topic");
  await until(
    () => workspace.done.some(
      (call) => call.address === "so.tilo.discord.preview#map" &&
        call.args.kind === "thread" && call.args.discord === "7001",
    ),
    "the thread's mapping",
  );
  const mapped = workspace.done.find(
    (call) => call.address === "so.tilo.discord.preview#map" && call.args.kind === "thread",
  );
  expect(mapped.args.tilo).toBe(`${CONVERSATION}/threads/t1`);
  expect(mapped.args.root).toBe(CONVERSATION);
  workspace.finish();
  await running;
});

test("a test's fake Discord is reached from the account's own record, both ways", async () => {
  const logins = [];
  const workspace = fakeWorkspace({
    accounts: [
      account({
        api_origin: "http://discord.test/api/v10",
        gateway_url: "ws://discord.test/gateway",
      }),
    ],
    messages: { [`chat/${RECORD}/messages/m1`]: messageFields({ body: "hello" }) },
  });
  const gateway = fakeGateway();
  gateway.login = async (token, url) => {
    logins.push(url);
    return gateway;
  };
  const running = run({
    tilo: workspace.tilo,
    makeClient: () => gateway,
    log: () => {},
  });
  workspace.say(change({ item: "messages/m1" }));
  await until(() => logins.length > 0 && workspace.requests.length > 0, "the fake Discord");
  expect(logins).toEqual(["ws://discord.test/gateway"]);
  expect(workspace.requests[0].url.startsWith("http://discord.test/api/v10/webhooks/555")).toBe(true);
  workspace.finish();
  await running;
});

test("a server connected after the program started opens the gateway then", async () => {
  const gateways = [];
  let connected = false;
  const workspace = fakeWorkspace({
    accounts: () => [connected ? account() : { guild_id: "" }],
  });
  const running = run({
    tilo: workspace.tilo,
    makeClient: () => {
      const gateway = fakeGateway();
      gateways.push(gateway);
      return gateway;
    },
    log: () => {},
  });

  // The daemon starts the program as the app is installed: no server is connected yet, so no
  // gateway is opened and nothing says so as a failure.
  await until(() => true, "the program started");
  expect(gateways).toHaveLength(0);

  // The person connects the server: the record changes, and the gateway opens then.
  connected = true;
  workspace.said("so.tilo.discord.preview/accounts", { kind: "change" });
  await until(() => gateways.length === 1, "the gateway after connecting");

  workspace.finish();
  await running;
});

test("a conversation paired after the gateway opened is carried at once", async () => {
  // The record as the workspace holds it: a server connected, and at first nothing paired.
  let pairs = [];
  let reads = 0;
  const workspace = fakeWorkspace({
    accounts: () => {
      reads += 1;
      return [account({ map: pairs })];
    },
  });
  const gateway = fakeGateway();
  const running = run({
    tilo: workspace.tilo,
    makeClient: () => gateway,
    log: () => {},
  });
  await until(() => gateway.has("messageCreate"), "the gateway");
  const opened = reads;

  // The person pairs the conversation while the gateway holds. The record is read again, so the
  // next change of that conversation reaches Discord without a restart.
  pairs = [
    { kind: "channel", tilo: CONVERSATION, discord: "456" },
    { kind: "webhook", tilo: CONVERSATION, discord: "555" },
  ];
  workspace.said("so.tilo.discord.preview/accounts", { kind: "change" });
  await until(() => reads > opened, "the record read again");

  workspace.message(`chat/${RECORD}/messages/m1`, messageFields({ body: "hello" }));
  workspace.say(change({ item: "messages/m1" }));
  await until(() => workspace.requests.length >= 1, "the send at once");
  expect(JSON.parse(workspace.requests[0].body).content).toBe("hello");

  workspace.finish();
  await running;
});

test("a bridged channel the bot has no webhook in gets one, joined in the record", async () => {
  const workspace = fakeWorkspace({
    accounts: [account({ map: [{ kind: "channel", tilo: CONVERSATION, discord: "456" }] })],
    messages: { [`chat/${RECORD}/messages/m1`]: messageFields({ body: "hello" }) },
    webhooks: [{ id: "77", name: "Someone's", token: null }],
  });
  const running = run({ tilo: workspace.tilo, makeClient: () => fakeGateway(), log: () => {} });
  workspace.say(change({ item: "messages/m1" }));
  await until(() => workspace.requests.length >= 1, "the send");
  // Discord's own list first, then the bridge's webhook made, asked of the app's own action.
  expect(workspace.hooks.map((request) => [request.method, request.url])).toEqual([
    ["GET", "https://discord.com/api/v10/channels/456/webhooks"],
    ["POST", "https://discord.com/api/v10/channels/456/webhooks"],
  ]);
  expect(JSON.parse(workspace.hooks[1].body)).toEqual({ name: "Tilo" });
  expect(
    workspace.done.some(
      (call) => call.address === "so.tilo.discord.preview#create_webhook" &&
        call.args.op.channel === "456",
    ),
  ).toBe(true);
  // The record joins the conversation to the webhook's id; its token is kept nowhere.
  const mapped = workspace.done.find(
    (call) => call.address === "so.tilo.discord.preview#map" && call.args.kind === "webhook",
  );
  expect(mapped.args).toEqual({ kind: "webhook", tilo: CONVERSATION, discord: "556", origin: "tilo" });
  expect(workspace.requests[0].url).toBe("https://discord.com/api/v10/webhooks/556/made?wait=true");

  // The next message goes through the same webhook, with no list or making again.
  workspace.message(`chat/${RECORD}/messages/m2`, messageFields({ body: "again" }));
  workspace.say(change({ item: "messages/m2" }));
  await until(() => workspace.requests.length >= 2, "the second send");
  expect(workspace.hooks).toHaveLength(2);
  workspace.finish();
  await running;
});

test("a server's category is made before the channels in it, which are made inside it", async () => {
  const done = [];
  let record = account({ ownership: "importing", map: [] });
  const calls = {
    async state(path) {
      if (path === "so.tilo.chat/conversations") return { channels: [] };
      return { accounts: [record] };
    },
    async do(call) {
      done.push([call.action, call.args]);
      const made = { id: `made${done.length}` };
      if (call.map) {
        record = {
          ...record,
          map: [...record.map, { kind: call.map.kind, tilo: `tl:so.tilo.chat:${made.id}`, discord: call.map.discord }],
        };
      }
      return made;
    },
  };
  await adoptServer({
    calls,
    account: record,
    layout: {
      channels: [
        { id: "10", kind: "category", name: "Projects", position: 0 },
        { id: "456", kind: "text", name: "design", parent: "10", position: 0 },
      ],
    },
  });
  expect(done).toEqual([
    ["new_category", { name: "Projects" }],
    ["new_channel", { name: "design", category: "tl:so.tilo.chat:made1" }],
    // The next pass reads the record those calls wrote: everything is joined, so it finishes.
    ["finish_import", { tilo_only: [] }],
  ]);
});

test("what a bridged channel said before, or while the bridge was away, is relayed once, oldest first", async () => {
  const done = [];
  const asked = [];
  const calls = {
    async webCall(request) {
      asked.push(request.url);
      // Discord answers the newest first; one of them is already joined to a message here.
      return {
        status: 200,
        body: JSON.stringify([
          { id: "803", channel_id: "456", content: "third", timestamp: "2026-11-24T09:03:00Z", author: { id: "3001", username: "ada" } },
          { id: "802", channel_id: "456", content: "second", timestamp: "2026-11-24T09:02:00Z", author: { id: "3002", username: "bob" } },
          { id: "801", channel_id: "456", content: "first", timestamp: "2026-11-24T09:01:00Z", author: { id: "3002", username: "bob" } },
        ]),
      };
    },
    async do(call) {
      done.push(call);
      return { id: `m${done.length}` };
    },
    async content() {
      return null;
    },
  };
  const held = account({
    map: [
      { kind: "channel", tilo: CONVERSATION, discord: "456" },
      { kind: "message", tilo: "tl:so.tilo.chat:m0", discord: "803" },
    ],
  });
  await readBack({ calls, account: held });
  expect(asked).toEqual(["https://discord.com/api/v10/channels/456/messages?limit=50"]);
  const relayed = done.filter((call) => call.action === "relay");
  expect(relayed.map((call) => [call.args.body, call.args.by, call.map.discord])).toEqual([
    ["first", "bob", "801"],
    ["second", "bob", "802"],
  ]);
  expect(relayed[0].args.written).toBe(Date.parse("2026-11-24T09:01:00Z"));
});

test("a person the workspace bans, lets back or removes is banned, unbanned or kicked in the server", async () => {
  const workspace = fakeWorkspace();
  const running = run({ tilo: workspace.tilo, makeClient: () => fakeGateway(), log: () => {} });
  await until(() => workspace.released.length > 0 || workspace.done.length > 0, "the bridge");

  const people = (verb, person, sentence = "") => ({
    kind: "change",
    path: "workspace/members",
    app: "tilo.workspace",
    verb,
    what: verb === "unban" ? "edited" : "removed",
    sentence,
    time_ms: 1000,
    actor: { by: { member: { person: "mod" } } },
    resources: [{ path: "workspace/members", app: "tilo.workspace", item: `workspace/members/${person}` }],
  });
  workspace.say(people("ban", "ada", "spam"));
  workspace.say(people("unban", "ada"));
  workspace.say(people("remove-member", "ada"));
  // Someone with no Discord account linked: nothing reaches Discord for them.
  workspace.say(people("ban", "grace"));
  await until(() => workspace.requests.length >= 3, "three member calls");

  expect(workspace.requests.map((request) => [request.method, request.url.replace(/^.*\/guilds/, "")])).toEqual([
    ["PUT", "/123/bans/3001"],
    ["DELETE", "/123/bans/3001"],
    ["DELETE", "/123/members/3001"],
  ]);
  expect(workspace.requests[0].headers["x-audit-log-reason"]).toBe("spam");
  // Each passed the bridge's own action first, whose guards say who may do it there.
  expect(
    workspace.done
      .map((call) => call.address)
      .filter((address) => /#(kick|ban|unban)$/.test(address)),
  ).toEqual([
    "so.tilo.discord.preview#ban",
    "so.tilo.discord.preview#unban",
    "so.tilo.discord.preview#kick",
  ]);

  workspace.finish();
  await running;
});

test("a bridge of one channel leaves the server's members alone", async () => {
  const workspace = fakeWorkspace({ accounts: [account({ only_channel: "456" })] });
  const running = run({ tilo: workspace.tilo, makeClient: () => fakeGateway(), log: () => {} });
  await until(() => workspace.released.length > 0 || workspace.done.length > 0, "the bridge");
  workspace.say({
    kind: "change",
    verb: "ban",
    resources: [{ path: "workspace/members", app: "tilo.workspace", item: "workspace/members/ada" }],
  });
  // A message after it is carried, so the ban was read and passed over.
  workspace.message(`chat/${RECORD}/messages/m1`, messageFields({ body: "after" }));
  workspace.say(change({ item: "messages/m1" }));
  await until(() => workspace.requests.length >= 1, "the send");
  expect(workspace.requests.map((request) => request.url).filter((url) => url.includes("/guilds/"))).toEqual([]);
  workspace.finish();
  await running;
});

test("a Discord account a moderator blocks loses what was mirrored of it here", async () => {
  const relayed = { kind: "message", tilo: "tl:so.tilo.chat:m9", discord: "779", origin: "discord", persona: "3006" };
  let held = account({ map: [...account().map, relayed] });
  const workspace = fakeWorkspace({ accounts: () => [held] });
  const running = run({ tilo: workspace.tilo, makeClient: () => fakeGateway(), log: () => {} });
  await until(() => workspace.released.length > 0 || workspace.done.length > 0, "the bridge");

  // The moderator's block is the record's newest change: the bridge reads the record again.
  held = { ...held, blocked_personas: ["3006"] };
  workspace.say({
    kind: "change",
    verb: "block_persona",
    actor: { by: { member: { person: "mod" } } },
    resources: [{ path: "so.tilo.discord.preview", app: "so.tilo.discord.preview", record: "accounts", item: "x" }],
  });
  await until(() => workspace.done.some((call) => call.address === "so.tilo.chat#delete"), "the deletion");
  expect(workspace.done.filter((call) => /#(delete|unmap)$/.test(call.address))).toEqual([
    { address: "so.tilo.chat#delete", args: { m: "tl:so.tilo.chat:m9", to: null } },
    { address: "so.tilo.discord.preview#unmap", args: { kind: "message", tilo: "tl:so.tilo.chat:m9" } },
  ]);
  workspace.finish();
  await running;
});
