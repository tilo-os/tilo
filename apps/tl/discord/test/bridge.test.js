// A workspace bridged to a fake Discord: a message written on Discord reaches the linked
// conversation as its author, a message written here reaches the webhook under the linked
// account's name, and an edit, a deletion and a reaction follow it there.

import { test, expect } from "bun:test";
import { run } from "../main.js";

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
function change({ record = RECORD, item, actor = { by: JSON.parse(ADA) }, time = 1000 } = {}) {
  return {
    kind: "change",
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
function fakeWorkspace({ accounts = [account()], messages = {}, webAnswer = null } = {}) {
  const done = [];
  const requests = [];
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
  expect(send.url).toBe("https://discord.com/api/v10/webhooks/555?wait=true");
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
  expect(edit.url).toBe("https://discord.com/api/v10/webhooks/555/messages/9001");
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

  // And a deletion, once the message is gone from the workspace.
  workspace.reads.delete(message);
  workspace.say(change({ item: "messages/m1", time: 4000 }));
  await until(() => workspace.requests.length >= 4, "the deletion");
  const deleted = workspace.requests[3];
  expect(deleted.method).toBe("DELETE");
  expect(deleted.url).toBe("https://discord.com/api/v10/webhooks/555/messages/9001");
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
  expect(found).toEqual([{ app: "so.tilo.discord.preview", purpose: "bot-token" }]);
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
