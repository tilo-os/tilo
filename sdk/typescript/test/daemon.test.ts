/**
 * The SDK against a real daemon, end to end.
 *
 * `TILO_RUNTIME_PROFILE` names a profile whose daemon this computer runs:
 * `tools/ci/sdk-native.sh` starts one in a profile of its own and runs this
 * suite with it set. Without it there is no daemon to call, and the suite
 * skips.
 */

import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import { afterAll, describe, expect, setDefaultTimeout, test } from "bun:test";

import {
  type ChangeEvent,
  Refused,
  Tilo,
  type Listing,
  type Row,
  type Workspace,
} from "../src/index.ts";

const PROFILE = process.env.TILO_RUNTIME_PROFILE;
const suite = PROFILE ? describe : describe.skip;

if (!PROFILE) {
  console.log(
    "tilo-sdk: skipping the daemon suite: TILO_RUNTIME_PROFILE names no daemon " +
      "(tools/ci/sdk-native.sh runs it against one)",
  );
}

// A fresh daemon starts its workspace's apps and reads its identity on the first call.
setDefaultTimeout(60_000);

const tilo = new Tilo();
const once = `sdk-${crypto.randomUUID().slice(0, 8)}`;

afterAll(() => {
  tilo.close();
});

/** Every row of a listing, every page of it. */
async function rows(listing: Listing): Promise<Row[]> {
  const all: Row[] = [];
  for await (const row of listing) {
    all.push(row);
  }
  return all;
}

/** The Tasks app's action that makes a task, and the task list it adds it to. */
const NEW_TASK = "so.tilo.tasks#new_task";
const TASK_LIST = "so.tilo.tasks/tl:so.tilo.tasks";

/** The id a call that made something names it by: its result's reference, or the resources of
 * the answer it kept. */
function made(answer: unknown): string {
  if (typeof answer === "string") {
    return answer;
  }
  const raw = answer as Record<string, unknown> | null;
  if (typeof raw?.id === "string") {
    return raw.id;
  }
  const found = raw?.resources;
  if (!Array.isArray(found) || found.length === 0) {
    throw new Error(`the call names what it made: ${JSON.stringify(answer)}`);
  }
  const first = found[0];
  return typeof first === "string"
    ? first
    : String((first as Record<string, unknown>).id);
}

