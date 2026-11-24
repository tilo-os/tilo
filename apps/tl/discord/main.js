// The Discord bridge: Discord's REST, gateway and voice on this side, the workspace's records,
// actions and calls on the other. It runs as the app's process, attested as this package, and it
// is the device's held connection that carries the gateway and the call lane a voice channel's
// call is bridged through.

import { Calls } from "./src/tilo.js";
import { startCalls } from "./src/call.js";
import { connect as connectVoice } from "./src/voice.js";
import {
  fromDiscord,
  toDiscord,
  editToDiscord,
  deleteToDiscord,
  reactToDiscord,
  mapping,
  published,
  SELF,
} from "./src/plan.js";
import {
  API,
  ORIGIN,
  SENT_IN,
  channelsOf,
  guildChannels,
  serversOf,
  webhookSend,
  webhookEdit,
  webhookDelete,
  react,
  whyRefused,
} from "./src/rest.js";

/// The bridge's record path: its own app's root, the accounts it bridges.
export const ACCOUNTS = `${SELF}/accounts`;

/// How long to wait before asking again for a vault item whose person is being asked whether the
/// app may read it, and how many times.
export const RELEASE_RETRY_MS = 2_000;
export const RELEASE_TRIES = 90;

/// A procedure the process runs once: the bridge starts, answers the setup its person asks for,
/// and holds the gateway of the server it bridges until the connection ends.
export async function run({
  tilo,
  makeClient,
  makeVoice,
  fetch = globalThis.fetch,
  wait,
  log = () => {},
}) {
  const calls = new Calls(tilo.workspace ? tilo.workspace() : tilo);
  // The setup is answered beside whatever the bridge carries: a press reaches this process
  // through the record, and nothing else does.
  const setup = serveSetup({ calls, fetch, wait, log }).catch((error) =>
    log(`the setup stopped: ${error?.message ?? error}`),
  );
  const accounts = await calls.state(ACCOUNTS);
  const account = accounts?.accounts?.[0] ?? accounts;
  if (!account?.guild_id) {
    log("no Discord server is connected");
    return setup;
  }
  const token = await released(calls, account.token_item, { wait });
  // The call lane this app's connection was granted: a client that carries none leaves the
  // bridge reading and writing, never in a call.
  const lane = calls.lane();
  let gateway = null;
  const carried = lane
    ? startCalls({
        calls,
        lane,
        makeVoice: makeVoice ?? ((options) => connectVoice(options)),
        log,
        gateway: {
          get user() {
            return gateway?.user ?? null;
          },
          joinVoice: (guild, channel) => gateway.joinVoice(guild, channel),
          leaveVoice: (guild) => gateway.leaveVoice(guild),
        },
      })
    : null;
  if (!carried) {
    log("this build carries no call lane: the bridge reads and writes but joins no call");
  }
  const client = await import("./src/gateway.js").then(({ connect }) =>
    connect({
      make: makeClient,
      token,
      onEvent: (event) => carryFromDiscord({ calls, account, event, log }),
      onVoice: (event) => carried?.hear(event),
      log,
    }),
  );
  gateway = client;
  await carried?.start();
  await carryFromWorkspace({ calls, account, log });
  return client;
}

/// The bot's token, released from the workspace's vault into this process: the one place a
/// secret is a value here, and it is the app's held connection the vault answers it to. The
/// person is asked once whether the app may read the item, so a refusal saying so is waited on
/// and the call is made again.
export async function released(
  calls,
  item,
  {
    wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    tries = RELEASE_TRIES,
    retryMs = RELEASE_RETRY_MS,
  } = {},
) {
  if (!item) {
    throw new Error("this bridge keeps no bot's token in the workspace's vault");
  }
  for (let left = tries; left > 0; left -= 1) {
    try {
      return await calls.release(item);
    } catch (error) {
      if (!askedFirst(error)) throw error;
      await wait(retryMs);
    }
  }
  throw new Error(
    "the person was asked whether the bridge may read the bot's token, and did not answer in time",
  );
}

