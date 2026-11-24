/**
 * The SDK against a real daemon, end to end.
 *
 * `TILO_RUNTIME_PROFILE` names a profile whose daemon this computer runs:
 * `tools/ci/sdk-native.sh` starts one in a profile of its own and runs this
 * suite with it set. Without it there is no daemon to call, and the suite
 * skips.
 */

import { afterAll, describe, expect, setDefaultTimeout, test } from "bun:test";

import { Refused, Tilo, type Listing, type Row } from "../src/index.ts";

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
