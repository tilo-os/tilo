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

/// The bridge's record as the workspace holds it: one server, its one channel and webhook, and
/// the account ada linked.
function account() {
  return {
    guild_id: "123",
    server: "Server",
    bot_user: "900",
    token_item: "vault.item:discord-token",
    channels: [{ id: "456", kind: "text", name: "design" }],
    map: [
      { kind: "channel", tilo: "tl:so.tilo.chat:design", discord: "456" },
      { kind: "webhook", tilo: "tl:so.tilo.chat:design", discord: "555" },
    ],
    links: [
      {
        discord_user: "3001",
        person: "ada",
        user_name: "ada",
        display_name: "Ada",
        avatar: "abc",
      },
    ],
  };
}

/// What the bridge did: the calls it invoked and the web requests it made.
function fakeWorkspace({ events }) {
  const done = [];
  const requests = [];
  const released = [];
  const secret = "MTIz.GxYz.abcdefghij0123456789AB";
  let finish;
  const finished = new Promise((resolve) => {
    finish = resolve;
  });
  const ws = {
    async read() {
      return { accounts: [account()] };
    },
    async do(address, args) {
      done.push({ address, args });
      return { id: "made1" };
    },
    async *watch() {
      for (const event of events) yield event;
      await finished;
    },
    vault: {
      async release(item) {
        released.push(item);
        return secret;
      },
      async webCall(request) {
        requests.push(request);
        return { status: 200, body: JSON.stringify({ id: "9001", channel_id: "456" }) };
      },
    },
  };
  return {
    tilo: { workspace: () => ws },
    done,
    requests,
    released,
    finish,
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
  const workspace = fakeWorkspace({
    events: [
      {
        kind: "created",
        conversation: "tl:so.tilo.chat:design",
        message: { id: "m1", text: "hello", author: "ada", attachments: [] },
      },
      {
        kind: "updated",
        conversation: "tl:so.tilo.chat:design",
        message: { id: "m1", text: "hello again", author: "ada", attachments: [] },
      },
      {
        kind: "reaction",
        conversation: "tl:so.tilo.chat:design",
        emoji: "🎉",
        message: { id: "m1", text: "hello again", author: "ada", attachments: [] },
      },
      {
        kind: "deleted",
        conversation: "tl:so.tilo.chat:design",
        message: { id: "m1", text: "", author: "ada", attachments: [] },
      },
    ],
  });
  const running = run({
    tilo: workspace.tilo,
    makeClient: () => gateway,
    log: () => {},
  });

  // A message written on Discord reaches the linked conversation as its author.
  await until(() => workspace.done.length > 0 || workspace.released.length > 0, "the bridge");
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
  expect(relay.args.to).toBe("tl:so.tilo.chat:design");
  expect(relay.args.by).toBe("ada");
  expect(relay.args.service).toBe("Discord");
  expect(relay.args.avatar).toBe("https://cdn.discordapp.com/avatars/3001/abc.png?size=128");
  expect(
    workspace.done.some(
      (call) => call.address === "so.tilo.discord.preview#map" && call.args.discord === "777",
    ),
  ).toBe(true);

  // The workspace's message reaches the webhook, and what it made is joined to the message. The
  // bridge's own action takes it first, so Discord hears only what its guards allow.
  await until(() => workspace.requests.length >= 1, "the send");
  let carried = workspace.done.find((call) => call.address === "so.tilo.discord.preview#send_there");
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
  expect(mapped.args.tilo).toBe("tl:so.tilo.chat:m1");
  expect(mapped.args.kind).toBe("message");

  // An edit follows the message it is about.
  await until(() => workspace.requests.length >= 2, "the edit");
  expect(
    workspace.done.some((call) => call.address === "so.tilo.discord.preview#edit_there"),
  ).toBe(true);
  const edit = workspace.requests[1];
  expect(edit.url).toBe("https://discord.com/api/v10/webhooks/555/messages/9001");
  expect(edit.method).toBe("PATCH");
  expect(JSON.parse(edit.body).content).toBe("hello again");

  // A reaction follows it too.
  await until(() => workspace.requests.length >= 3, "the reaction");
  expect(
    workspace.done.some((call) => call.address === "so.tilo.discord.preview#react_there"),
  ).toBe(true);
  const reacted = workspace.requests[2];
  expect(reacted.method).toBe("PUT");
  expect(reacted.url).toBe(
    `https://discord.com/api/v10/channels/456/messages/9001/reactions/${encodeURIComponent("🎉")}`,
  );

  // And a deletion.
  await until(() => workspace.requests.length >= 4, "the deletion");
  const deleted = workspace.requests[3];
  expect(deleted.method).toBe("DELETE");
  expect(deleted.url).toBe("https://discord.com/api/v10/webhooks/555/messages/9001");

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

test("a file posted on Discord is kept here and the message carries it", async () => {
  const done = [];
  const kept = [];
  const fetched = [];
  const released = [];
  let finish;
  const finished = new Promise((resolve) => (finish = resolve));
  const ws = {
    async read() {
      return { accounts: [account()] };
    },
    async do(address, args) {
      done.push({ address, args });
      return { id: "made1" };
    },
    async *watch() {
      await finished;
    },
    files: {
      async upload(bytes) {
        kept.push(bytes);
        return "tilo.content:abc123";
      },
    },
    vault: {
      async release(item) {
        released.push(item);
        return "MTIz.GxYz.abcdefghij0123456789AB";
      },
      async webCall() {
        return { status: 200, body: "{}" };
      },
    },
  };
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
    tilo: { workspace: () => ws },
    makeClient: () => gateway,
    fetch,
    log: () => {},
  });
  await until(() => released.length > 0 || done.length > 0, "the bridge");
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
    () => done.some((call) => call.address === "so.tilo.chat#relay" && call.args.attachments?.length),
    "the relay with its file",
  );
  const relay = done.find(
    (call) => call.address === "so.tilo.chat#relay" && call.args.attachments?.length,
  );
  expect(fetched).toEqual(["https://cdn.discordapp.com/attachments/456/1/shot.png"]);
  expect(kept).toHaveLength(1);
  expect(relay.args.attachments).toEqual([
    { file: { name: "shot.png", media: "image/png", size: 3, content: "tilo.content:abc123" } },
  ]);
  finish();
  await running;
});