/// Whether `error` says the vault is waiting for the person to allow the app to read the item,
/// which a later call answers once they did.
function askedFirst(error) {
  return String(error?.message ?? error).includes("may not read this vault item yet");
}

/// Answer the setup presses this bridge's record names, one each: what the person asked for, and
/// the id of the press that asked, so nothing is answered twice.
export async function serveSetup({ calls, fetch = globalThis.fetch, wait, log = () => {} }) {
  const answered = new Set();
  for await (const _ of calls.watch(SELF)) {
    const accounts = await calls.state(ACCOUNTS);
    const account = accounts?.accounts?.[0] ?? accounts;
    if (!account?.setup_wants || !account.setup_ask) continue;
    const press = `${account.setup_wants}:${account.setup_ask}`;
    if (answered.has(press)) continue;
    answered.add(press);
    await answerSetup({ calls, account, fetch, wait, log });
  }
}

/// What one setup press asks this process for: the token put in the person's vault, the bot's
/// servers, a server's channels, or where to sign in with Discord. Every answer goes back through
/// the app's own action, matched to the press.
export async function answerSetup({ calls, account, fetch = globalThis.fetch, wait, log = () => {} }) {
  const ask = account.setup_ask;
  const want = account.setup_wants;
  const did = (action, args) => calls.do({ app: SELF, action, args: { ask, ...args } });
  const failed = async (problem) => {
    log(`the setup did not work: ${problem}`);
    return did("setup_failed", { problem });
  };
  try {
    if (want === "token") {
      const item = await calls.ask({
        kind: "key",
        belongsTo: [ORIGIN],
        username: "Discord bot token",
        sentIn: SENT_IN,
      });
      if (!item) {
        return await failed("No token was kept, so there is nothing to bridge with.");
      }
      return await did("keep_token", { item });
    }
    const token = await released(calls, account.token_item, { wait });
    if (want === "find") {
      const answer = await discord(fetch, token, "/users/@me/guilds");
      return await did("found_servers", { servers: serversOf(answer) });
    }
    if (want === "channels") {
      if (!account.setup_server) {
        return await failed("No server was chosen, so there are no channels to read.");
      }
      const answer = await discord(fetch, token, guildChannels(account.setup_server));
      return await did("found_channels", { channels: channelsOf(answer) });
    }
    if (want === "sign-in") {
      if (!account.sign_in_page) {
        return await failed("This device does not offer signing in with Discord.");
      }
      return await did("start_sign_in", await signInStart(account.sign_in_page));
    }
    return await failed("This bridge does not know that step of the setup.");
  } catch (error) {
    return await failed(String(error?.message ?? error));
  }
}

/// One request to Discord as the bot whose token this process holds: its answer, whose bodies
/// only carry what a list of servers or channels needs.
async function discord(fetch, token, path, init = {}) {
  let answer;
  try {
    answer = await fetch(`${API}${path}`, {
      ...init,
      headers: {
        authorization: `Bot ${token}`,
        "user-agent": "DiscordBot (https://tilo.so, 0.1)",
        ...(init.headers ?? {}),
      },
    });
  } catch {
    throw new Error(whyRefused({ status: 0 }));
  }
  const status = Number(answer?.status ?? 0);
  if (status < 200 || status > 299) {
    throw new Error(whyRefused({ status }));
  }
  const body = await answer.text();
  return { status, body };
}

/// Where the person goes to sign in with Discord, and the digest of the state that binds what
/// comes back to them: the page's own state when it names one, else a fresh one the page is
/// given.
export async function signInStart(page) {
  const named = /[?&]state=([^&]+)/.exec(page);
  const state = named ? decodeURIComponent(named[1]) : hex(crypto.getRandomValues(new Uint8Array(16)));
  const url = named ? page : `${page}&state=${state}`;
  return { url, sha256: await sha256Hex(state) };
}

