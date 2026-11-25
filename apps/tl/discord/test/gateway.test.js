import { test, expect } from "bun:test";
import { connect, INTENTS } from "../src/gateway.js";
import { missingToSpeak } from "../src/permissions.js";

/// Wait until `check()` is true, or fail the test after `timeout`.
async function until(check, what, timeout = 3_000) {
  const end = Date.now() + timeout;
  while (!check()) {
    if (Date.now() > end) throw new Error(`waited for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

/// A fake Discord gateway: Hello, then whatever the test sends to the identified client.
function fakeGateway() {
  const received = [];
  let socket = null;
  const server = Bun.serve({
    port: 0,
    fetch(request, server) {
      if (server.upgrade(request)) return;
      return new Response("a gateway speaks WebSocket alone", { status: 400 });
    },
    websocket: {
      open(ws) {
        ws.send(JSON.stringify({ op: 10, d: { heartbeat_interval: 60 } }));
      },
      message(ws, raw) {
        const frame = JSON.parse(String(raw));
        received.push(frame);
        if (frame.op === 1) ws.send(JSON.stringify({ op: 11 }));
        if (frame.op === 2) {
          socket = ws;
          ws.send(
            JSON.stringify({
              op: 0,
              s: 1,
              t: "READY",
              d: {
                session_id: "s1",
                resume_gateway_url: `ws://127.0.0.1:${server.port}`,
                user: { id: "900", username: "bridge" },
              },
            }),
          );
          ws.send(
            JSON.stringify({
              op: 0,
              s: 2,
              t: "GUILD_CREATE",
              d: {
                id: "123",
                owner_id: "1",
                channels: [
                  { id: "456", type: 0, name: "design", position: 0 },
                  { id: "457", type: 4, name: "Text", position: 1 },
                  { id: "458", type: 2, name: "Call", position: 2, permission_overwrites: [] },
                ],
                // `@everyone` may see and connect; the bot's role "7" may speak.
                roles: [
                  { id: "123", permissions: String((1 << 10) | (1 << 20)) },
                  { id: "7", permissions: String(1 << 21) },
                ],
                members: [{ user: { id: "900" }, roles: ["7"] }],
                emojis: [{ id: "22", name: "party" }],
              },
            }),
          );
        }
      },
    },
  });
  return {
    url: `ws://127.0.0.1:${server.port}/`,
    received,
    send(frame) {
      socket.send(JSON.stringify(frame));
    },
    stop() {
      server.stop(true);
    },
  };
}

test("the gateway identifies with the bridge's intents and carries Discord's events", async () => {
  const discord = fakeGateway();
  const events = [];
  const client = await connect({
    token: "MTIz.GxYz.abcdefghij0123456789AB",
    url: discord.url,
    onEvent: (event) => events.push(event),
  });
  expect(client.user.id).toBe("900");

  const identify = discord.received.find((frame) => frame.op === 2);
  expect(identify.d.token).toBe("MTIz.GxYz.abcdefghij0123456789AB");
  expect(identify.d.intents).toBe(INTENTS);
  expect(INTENTS).toBe(34_447);

  await until(() => events.some((event) => event.source === "LAYOUT_CHANGED"), "the layout");
  const layout = events.find((event) => event.source === "LAYOUT_CHANGED").payload;
  expect(layout.channels.map((channel) => [channel.id, channel.kind, channel.name])).toEqual([
    ["456", "text", "design"],
    ["457", "category", "Text"],
    ["458", "voice", "Call"],
  ]);
  expect(layout.emojis).toEqual([{ id: "22", name: "party", animated: false }]);

  discord.send({
    op: 0,
    s: 3,
    t: "MESSAGE_CREATE",
    d: {
      id: "777",
      channel_id: "456",
      guild_id: "123",
      content: "look at this",
      timestamp: "2026-11-24T09:00:00.000Z",
      author: { id: "3001", username: "ada", avatar: "abc" },
      attachments: [
        {
          url: "https://cdn.discordapp.com/attachments/456/1/shot.png",
          name: "shot.png",
          content_type: "image/png",
          size: 3,
        },
      ],
    },
  });
  discord.send({
    op: 0,
    s: 4,
    t: "MESSAGE_UPDATE",
    d: { id: "777", channel_id: "456", content: "look at this again" },
  });
  discord.send({ op: 0, s: 5, t: "MESSAGE_DELETE", d: { id: "777", channel_id: "456" } });
  discord.send({
    op: 0,
    s: 6,
    t: "MESSAGE_REACTION_ADD",
    d: {
      message_id: "777",
      channel_id: "456",
      user_id: "3001",
      emoji: { name: "🎉" },
      member: { user: { username: "ada" } },
    },
  });
  discord.send({
    op: 0,
    s: 7,
    t: "GUILD_MEMBER_UPDATE",
    d: { guild_id: "123", user: { id: "3001", username: "ada", avatar: "abc" }, roles: ["5"] },
  });

  await until(() => events.length >= 6, "the events");
  const by = (source) => events.find((event) => event.source === source).payload;
  const created = by("MESSAGE_CREATED");
  expect(created.id).toBe("777");
  expect(created.channel_id).toBe("456");
  expect(created.content).toBe("look at this");
  expect(created.author).toEqual({ id: "3001", name: "ada", avatar: "abc", bot: false });
  expect(created.attachments).toEqual([
    {
      url: "https://cdn.discordapp.com/attachments/456/1/shot.png",
      name: "shot.png",
      media: "image/png",
      size: 3,
    },
  ]);
  expect(by("MESSAGE_UPDATED").content).toBe("look at this again");
  expect(by("MESSAGE_DELETED").id).toBe("777");
  const reaction = by("REACTION_ADDED");
  expect(reaction.message_id).toBe("777");
  expect(reaction.user_name).toBe("ada");
  expect(by("MEMBER_CHANGED").roles).toEqual(["5"]);

  await until(() => discord.received.some((frame) => frame.op === 1), "a heartbeat");
  client.destroy();
  discord.stop();
});

