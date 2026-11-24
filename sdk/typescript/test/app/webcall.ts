/**
 * An app's process asking the vault to call a service with an item's key: the daemon launched it
 * for the app's connection, and `ws.vault.webCall` reaches the vault on that connection. The
 * vault makes the request itself, so the key never reaches this process. `daemon.test.ts` starts
 * the service, packs this with the two items and their service's address, installs it, and reads
 * what it did from the lines it writes to its error output, which the daemon's log keeps.
 *
 * Its first call waits for its person's approve and grant, once; the next is not asked again; a
 * second item asks on its own. It keeps its connection until the daemon stops it.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";

import { Refused, Tilo } from "../../src/index.ts";

interface Items {
  first: string;
  second: string;
  url: string;
}

/** How long the person has to grant the first item. */
const GRANT_WAIT_MS = 120_000;

const mount = process.env.TILO_APP_MODULE;
if (mount === undefined || mount === "") {
  throw new Error("started outside the app sandbox: TILO_APP_MODULE names no package");
}
const items = JSON.parse(readFileSync(join(mount, "items.json"), "utf8")) as Items;

function say(line: string): void {
  console.error(`sdk-app: webcall ${line}`);
}

/** One line of what refused a call, or failed it otherwise; never a secret. */
function why(error: unknown): string {
  if (error instanceof Refused) {
    return `refused ${error.code} ${error.message.replaceAll("\n", " ")}`;
  }
  const message = error instanceof Error ? error.message : String(error);
  return `failed ${message.replaceAll("\n", " ")}`;
}

const workspace = new Tilo().workspace();

/** The vault calls the test service with `key`, sending the app's body. */
async function call(key: string): Promise<string> {
  const answer = await workspace.vault.webCall({
    url: `${items.url}/upload`,
    key,
    method: "POST",
    body: "hello from the app",
  });
  return `${answer.status} ${answer.body}`;
}

// The first item: refused while its person is asked, then answered once they allowed it.
const until = Date.now() + GRANT_WAIT_MS;
let told = false;
for (;;) {
  try {
    say(`first answered ${await call(items.first)}`);
    break;
  } catch (error) {
    if (!told) {
      told = true;
      say(`first ${why(error)}`);
    }
    if (!(error instanceof Refused) || Date.now() > until) {
      say(`first gave up ${why(error)}`);
      break;
    }
  }
  await new Promise((resolve) => setTimeout(resolve, 500));
}

// The item's grant covers that service: this call is not asked again.
try {
  say(`again answered ${await call(items.first)}`);
} catch (error) {
  say(`again ${why(error)}`);
}

// A second item is asked about on its own: the first one's grant covers that item alone.
try {
  say(`second answered ${await call(items.second)}`);
} catch (error) {
  say(`second ${why(error)}`);
}

// The connection stays open until the daemon stops the app.
setInterval(() => {}, 1 << 30);