suite("a daemon", () => {
  test("serves its workspace", async () => {
    const workspace = tilo.workspace();

    const id = await workspace.id();

    expect(id).not.toBe("");
    expect((await workspace.info()).id).toBe(id);
  });

  test("lists its apps", async () => {
    const apps = await rows(await tilo.workspace().ls());

    expect(apps.length).toBeGreaterThan(0);
    expect(apps.some((row) => row.path !== "")).toBe(true);
  });

  test("runs an action, and reads the record it changed", async () => {
    const workspace = tilo.workspace();
    const title = `task ${once}`;

    await workspace.do(NEW_TASK, { title });
    const tasks = await workspace.read(TASK_LIST);

    expect(JSON.stringify(tasks.fields)).toContain(title);
  });

  test("follows a change with a watch", async () => {
    const workspace = tilo.workspace();
    const events = workspace.watch();
    const seen = (async () => {
      for await (const event of events) {
        if (event.kind === "change") {
          return event;
        }
      }
      return null;
    })();

    // The daemon reads the watch from now: a change before it is registered is not one it saw.
    await new Promise((resolve) => setTimeout(resolve, 500));
    await workspace.do(NEW_TASK, { title: `watched ${once}` });

    let timeout: ReturnType<typeof setTimeout> | undefined;
    const late = new Promise<null>((resolve) => {
      timeout = setTimeout(() => resolve(null), 30_000);
    });
    const event = await Promise.race([seen, late]);
    clearTimeout(timeout);
    await events.return(undefined);

    expect(event).not.toBeNull();
    expect(event?.kind).toBe("change");
  });

  test("says a new message was created and its edit was edited", async () => {
    const workspace = tilo.workspace();
    const channel = made(await workspace.do("so.tilo.chat#new_channel", { name: `sdk-watch-${once}` }));
    const events = workspace.watch();
    const seen: ChangeEvent[] = [];
    const watching = (async () => {
      for await (const event of events) {
        if (event.kind === "change") {
          seen.push(event);
        }
      }
    })();

    // The daemon reads the watch from now: a change before it is registered is not one it saw.
    await new Promise((resolve) => setTimeout(resolve, 1_000));
    const message = made(await workspace.do(`so.tilo.chat/${channel}#send`, { body: "first words" }));
    await workspace.do(`so.tilo.chat/${message}#edit`, { body: "second words" });

    const told = (): ChangeEvent[] =>
      seen.filter((event) => JSON.stringify(event.raw).includes(message));
    const deadline = Date.now() + 30_000;
    while (told().length < 2 && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
    await events.return(undefined);
    await watching;

    const found = told();
    expect(found.length).toBeGreaterThanOrEqual(2);
    const sent = found[0];
    const edited = found[found.length - 1];
    // The store says what a change did to its record, next to `change`: the new message reads
    // created and its edit reads edited, in the event's own `what`.
    expect(sent?.what).toBe("created");
    expect(edited?.what).toBe("edited");
    expect(sent?.change).not.toBe(edited?.change);
  });

  test("throws a refusal with its code", async () => {
    const workspace = tilo.workspace();

    const failure = await workspace.do("nosuchapp/x#nothing").catch((error: unknown) => error);

    expect(failure).toBeInstanceOf(Refused);
    expect((failure as Refused).code).toBe("unknown_app");
  });

  test("puts a file and gets the same bytes back", async () => {
    const workspace = tilo.workspace();
    // Past one append, so more than one piece goes up.
    const bytes = new Uint8Array(384 * 1024 + 17);
    for (let index = 0; index < bytes.length; index += 1) {
      bytes[index] = (index * 7) & 0xff;
    }

    const content = await workspace.files.upload(bytes);
    const back = await workspace.files.download(content);

    expect(content.startsWith("tilo.content:")).toBe(true);
    expect(back.length).toBe(bytes.length);
    expect(Buffer.from(back).equals(Buffer.from(bytes))).toBe(true);
  });
});

/**
 * What `tools/ci/sdk-native.sh build` made an app of the suite's from: a throwaway publisher the
 * daemon trusts, the runtimes it names (the bun one carrying the SDK's addon) and the packer.
 */
const KIT = process.env.TILO_SDK_APP_KIT;
/** The suite's app. */
const APP = "so.tilo.sdk-release-probe";
/** The app that calls a service with a vault item's key. */
const WEBCALL_APP = "so.tilo.sdk-webcall-probe";
const APP_VERSION = "0.1.0";
/** Where each item's secret belongs: what tells their asks apart. */
const FIRST_PLACE = "https://first.example.test";
const SECOND_PLACE = "https://second.example.test";
/** The most one upload piece carries. */
const UPLOAD_PIECE = 256 * 1024;
/** How long the daemon has to launch the app and the app to say what it did. */
const SAID_WAIT_MS = 120_000;

/** `program args`, run to its end: what it printed, or the failure with what it said. */
function run(program: string, args: string[]): string {
  const ran = Bun.spawnSync([program, ...args], { stdout: "pipe", stderr: "pipe" });
  const out = ran.stdout.toString();
  if (ran.exitCode !== 0) {
    throw new Error(`${program} ${args.join(" ")} exited ${ran.exitCode}: ${out}${ran.stderr.toString()}`);
  }
  return out;
}

/**
 * The app's package, signed by the kit's publisher: its entrypoint bundled with the SDK, the
 * addon and its worker where the SDK loads them, and the items it asks for. Its path, its
 * package root and its publisher, as an install names them.
 */
async function packApp(
  kit: string,
  id: string,
  entry: string,
  items: Record<string, unknown>,
) {
  const dir = mkdtempSync(join(tmpdir(), "sdk-app-"));
  const payload = join(dir, "payload");
  const built = await Bun.build({
    entrypoints: [entry],
    outdir: payload,
    naming: "main.js",
    target: "bun",
    external: ["tilo-sdk-native"],
  });
  if (!built.success) {
    throw new Error(`the app does not build: ${built.logs.join("\n")}`);
  }
  writeFileSync(join(payload, "items.json"), JSON.stringify(items));

  const bun = readFileSync(join(kit, "bun.provides"), "utf8").trim();
  const runtime = readFileSync(join(kit, "app.runtime"), "utf8").trim();
  const manifest = join(dir, "manifest.toml");
  writeFileSync(
    manifest,
    `[package]
id = "${id}"
version = "${APP_VERSION}"
kind = "app"
runtime = "${runtime}"
required-runtimes = ["${bun}"]

[publisher]
identity = "ed25519:${"00".repeat(32)}"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[sandbox.services]
native = "required@40"

[entrypoint]
type = "cli"
runtime = "${bun}"
bin = "bin/bun"
script = "main.js"
connection = true
`,
  );
  const packer = join(kit, "tilo-app-package");
  const tapp = join(dir, "app.tapp");
  run(packer, [
    "pack",
    "--manifest",
    manifest,
    "--payload",
    payload,
    "--output",
    tapp,
    "--identity",
    join(kit, "publisher.json"),
    "--local-build",
  ]);
  const inspected = run(packer, ["inspect", tapp]);
  const field = (name: string, prefix: string): string => {
    const line = inspected.split("\n").find((each) => each.startsWith(`${name}: ${prefix}`));
    if (line === undefined) {
      throw new Error(`the package says no ${name}: ${inspected}`);
    }
    return line.slice(`${name}: ${prefix}`.length).trim();
  };
  const bytes = new Uint8Array(readFileSync(tapp));
  rmSync(dir, { recursive: true, force: true });
  return {
    bytes,
    id,
    package: field("package-id", "tapp:"),
    publisher: field("publisher-identity", "ed25519:"),
  };
}

/** Hand the workspace the package in pieces, and install it as a developer's own build. */
async function install(
  workspace: Workspace,
  app: { bytes: Uint8Array; id: string; package: string; publisher: string },
): Promise<void> {
  // The workspace keeps it under its BLAKE3 hash.
  const hash = bytesToHex(blake3(app.bytes));
  for (let offset = 0; offset < app.bytes.length; offset += UPLOAD_PIECE) {
    const piece = app.bytes.subarray(offset, offset + UPLOAD_PIECE);
    await workspace.do("workspace/apps#upload", {
      file: hash,
      offset,
      total: app.bytes.length,
      data: Buffer.from(piece).toString("hex"),
    });
  }
  await workspace.do("workspace/apps#install", {
    app: app.id,
    version: APP_VERSION,
    package: app.package,
    publisher: app.publisher,
    dev: true,
    file: hash,
  });
}

/** The lines the app wrote, as the daemon's log in the profile keeps them. */
function appSaid(): string[] {
  const log = join(PROFILE ?? "", "daemon.log");
  if (!existsSync(log)) {
    return [];
  }
  return readFileSync(log, "utf8")
    .split("\n")
    .flatMap((line) => {
      const at = line.indexOf("sdk-app: ");
      return at < 0 ? [] : [line.slice(at + "sdk-app: ".length).trim()];
    });
}

/** The app's first line starting with `prefix`, waited for. */
async function said(prefix: string): Promise<string> {
  const until = Date.now() + SAID_WAIT_MS;
  for (;;) {
    const line = appSaid().find((each) => each.startsWith(prefix));
    if (line !== undefined) {
      return line.slice(prefix.length).trim();
    }
    if (Date.now() > until) {
      throw new Error(`the app never said "${prefix}"; it said: ${JSON.stringify(appSaid())}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
}

/** The questions the workspace waits on its person with about the item that belongs to `place`. */
async function asksAbout(workspace: Workspace, place: string): Promise<string[]> {
  const read = await workspace.readRaw("record:tilo.invocations:");
  const fields = (read.fields ?? {}) as { items?: { asked?: { record?: { record?: string } } }[] };
  return (fields.items ?? [])
    .map((item) => item.asked)
    .filter((each) => each !== undefined && each !== null && JSON.stringify(each).includes(place))
    .map((each) => each?.record?.record)
    .filter((record): record is string => typeof record === "string" && record !== "");
}

/** The one question about the item that belongs to `place`, waited for. */
async function askedAbout(workspace: Workspace, place: string): Promise<string> {
  const until = Date.now() + SAID_WAIT_MS;
  for (;;) {
    const asked = await asksAbout(workspace, place);
    if (asked.length > 0) {
      expect(asked).toHaveLength(1);
      return asked[0] as string;
    }
    if (Date.now() > until) {
      throw new Error(`nobody was asked about the item for ${place}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
}

/** A key the person keeps in the vault for `place`: its reference. */
async function kept(workspace: Workspace, place: string, secret: string): Promise<string> {
  const added = (await workspace.do("workspace/vault#add", {
    kind: "key",
    belongsTo: [place],
    secret,
  })) as { reference?: unknown };
  expect(typeof added.reference).toBe("string");
  return added.reference as string;
}

/** How the vault refuses a release its person has not allowed yet. */
const ASKED_FIRST = "may not read this vault item yet: your person is asked whether it may, once";
/** How the vault refuses a web call its person has not allowed yet. */
const CALL_ASKED_FIRST =
  "may not call this service with this vault item yet: your person is asked whether it may, once";

function sha256(text: string): string {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

suite("an app the daemon launches", () => {
  test(
    "is released its own vault item once its person allows it, and nothing else is",
    async () => {
      if (KIT === undefined || KIT === "") {
        throw new Error("TILO_SDK_APP_KIT names no app kit (tools/ci/sdk-native.sh names one)");
      }
      const kit = KIT;
      const workspace = tilo.workspace();
      const firstSecret = `first-${crypto.randomUUID()}`;
      const secondSecret = `second-${crypto.randomUUID()}`;
      const first = await kept(workspace, FIRST_PLACE, firstSecret);
      const second = await kept(workspace, SECOND_PLACE, secondSecret);

      await install(
        workspace,
        await packApp(kit, APP, join(import.meta.dir, "app", "main.ts"), {
          first,
          second,
        }),
      );

      // Its first release is refused while its person is asked, once.
      const refused = await said("first ");
      expect(refused).toStartWith("refused ");
      expect(refused).toContain(ASKED_FIRST);
      const record = await askedAbout(workspace, FIRST_PLACE);

      // The person approves and grants it; the app's next call is released exactly the secret.
      await workspace.do(`invocations/workspace/${record}#resolve`, {
        resolution: "approve_and_grant",
      });
      expect(await said("first released ")).toBe(sha256(firstSecret));

      // The grant covers that item alone: a second item asks again.
      const again = await said("second ");
      expect(again).toStartWith("refused ");
      expect(again).toContain(ASKED_FIRST);
      await askedAbout(workspace, SECOND_PLACE);

      // A plain SDK script, holding no app's connection, is refused even the granted item.
      let plain: unknown = null;
      try {
        await workspace.vault.release(first);
      } catch (error) {
        plain = error;
      }
      expect(plain).toBeInstanceOf(Refused);

      // No secret is in the daemon's log, which keeps everything the app wrote too.
      const log = readFileSync(join(PROFILE ?? "", "daemon.log"), "utf8");
      expect(log.includes(firstSecret)).toBe(false);
      expect(log.includes(secondSecret)).toBe(false);
      expect(String(plain).includes(firstSecret)).toBe(false);
    },
    SAID_WAIT_MS * 3,
  );

  test(
    "calls a service with a vault item's key once its person allows it, and asks once",
    async () => {
      if (KIT === undefined || KIT === "") {
        throw new Error("TILO_SDK_APP_KIT names no app kit (tools/ci/sdk-native.sh names one)");
      }
      const kit = KIT;
      const said_by_the_service: (string | null)[] = [];
      const service = Bun.serve({
        port: 0,
        hostname: "127.0.0.1",
        async fetch(request) {
          said_by_the_service.push(request.headers.get("authorization"));
          return new Response(await request.text());
        },
      });
      const url = `http://127.0.0.1:${service.port}`;
      try {
        const workspace = tilo.workspace();
        const firstSecret = `first-${crypto.randomUUID()}`;
        const secondSecret = `second-${crypto.randomUUID()}`;
        const first = await kept(workspace, url, firstSecret);
        const second = await kept(workspace, url, secondSecret);
        await install(
          workspace,
          await packApp(kit, WEBCALL_APP, join(import.meta.dir, "app", "webcall.ts"), {
            first,
            second,
            url,
          }),
        );

        // The app's first call is refused while its person is asked, once; nothing is sent.
        const refused = await said("webcall first refused ");
        expect(refused).toContain(CALL_ASKED_FIRST);
        const record = await askedAbout(workspace, url);
        expect(said_by_the_service).toHaveLength(0);

        // Their approve and grant lets the app call that service with that item: the request
        // reaches the service with the key, and the app is answered what the service said.
        await workspace.do(`invocations/workspace/${record}#resolve`, {
          resolution: "approve_and_grant",
        });
        expect(await said("webcall first answered ")).toBe("200 hello from the app");
        expect(said_by_the_service).toStrictEqual([`Bearer ${firstSecret}`]);

        // The grant covers that item: the next call is not asked again.
        expect(await said("webcall again answered ")).toBe("200 hello from the app");
        expect(said_by_the_service).toStrictEqual([
          `Bearer ${firstSecret}`,
          `Bearer ${firstSecret}`,
        ]);

        // Another item asks on its own, and nothing of it is sent.
        expect(await said("webcall second refused ")).toContain(CALL_ASKED_FIRST);
        expect(said_by_the_service).toHaveLength(2);

        // The app never saw the secret, and neither does the daemon's log.
        const log = readFileSync(join(PROFILE ?? "", "daemon.log"), "utf8");
        expect(log.includes(firstSecret)).toBe(false);
        expect(log.includes(secondSecret)).toBe(false);
      } finally {
        service.stop(true);
      }
    },
    SAID_WAIT_MS * 3,
  );
});
