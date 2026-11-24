// The Discord bridge: Discord's REST and gateway on this side, the workspace's records and
// actions on the other. It runs as the app's process, attested as this package, and it is the
// device's held connection that carries the gateway; a process started for a call writes records
// the holder watches and never opens the gateway.

import { Calls } from "./src/tilo.js";
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
  webhookSend,
  webhookEdit,
  webhookDelete,
  react,
} from "./src/rest.js";

/// The bridge's record path: its own app's root, the accounts it bridges.
export const ACCOUNTS = `${SELF}/accounts`;

/// A procedure the process runs once: the bridge starts, reads its account, and holds the
/// gateway until the connection ends.
export async function run({ tilo, makeClient, log = () => {} }) {
  const calls = new Calls(tilo.workspace ? tilo.workspace() : tilo);
  const accounts = await calls.state(ACCOUNTS);
  const account = accounts?.accounts?.[0] ?? accounts;
  if (!account?.guild_id) {
    log("no Discord server is connected");
    return;
  }
  const token = await calls.release(account.token_item);
  const client = await import("./src/gateway.js").then(({ connect }) =>
    connect({
      make: makeClient,
      token,
      onEvent: (event) => carryFromDiscord({ calls, account, event, log }),
      log,
    }),
  );
  await carryFromWorkspace({ calls, account, log });
  return client;
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