test("the gateway keeps what decides the bot's permissions as Discord changes it", async () => {
  const discord = fakeGateway();
  const client = await connect({
    token: "MTIz.GxYz.abcdefghij0123456789AB",
    url: discord.url,
    onEvent: () => {},
  });
  await until(() => client.standing("123"), "the server described");
  const missing = () => missingToSpeak(client.standing("123"), "458");
  expect(missing()).toEqual([]);

  // The call's channel denies the bot Speak.
  discord.send({
    op: 0,
    s: 3,
    t: "CHANNEL_UPDATE",
    d: {
      id: "458",
      guild_id: "123",
      permission_overwrites: [{ id: "900", type: 1, allow: "0", deny: String(1 << 21) }],
    },
  });
  await until(() => missing().length > 0, "the overwrite kept");
  expect(missing()).toEqual(["Speak"]);

  // The bot's role becomes Administrator, which no overwrite takes away.
  discord.send({
    op: 0,
    s: 4,
    t: "GUILD_ROLE_UPDATE",
    d: { guild_id: "123", role: { id: "7", permissions: String(1 << 3) } },
  });
  await until(() => missing().length === 0, "the role kept");

  // The bot loses the role: back to what `@everyone` and the overwrite leave it.
  discord.send({
    op: 0,
    s: 5,
    t: "GUILD_MEMBER_UPDATE",
    d: { guild_id: "123", user: { id: "900" }, roles: [] },
  });
  await until(() => missing().length > 0, "the bot's roles kept");
  expect(missing()).toEqual(["Speak"]);
  expect(client.standing("999")).toBe(null);
  client.destroy();
  discord.stop();
});

test("a thread started and a bulk deletion reach the bridge as their events", async () => {
  const discord = fakeGateway();
  const events = [];
  const client = await connect({
    token: "MTIz.GxYz.abcdefghij0123456789AB",
    url: discord.url,
    onEvent: (event) => events.push(event),
  });
  discord.send({
    op: 0,
    s: 3,
    t: "THREAD_CREATE",
    d: { id: "777", guild_id: "123", parent_id: "456", name: "follow-up", owner_id: "3001", type: 11 },
  });
  discord.send({
    op: 0,
    s: 4,
    t: "MESSAGE_DELETE_BULK",
    d: { ids: ["801", "802"], channel_id: "456", guild_id: "123" },
  });
  await until(() => events.filter((event) => event.source === "MESSAGE_DELETED").length === 2, "the deletions");
  expect(events.find((event) => event.source === "THREAD_CREATED").payload).toEqual({
    id: "777",
    parent_id: "456",
    name: "follow-up",
    owner_id: "3001",
  });
  expect(
    events.filter((event) => event.source === "MESSAGE_DELETED").map((event) => event.payload.id),
  ).toEqual(["801", "802"]);
  client.destroy();
  discord.stop();
});

test("a call hands the voice session Discord named, never the gateway's", async () => {
  const discord = fakeGateway();
  const client = await connect({
    token: "MTIz.GxYz.abcdefghij0123456789AB",
    url: discord.url,
    onEvent: () => {},
  });
  const joining = client.joinVoice("123", "458");
  await until(
    () => discord.received.some((frame) => frame.op === 4),
    "the join",
  );
  const join = discord.received.find((frame) => frame.op === 4);
  expect(join.d).toMatchObject({ guild_id: "123", channel_id: "458", self_mute: false });

  // Discord moves the bot in, naming the voice session its connection identifies with.
  discord.send({
    op: 0,
    s: 3,
    t: "VOICE_STATE_UPDATE",
    d: { guild_id: "123", user_id: "900", channel_id: "458", session_id: "voice-session-1" },
  });
  discord.send({
    op: 0,
    s: 4,
    t: "VOICE_SERVER_UPDATE",
    d: {
      guild_id: "123",
      user_id: "900",
      endpoint: "c-sjc01.discord.media:2096",
      token: "voice-token",
    },
  });
  const server = await joining;
  expect(server.endpoint).toBe("c-sjc01.discord.media:2096");
  expect(server.token).toBe("voice-token");
  expect(server.channel).toBe("458");
  expect(server.session).toBe("voice-session-1");

  // The person ends the call in Discord: the bot's own state says it is in none.
  discord.send({
    op: 0,
    s: 5,
    t: "VOICE_STATE_UPDATE",
    d: { guild_id: "123", user_id: "900", channel_id: null, session_id: "voice-session-1" },
  });
  await until(() => !client.voice.has("123"), "the call ended");
  client.destroy();
  discord.stop();
});
