// The Discord bridge: Discord's REST, gateway and voice on this side, the workspace's records,
// actions and calls on the other. It runs as the app's process, attested as this package, and it
// is the device's held connection that carries the gateway and the call lane a voice channel's
// call is bridged through.

import { startCalls } from "./src/call.js";
import { connect as connectVoice } from "./src/voice.js";
import {
  fromDiscord,
  toDiscord,
  editToDiscord,
  deleteToDiscord,
  reactToDiscord,
  threadToDiscord,
  mapping,
  published,
  mappings,
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
  startThread,
  whyRefused,
} from "./src/rest.js";

/// The bridge's record path: its own app's root, the accounts it bridges.
export const ACCOUNTS = `${SELF}/accounts`;

/// The id a call answered: a resource reference's id, or the id text itself.
export function referenceId(result) {
  if (!result) return null;
  if (typeof result === "string") return result;
  if (typeof result.id === "string") return result.id;
  if (typeof result.ref === "string") return result.ref;
  if (result.item && typeof result.item.id === "string") return result.item.id;
  if (typeof result.value === "string") return result.value;
  if (result.value && typeof result.value.id === "string") return result.value.id;
  return null;
}

/// A resource's published id, from the reference a call answered.
export function publishedOf(id) {
  const text = String(id ?? "");
  return text.startsWith("tl:") ? text : `tl:so.tilo.chat:${text}`;
}

/// The workspace as the bridge uses it: the SDK's one client, its calls by address, and the map
/// joining what a call made to the Discord id it is.
export function callsOf(ws) {
  return {
    ws,
    async do(call) {
      const made = await ws.do(`${call.app}#${call.action}`, call.args);
      const id = referenceId(made);
      if (call.map && id) {
        const tilo = call.app === "so.tilo.chat" ? publishedOf(id) : id;
        await ws.do(`${SELF}#map`, {
          kind: call.map.kind,
          tilo,
          discord: call.map.discord,
          origin: call.map.origin ?? "discord",
          persona: call.map.persona ?? "",
          root: call.map.root ?? "",
        });
      }
      return made;
    },
    async state(path) {
      const read = await ws.read(path);
      return read?.fields ?? read;
    },
    watch(path) {
      return ws.watch(path);
    },
    ask(question) {
      return ws.vault.ask(question);
    },
    webCall(request) {
      return ws.vault.webCall(request);
    },
    release(item) {
      return ws.vault.release(item);
    },
    content(bytes) {
      return ws.files?.upload ? ws.files.upload(bytes) : null;
    },
    /// This app's call lane, when the workspace client has one: the daemon's side of a call
    /// carried from an outside service. `null` for a client that carries none, which is a bridge
    /// that reads and writes but never joins a call.
    lane() {
      const lane = ws.callLane;
      if (!lane) return null;
      return {
        carry: (pair) => lane.carry(pair),
        report: (report) => lane.report(report),
        heard: (frame) => lane.heard(frame),
        said: () => lane.said(),
      };
    },
  };
}

/// How long to wait before asking again for a vault item whose person is being asked whether the
/// app may read it, and how many times.
export const RELEASE_RETRY_MS = 2_000;
export const RELEASE_TRIES = 90;

/// A procedure the process runs once: the bridge starts, answers the setup its person asks for,
/// and holds the gateway of the server it bridges until the connection ends. The account record
/// is watched: the daemon starts the program as soon as the app is installed, so the server the
/// person connects, or a token item or origin that changes, opens the gateway then, not only at
/// the start.
export async function run({
  tilo,
  makeClient,
  makeVoice,
  fetch = globalThis.fetch,
  wait,
  log = () => {},
}) {
  const calls = callsOf(tilo.workspace ? tilo.workspace() : tilo);
  // The setup is answered beside whatever the bridge carries: a press reaches this process
  // through the record, and nothing else does.
  const setup = serveSetup({ calls, fetch, wait, log }).catch((error) =>
    log(`the setup stopped: ${error?.message ?? error}`),
  );
  const holding = held({ calls, makeClient, makeVoice, fetch, wait, log }).catch((error) =>
    log(`the gateway stopped: ${error?.message ?? error}`),
  );
  await Promise.all([setup, holding]);
}

/// The server a bridge's record names, as what the held connection is keyed by: the gateway is
/// reopened when any of it changes, and closed when no server is connected.
function connectionOf(account) {
  if (!account?.guild_id || !account?.token_item) return null;
  return [
    account.guild_id,
    account.token_item ?? "",
    account.only_channel ?? "",
    account.api_origin ?? "",
    account.gateway_url ?? "",
  ].join("|");
}