test("a thread started here is started on Discord under the message it answers", async () => {
  const done = [];
  const requests = [];
  let finish;
  const finished = new Promise((resolve) => (finish = resolve));
  const ws = {
    async read() {
      return {
        accounts: [
          {
            ...account(),
            map: [
              ...account().map,
              { kind: "message", tilo: "tl:so.tilo.chat:m1", discord: "9001" },
            ],
          },
        ],
      };
    },
    async do(address, args) {
      done.push({ address, args });
      return { id: "made1" };
    },
    async *watch() {
      yield {
        kind: "thread",
        conversation: "tl:so.tilo.chat:design",
        name: "topic",
        thread: "tl:so.tilo.chat:topic",
        message: { id: "m1", text: "hello", author: "ada" },
      };
      await finished;
    },
    vault: {
      async release() {
        return "MTIz.GxYz.abcdefghij0123456789AB";
      },
      async webCall(request) {
        requests.push(request);
        return { status: 200, body: JSON.stringify({ id: "7001" }) };
      },
    },
  };
  const running = run({
    tilo: { workspace: () => ws },
    makeClient: () => fakeGateway(),
    log: () => {},
  });
  await until(() => requests.length > 0, "the thread");
  expect(
    done.some(
      (call) => call.address === "so.tilo.discord.preview#start_thread_there" &&
        call.args.op.message === "9001" &&
        call.args.op.name === "topic",
    ),
  ).toBe(true);
  expect(requests[0].url).toBe(
    "https://discord.com/api/v10/channels/456/messages/9001/threads",
  );
  expect(JSON.parse(requests[0].body).name).toBe("topic");
  await until(
    () => done.some(
      (call) => call.address === "so.tilo.discord.preview#map" &&
        call.args.kind === "thread" && call.args.discord === "7001",
    ),
    "the thread's mapping",
  );
  finish();
  await running;
});

test("a test's fake Discord is reached from the account's own record, both ways", async () => {
  let finish;
  const finished = new Promise((resolve) => (finish = resolve));
  const requests = [];
  const logins = [];
  const ws = {
    async read() {
      return {
        accounts: [
          {
            ...account(),
            api_origin: "http://discord.test/api/v10",
            gateway_url: "ws://discord.test/gateway",
          },
        ],
      };
    },
    async do() {
      return { id: "made1" };
    },
    async *watch() {
      yield {
        kind: "created",
        conversation: "tl:so.tilo.chat:design",
        message: { id: "m1", text: "hello", author: "ada", attachments: [] },
      };
      await finished;
    },
    vault: {
      async release() {
        return "MTIz.GxYz.abcdefghij0123456789AB";
      },
      async webCall(request) {
        requests.push(request);
        return { status: 200, body: JSON.stringify({ id: "9001" }) };
      },
    },
  };
  const gateway = fakeGateway();
  gateway.login = async (token, url) => {
    logins.push(url);
    return gateway;
  };
  const running = run({
    tilo: { workspace: () => ws },
    makeClient: () => gateway,
    log: () => {},
  });
  await until(() => logins.length > 0 && requests.length > 0, "the fake Discord");
  expect(logins).toEqual(["ws://discord.test/gateway"]);
  expect(requests[0].url.startsWith("http://discord.test/api/v10/webhooks/555")).toBe(true);
  finish();
  await running;
});

test("a server connected after the program started opens the gateway then", async () => {
  const gateways = [];
  let connected = false;
  let second;
  const next = new Promise((resolve) => (second = resolve));
  let finish;
  const finished = new Promise((resolve) => (finish = resolve));
  const ws = {
    async read() {
      return { accounts: [connected ? account() : { guild_id: "" }] };
    },
    async do() {
      return { id: "made1" };
    },
    async *watch() {
      yield { kind: "change" };
      await next;
      yield { kind: "change" };
      await finished;
    },
    vault: {
      async release() {
        return "MTIz.GxYz.abcdefghij0123456789AB";
      },
      async webCall() {
        return { status: 200, body: "{}" };
      },
    },
  };
  const running = run({
    tilo: { workspace: () => ws },
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
  second();
  await until(() => gateways.length === 1, "the gateway after connecting");

  finish();
  await running;
});
