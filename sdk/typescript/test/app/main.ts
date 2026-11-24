/**
 * An app's process, as the daemon launches it through tilo-launcher for the app's connection:
 * the SDK reaches the workspace over the connection the launcher handed it (`TILO_SVC_NATIVE_FD`),
 * as the app. `daemon.test.ts` packs it with the two vault items it names in `items.json`, installs
 * it, and reads what it did from the lines it writes to its error output, which the daemon's log
 * keeps. A secret is never written: a released one is told by its SHA-256.
 *
 * It asks for its first item until its person allows it and it is released, then once for a second
 * item, and keeps its connection until the daemon stops it.
 */

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { Refused, Tilo } from "../../src/index.ts";

interface Items {
  first: string;
  second: string;
}

/** How long the person has to grant the first item. */
const GRANT_WAIT_MS = 120_000;

const mount = process.env.TILO_APP_MODULE;
if (mount === undefined || mount === "") {
  throw new Error("started outside the app sandbox: TILO_APP_MODULE names no package");
}
const items = JSON.parse(readFileSync(join(mount, "items.json"), "utf8")) as Items;

function say(line: string): void {
  console.error(`sdk-app: ${line}`);
}

/** One line of what refused a release, or failed it otherwise; never a secret. */
function why(error: unknown): string {
  if (error instanceof Refused) {
    return `refused ${error.code} ${error.message.replaceAll("\n", " ")}`;
  }
  const message = error instanceof Error ? error.message : String(error);
  return `failed ${message.replaceAll("\n", " ")}`;
}

function digest(secret: string): string {
  return createHash("sha256").update(secret, "utf8").digest("hex");
}

const workspace = new Tilo().workspace();
say(`started in ${await workspace.id()}`);

// The first item: refused while its person is asked, then released once they allowed it.
const until = Date.now() + GRANT_WAIT_MS;
let told = false;
for (;;) {
  try {
    say(`first released ${digest(await workspace.vault.release(items.first))}`);
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

// A second item is asked about on its own: the first one's grant covers that item alone.
try {
  say(`second released ${digest(await workspace.vault.release(items.second))}`);
} catch (error) {
  say(`second ${why(error)}`);
}

// The connection stays open until the daemon stops the app.
setInterval(() => {}, 1 << 30);