/// Holds the gateway of the account the record names, reopening it when what it is keyed by
/// changes and closing it when no server is connected. The watch is the record's, so a person's
/// Connect reaches this process with no window open.
export async function held({ calls, makeClient, makeVoice, fetch = globalThis.fetch, wait, log = () => {} }) {
  let connection = null;
  let key = null;
  const look = async () => {
    const accounts = await calls.state(ACCOUNTS);
    const account = accounts?.accounts?.[0] ?? accounts;
    const next = connectionOf(account);
    if (next === key) return;
    key = next;
    if (connection) {
      await connection.close();
      connection = null;
    }
    if (!next) return;
    try {
      connection = await openGateway({ calls, account, makeClient, makeVoice, fetch, wait, log });
    } catch (error) {
      log(`the gateway did not open: ${error?.message ?? error}`);
    }
  };
  await look();
  for await (const _ of calls.watch(ACCOUNTS)) {
    await look();
  }
  if (connection) await connection.close();
}

/// Opens the gateway of `account` and carries each side's changes: the client, and how to close
/// both the connection and the watch of the workspace's messages.
export async function openGateway({
  calls,
  account,
  makeClient,
  makeVoice,
  fetch = globalThis.fetch,
  wait,
  log = () => {},
}) {
  const token = await released(calls, account.token_item, { wait });
  const stop = { stopped: false };
  // The call lane this app's connection was granted: a client that carries none leaves the
  // bridge reading and writing, never in a call.
  const lane = calls.lane();
  let client = null;
  const carried = lane
    ? startCalls({
        calls,
        lane,
        makeVoice: makeVoice ?? ((options) => connectVoice(options)),
        log,
        gateway: {
          get user() {
            return client?.user ?? null;
          },
          joinVoice: (guild, channel) => client.joinVoice(guild, channel),
          leaveVoice: (guild) => client.leaveVoice(guild),
        },
      })
    : null;
  if (!carried) {
    log("this build carries no call lane: the bridge reads and writes but joins no call");
  }
  client = await import("./src/gateway.js").then(({ connect }) =>
    connect({
      make: makeClient,
      token,
      url: account.gateway_url || undefined,
      onEvent: (event) => carryFromDiscord({ calls, account, event, fetch, log }),
      onVoice: (event) => carried?.hear(event),
      log,
    }),
  );
  await carried?.start();
  const carrying = carryFromWorkspace({ calls, account, log, stop }).catch((error) =>
    log(`the workspace's changes stopped reaching Discord: ${error?.message ?? error}`),
  );
  return {
    client,
    carrying,
    async close() {
      stop.stopped = true;
      carried?.close();
      if (typeof client?.destroy === "function") await client.destroy();
    },
  };
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
      const answer = await discord(fetch, token, "/users/@me/guilds", {}, account.api_origin);
      return await did("found_servers", { servers: serversOf(answer) });
    }
    if (want === "channels") {
      if (!account.setup_server) {
        return await failed("No server was chosen, so there are no channels to read.");
      }
      const answer = await discord(fetch, token, guildChannels(account.setup_server), {}, account.api_origin);
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
async function discord(fetch, token, path, init = {}, origin = API) {
  let answer;
  try {
    answer = await fetch(`${origin || API}${path}`, {
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

/// The largest file the workspace keeps from Discord: larger ones stay there.
export const MAX_FILE_BYTES = 32 * 1024 * 1024;

/// What one Discord event means, done on the workspace's side. A message's files are fetched from
/// where Discord serves them and kept in the workspace first, so the relayed message carries them.
export async function carryFromDiscord({
  calls,
  account,
  event,
  fetch = globalThis.fetch,
  log = () => {},
}) {
  for (const call of fromDiscord(account, event)) {
    try {
      const files = await keptFiles(calls, call.attachments, { fetch, log });
      if (files.length > 0) call.args = { ...call.args, attachments: files };
      delete call.attachments;
      await calls.do(call);
    } catch (error) {
      log(`a Discord event did not reach the workspace: ${error?.message ?? error}`);
    }
  }
}

/// The files of a Discord message, each fetched from `url` and kept in the workspace as the
/// content a message here names. One that does not come over is left out, and said.
export async function keptFiles(
  calls,
  attachments,
  { fetch = globalThis.fetch, log = () => {} } = {},
) {
  const files = [];
  for (const attachment of attachments ?? []) {
    const url = String(attachment?.url ?? "");
    if (url === "") continue;
    const size = Number(attachment?.size ?? 0);
    if (size > MAX_FILE_BYTES) {
      log(`a Discord file is larger than the workspace keeps (${size} bytes): it stays there`);
      continue;
    }
    try {
      const answer = await fetch(url);
      if (!answer?.ok) throw new Error(`Discord answered ${answer?.status ?? 0}`);
      const bytes = new Uint8Array(await answer.arrayBuffer());
      const content = await calls.content(bytes);
      if (!content) throw new Error("the workspace kept no bytes");
      files.push({
        file: {
          name: String(attachment?.name || "file"),
          media: String(attachment?.media ?? ""),
          size: bytes.length,
          content,
        },
      });
    } catch (error) {
      log(`a Discord file did not come over: ${error?.message ?? error}`);
    }
  }
  return files;
}

/// What the workspace says, carried to Discord: the changes the holder watches, never a call on
/// another device reaching Discord.
///
/// Each event is one message of a bridged conversation and what happened to it: `created` (a
/// message written here), `updated`, `deleted`, `reaction` or `unreaction`, with the emoji a
/// reaction gave. What the bridge sent is joined to the message there by the id Discord answered,
/// so an edit or a deletion reaches the message it is about.
export async function carryFromWorkspace({ calls, account, log = () => {}, stop = { stopped: false } }) {
  const known = new Map();
  for (const entry of mappings(account)) {
    if (entry?.kind === "message" && entry.discord && entry.tilo) {
      known.set(entry.tilo, entry.discord);
    }
  }
  for await (const event of calls.watch(`${CHAT}/conversations`)) {
    if (stop.stopped) break;
    const message = event?.message ?? event?.value ?? event;
    if (!message || message.text === undefined) continue;
    const kind = event?.kind ?? "created";
    const conversation = event?.conversation ?? message.channel ?? "";
    const tilo = published(message.id);
    const there = known.get(tilo);
    try {
      if (kind === "created") {
        for (const op of toDiscord(account, conversation, message)) {
          if (!(await carries(calls, "send_there", op, log))) continue;
          const answer = await calls.webCall(
            webhookSend(
              { webhook: op.webhook, tokenItem: account.token_item, origin: account.api_origin },
              op,
            ),
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
          if (!(await carries(calls, "edit_there", op, log))) continue;
          await calls.webCall(
            webhookEdit(
              { webhook: op.webhook, tokenItem: account.token_item, origin: account.api_origin },
              op,
            ),
          );
        }
      } else if (kind === "deleted" && there) {
        const hook = mapping(account, "webhook", conversation)?.discord ?? "";
        for (const op of deleteToDiscord(account, conversation, there, hook)) {
          if (!(await carries(calls, "delete_there", op, log))) continue;
          await calls.webCall(
            webhookDelete(
              { webhook: op.webhook, tokenItem: account.token_item, origin: account.api_origin },
              op,
            ),
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
          const carrying = op.op === "react" ? "react_there" : "unreact_there";
          if (!(await carries(calls, carrying, op, log))) continue;
          await calls.webCall(
            react({ tokenItem: account.token_item, origin: account.api_origin }, op),
          );
        }
      } else if (kind === "thread") {
        // A thread started here under a message the bridge sent: Discord starts it there, and
        // the two threads are joined by the id Discord answers.
        const parent = known.get(published(event.message?.id ?? ""));
        for (const op of threadToDiscord(
          account,
          conversation,
          message,
          parent,
          event.name,
          event.thread ?? message.id,
        )) {
          if (!(await carries(calls, "start_thread_there", op, log))) continue;
          const answer = await calls.webCall(
            startThread({ tokenItem: account.token_item, origin: account.api_origin }, op),
          );
          const made = messageIdOf(answer);
          if (made) {
            await calls.do({
              app: SELF,
              action: "map",
              args: { kind: "thread", tilo: op.tilo, discord: made, origin: "tilo" },
            });
          }
        }
      }
    } catch (error) {
      log(`a workspace change did not reach Discord: ${error?.message ?? error}`);
    }
  }
}

/// Have the bridge's own action take `op` before Discord hears of it: its guards say what the
/// bridge may do and where, so a call it refuses is never made. `true` when it was taken.
async function carries(calls, action, op, log) {
  try {
    await calls.do({ app: SELF, action, args: { op } });
    return true;
  } catch (error) {
    log(`Discord's bridge refused a call: ${error?.message ?? error}`);
    return false;
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
    // The client of this computer's daemon is the process's entry alone: the tests drive `run`
    // with their own workspace, so they need nothing of the SDK's native part.
    const { Tilo } = await import("tilo-sdk");
    await run({ tilo: new Tilo(), log });
  } catch (error) {
    log(`the bridge stopped: ${error?.message ?? error}`);
    process.exitCode = 1;
  }
}
