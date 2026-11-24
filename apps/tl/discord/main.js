// The Discord bridge: Discord's REST and gateway on this side, the workspace's records and
// actions on the other. It runs as the app's process, attested as so.tilo.discord, and it is the
// device's held connection that carries the gateway; a process started for a call writes records
// the holder watches and never opens the gateway.

import { Calls } from "./src/tilo.js";
import { fromDiscord, toDiscord, editToDiscord, deleteToDiscord, reactToDiscord, carried, byDiscord } from "./src/plan.js";
import { webhookSend } from "./src/rest.js";

/// The bridge's record path: its own app's root, the accounts it bridges.
export const ACCOUNTS = "so.tilo.discord/accounts";

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
export async function carryFromWorkspace({ calls, account, log }) {
  for await (const event of calls.watch("so.tilo.chat/conversations")) {
    const conversation = event?.path ?? event?.id ?? "";
    const message = event?.value ?? event;
    if (!message?.text) continue;
    if (byDiscord(account, "message", message.id)) continue;
    for (const op of toDiscord(account, conversation, message)) {
      try {
        await calls.webCall(
          webhookSend({ webhook: op.webhook, tokenItem: account.token_item }, op),
        );
      } catch (error) {
        log(`a workspace message did not reach Discord: ${error?.message ?? error}`);
      }
    }
  }
}
