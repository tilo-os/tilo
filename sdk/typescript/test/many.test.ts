/** A bulk call: one all-or-nothing step per call, and every call one run. */

import { describe, expect, it } from "bun:test";

import { Tilo } from "../src/index.ts";
import { tasks } from "../src/types/index.ts";
import { FakeApi, TASKS, WORKSPACE_ID } from "./fakeApi.ts";

async function tracker(api: FakeApi): Promise<tasks.Tracker> {
  api.surface = tasks.SURFACE;
  const issues = tasks.tracker(
    new Tilo({ url: "https://api.test", key: api.key, fetch: api.fetch }).workspace("jane-doe"),
    TASKS,
  );
  return (await issues.load()) as tasks.Tracker;
}

function groupCalls(api: FakeApi) {
  return api.asked(
    "POST",
    `/v1/workspaces/${WORKSPACE_ID}/resources/${TASKS}/actions/group`,
  );
}

describe("a bulk call", () => {
  it("sends a few group calls of one run", async () => {
    const api = new FakeApi();
    const issues = await tracker(api);
    const items = Array.from({ length: 1200 }, (_, index) => ({
      title: `Issue ${index}`,
      status: "todo" as const,
    }));

    const answers = await issues.createMany(items);

    const calls = groupCalls(api);
    expect(calls.length).toBe(3);
    expect(new Set(calls.map((call) => call.headers["tilo-run"])).size).toBe(1);
    expect(new Set(calls.map((call) => call.headers["tilo-run-title"])).size).toBe(1);
    const sent = calls.flatMap(
      (call) => (JSON.parse(call.body) as { actions: unknown[] }).actions,
    );
    expect(sent).toEqual(items.map((args) => ({ action: "create", args })));
    expect(answers.map((answer) => answer.result)).toEqual([
      { applied: 500 },
      { applied: 500 },
      { applied: 200 },
    ]);
  });

  it("sends nothing for an empty list", async () => {
    const api = new FakeApi();
    const issues = await tracker(api);
    const sent = api.calls.length;

    expect(await issues.createMany([])).toEqual([]);
    expect(api.calls.length).toBe(sent);
  });

  it("is a step of the run the caller opened", async () => {
    const api = new FakeApi();
    const issues = await tracker(api);

    await new Tilo({ url: "https://api.test", key: api.key, fetch: api.fetch })
      .workspace("jane-doe")
      .run("Import the backlog", async () => {
        await issues.many("create", [{ title: "One" }, { title: "Two" }]);
      });

    const calls = groupCalls(api);
    expect(calls.length).toBe(1);
    expect(calls[0]?.headers["tilo-run-title"]).toBe("Import the backlog");
  });
});