/// `bytes` as lowercase hexadecimal.
function hex(bytes) {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/// The SHA-256 digest of `text`, in lowercase hexadecimal: the state a sign-in comes back with
/// binds the person by it, and only it is kept.
export async function sha256Hex(text) {
  const bytes = new TextEncoder().encode(text);
  return hex(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)));
}

/// What one Discord event means, done on the workspace's side.
export async function carryFromDiscord({ calls, account, event, log }) {
  for (const call of fromDiscord(account, event)) {
    try {
      await calls.do(call);
    } catch (error) {
      log(`a Discord event did not reach the workspace: ${error?.message ?? error}`);
    }
  }
}

/// What the workspace says, carried to Discord: the changes the holder watches, never a call on
/// another device reaching Discord.
///
/// Each event is one message of a bridged conversation and what happened to it: `created` (a
/// message written here), `updated`, `deleted`, `reaction` or `unreaction`, with the emoji a
/// reaction gave. What the bridge sent is joined to the message there by the id Discord answered,
/// so an edit or a deletion reaches the message it is about.
export async function carryFromWorkspace({ calls, account, log = () => {} }) {
  const known = new Map();
  for (const entry of Object.values(account.map ?? {})) {
    if (entry?.kind === "message" && entry.discord && entry.tilo) {
      known.set(entry.tilo, entry.discord);
    }
  }
  for await (const event of calls.watch(`${CHAT}/conversations`)) {
    const message = event?.message ?? event?.value ?? event;
    if (!message || message.text === undefined) continue;
    const kind = event?.kind ?? "created";
    const conversation = event?.conversation ?? message.channel ?? "";
    const tilo = published(message.id);
    const there = known.get(tilo);
    try {
      if (kind === "created") {
        for (const op of toDiscord(account, conversation, message)) {
          const answer = await calls.webCall(
            webhookSend({ webhook: op.webhook, tokenItem: account.token_item }, op),
          );
          const made = messageIdOf(answer);
          if (made) {
            known.set(tilo, made);
            await calls.do({
              app: SELF,
              action: "map",
              args: { kind: "message", tilo, discord: made, origin: "tilo" },
            });
          }
        }
      } else if (kind === "updated" && there) {
        for (const op of editToDiscord(account, conversation, message, there)) {
          await calls.webCall(
            webhookEdit({ webhook: op.webhook, tokenItem: account.token_item }, op),
          );
        }
      } else if (kind === "deleted" && there) {
        const hook = mapping(account, "webhook", conversation)?.discord ?? "";
        for (const op of deleteToDiscord(account, conversation, there, hook)) {
          await calls.webCall(
            webhookDelete({ webhook: op.webhook, tokenItem: account.token_item }, op),
          );
        }
      } else if ((kind === "reaction" || kind === "unreaction") && there) {
        for (const op of reactToDiscord(
          account,
          conversation,
          message,
          event.emoji ?? "",
          kind === "reaction",
          there,
        )) {
          await calls.webCall(react({ tokenItem: account.token_item }, op));
        }
      }
    } catch (error) {
      log(`a workspace change did not reach Discord: ${error?.message ?? error}`);
    }
  }
}

/// The id of the message a webhook's answer made, from the vault's answer to the call.
function messageIdOf(answer) {
  if (!answer) return null;
  let body = answer.body ?? answer;
  if (typeof body === "string") {
    try {
      body = JSON.parse(body);
    } catch {
      return null;
    }
  }
  return typeof body?.id === "string" ? body.id : null;
}

/// Chat's app id, whose conversations the bridge carries.
const CHAT = "so.tilo.chat";

/// Whether this file is the process's entry: the daemon starts the package's script with no
/// window for its connection.
if (import.meta.main) {
  const log = (line) => console.error(`discord: ${line}`);
  try {
    const { Tilo } = await import("./src/sdk.js");
    await run({ tilo: Tilo(), log });
  } catch (error) {
    log(`the bridge stopped: ${error?.message ?? error}`);
    process.exitCode = 1;
  }
}
