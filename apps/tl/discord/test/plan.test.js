import { test, expect } from "bun:test";
import {
  fromDiscord,
  toDiscord,
  carried,
  fit,
  bareBlob,
  avatarUrl,
  mapping,
} from "../src/plan.js";
import { Calls } from "../src/tilo.js";
import { looksLikeToken } from "../src/rest.js";

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
        person: "ada",
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
  expect(call.args.avatar).toBe("https://cdn.discordapp.com/avatars/3001/abc.png?size=128");
  expect(call.args.outside_emoji).toEqual([
    { name: "party", picture: "https://cdn.discordapp.com/emojis/22.png" },
  ]);
  expect(call.map).toEqual({ kind: "message", discord: "777", origin: "discord", persona: "3001" });
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
  expect(calls[0].action).toBe("observe_layout");
  const made = calls.find((call) => call.action === "new_channel");
  expect(made.args.name).toBe("notes");
  expect(made.map).toEqual({ kind: "channel", discord: "458", origin: "discord" });
});

test("a workspace message goes through the webhook, cut where Discord takes it", () => {
  const ops = toDiscord(state(), "tl:so.tilo.chat:design", {
    id: "m9",
    text: "word ".repeat(600),
    author: "ada",
    attachments: [{ blob: "tilo-blob://abc123", name: "a.png", size: 10 }],
  });
  expect(ops).toHaveLength(1);
  const [op] = ops;
  expect(op.channel).toBe("456");
  expect(op.webhook).toBe("555");
  expect(op.username).toBe("Ada");
  expect(op.attachments).toEqual([{ blob: "abc123", name: "a.png", size: 10 }]);
  expect([...op.content].length).toBe(2000);
  expect(op.content.endsWith("…")).toBe(true);
  expect(op.tilo).toBe("tl:so.tilo.chat:m9");
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
  const calls = new Calls(ws);
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
  const calls = new Calls(ws);
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
    author: "ada",
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
    avatarUrl({ discord_user: "1", avatar: "h" }),
  ).toBe("https://cdn.discordapp.com/avatars/1/h.png?size=128");
  expect(avatarUrl(null)).toBe("");
  expect(mapping(state(), "channel", "tl:so.tilo.chat:design").discord).toBe("456");
});
