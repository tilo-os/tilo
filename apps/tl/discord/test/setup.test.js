// Setting a server up from the app: the bot's token goes into the person's vault and only its
// address into the record, the bot's servers come from Discord's own answer, and a server's
// channels are read to choose the ones brought in.

import { test, expect } from "bun:test";
import { callsOf } from "../main.js";
import { answerSetup, released, sha256Hex, signInStart } from "../main.js";

const TOKEN = "MTIz.GxYz.abcdefghij0123456789AB";
const ITEM = `vault.item:${"a".repeat(32)}`;

/// What the bridge did: the calls it invoked and what it released from the vault.
function fakeWorkspace({ account, ask = ITEM, release = () => TOKEN, answer } = {}) {
  const done = [];
  const released = [];
  const asked = [];
  const ws = {
    async read() {
      return { accounts: [account] };
    },
    async do(address, args) {
      done.push({ address, args });
      return { id: "made1" };
    },
    async *watch() {},
    vault: {
      async release(item) {
        released.push(item);
        return release(item, released.length);
      },
      async ask(question) {
        asked.push(question);
        return ask;
      },
    },
  };
  const fetch = async (url, init) => {
    const answer_ = answer?.(url, init) ?? { status: 404, body: "{}" };
    return {
      status: answer_.status,
      async text() {
        return typeof answer_.body === "string"
          ? answer_.body
          : JSON.stringify(answer_.body);
      },
    };
  };
  return { calls: callsOf(ws), done, released, asked, fetch };
}

/// The account the setup writes into.
function account(wants, more = {}) {
  return {
    token_item: ITEM,
    setup_wants: wants,
    setup_ask: "press1",
    setup_person: "ada",
    sign_in_page: "https://discord.com/oauth2/authorize?client_id=1",
    ...more,
  };
}

test("the token's item goes into the record, and the token never leaves the vault", async () => {
  const workspace = fakeWorkspace({ account: account("token") });
  await answerSetup({ calls: workspace.calls, account: account("token"), fetch: workspace.fetch });

  // The person typed the token into the vault's own field: the program asked for it, and kept
  // only where the vault put it.
  expect(workspace.asked).toHaveLength(1);
  expect(workspace.asked[0].kind).toBe("key");
  expect(workspace.asked[0].belongsTo).toEqual(["https://discord.com"]);
  expect(workspace.asked[0].sentIn).toBe("header:Authorization:Bot");
  const kept = workspace.done.find((call) => call.address.endsWith("#keep_token"));
  expect(kept.args).toEqual({ ask: "press1", item: ITEM });
  for (const call of workspace.done) {
    expect(JSON.stringify(call)).not.toContain(TOKEN);
  }
  // Nothing reached Discord: the token was never in this process at all.
  expect(workspace.released).toHaveLength(0);
});

test("finding the bot's servers lists what Discord answered, as the press it answers", async () => {
  const workspace = fakeWorkspace({
    account: account("find"),
    answer: (url, init) => {
      expect(url).toBe("https://discord.com/api/v10/users/@me/guilds");
      expect(init.headers.authorization).toBe(`Bot ${TOKEN}`);
      return {
        status: 200,
        body: [
          { id: "123", name: "The Lounge" },
          { id: "456", name: "Design" },
          { name: "no id" },
        ],
      };
    },
  });
  await answerSetup({ calls: workspace.calls, account: account("find"), fetch: workspace.fetch });

  expect(workspace.released).toEqual([ITEM]);
  const found = workspace.done.find((call) => call.address.endsWith("#found_servers"));
  expect(found.args).toEqual({
    ask: "press1",
    servers: [
      { id: "456", name: "Design" },
      { id: "123", name: "The Lounge" },
    ],
  });
});

test("a server's channels come from Discord, as the app's record keeps them", async () => {
  const chosen = account("channels", { setup_server: "123" });
  const workspace = fakeWorkspace({
    account: chosen,
    answer: (url) => {
      expect(url).toBe("https://discord.com/api/v10/guilds/123/channels");
      return {
        status: 200,
        body: [
          { id: "1", name: "design", type: 0, parentId: "9", position: 2, topic: "looks" },
          { id: "2", name: "voice", type: 2, position: 3 },
          { id: "9", name: "Rooms", type: 4, position: 1 },
        ],
      };
    },
  });
  await answerSetup({ calls: workspace.calls, account: chosen, fetch: workspace.fetch });

  const found = workspace.done.find((call) => call.address.endsWith("#found_channels"));
  expect(found.args.ask).toBe("press1");
  expect(found.args.channels).toEqual([
    { id: "1", kind: "text", name: "design", parent: "9", position: 2, topic: "looks", overwrites: [] },
    { id: "2", kind: "voice", name: "voice", parent: null, position: 3, topic: "", overwrites: [] },
    { id: "9", kind: "category", name: "Rooms", parent: null, position: 1, topic: "", overwrites: [] },
  ]);
});

test("a token Discord refuses is said in words the person can act on", async () => {
  const workspace = fakeWorkspace({
    account: account("find"),
    answer: () => ({ status: 401, body: {} }),
  });
  await answerSetup({ calls: workspace.calls, account: account("find"), fetch: workspace.fetch });

  const failed = workspace.done.find((call) => call.address.endsWith("#setup_failed"));
  expect(failed.args.ask).toBe("press1");
  expect(failed.args.problem).toContain("did not accept the bot's token");
});

test("the release is asked again once the person allowed the app to read the item", async () => {
  const waits = [];
  const calls = {
    async release(item) {
      if (waits.length === 0) {
        throw new Error(
          "so.tilo.discord.preview may not read this vault item yet: your person is asked " +
            "whether it may, once. Call again once they allowed it.",
        );
      }
      expect(item).toBe(ITEM);
      return TOKEN;
    },
  };
  const token = await released(calls, ITEM, { wait: async (ms) => waits.push(ms) });
  expect(token).toBe(TOKEN);
  expect(waits).toEqual([2_000]);
});

test("signing in goes to the page's own state, hashed, and a fresh one otherwise", async () => {
  const named = await signInStart("https://discord.com/oauth2/authorize?client_id=1&state=abc");
  expect(named.url).toBe("https://discord.com/oauth2/authorize?client_id=1&state=abc");
  expect(named.sha256).toBe(await sha256Hex("abc"));
  expect(named.sha256).toHaveLength(64);

  const fresh = await signInStart("https://discord.com/oauth2/authorize?client_id=1");
  const state = new URL(fresh.url).searchParams.get("state");
  expect(state).toMatch(/^[0-9a-f]{32}$/);
  expect(fresh.sha256).toBe(await sha256Hex(state));
});
