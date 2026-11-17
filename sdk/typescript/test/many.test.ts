/** A bulk call: one all-or-nothing step per call, and every call one invocation group. */

import { describe, expect, it } from "bun:test";

import { Tilo, made } from "../src/index.ts";
import { tasks } from "../src/types/index.ts";
import { FakeApi, TASKS, WORKSPACE_ID } from "./fakeApi.ts";

async function tracker(api: FakeApi): Promise<tasks.Issues> {
  api.surface = tasks.SURFACE;
  const issues = tasks.issues(
    new Tilo({ url: "https://api.test", key: api.key, fetch: api.fetch }).workspace("jane-doe"),
    TASKS,
  );
  return (await issues.load()) as tasks.Issues;
}

function groupCalls(api: FakeApi) {
  return api.asked(
    "POST",
    `/v1/workspaces/${WORKSPACE_ID}/resources/${TASKS}/actions/group`,
  );
}

describe("a bulk call", () => {
  it("sends a few group calls of one invocation", async () => {
    const api = new FakeApi();
    const issues = await tracker(api);
    const items = Array.from({ length: 1200 }, (_, index) => ({
      title: `Issue ${index}`,
      status: "todo" as const,
    }));

    const answers = await issues.createMany(items);

    const calls = groupCalls(api);
    expect(calls.length).toBe(3);
    expect(new Set(calls.map((call) => call.headers["tilo-invocation"])).size).toBe(1);
    expect(new Set(calls.map((call) => call.headers["tilo-invocation-title"])).size).toBe(1);
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

  it("names an item an earlier one makes", async () => {
    const api = new FakeApi();
    const issues = await tracker(api);

    await issues.many("create", [
      { title: "Parent" },
      { title: "Child", parent: made(0) },
    ]);

    const call = groupCalls(api)[0];
    const members = (JSON.parse(call.body) as {
      actions: { args: Record<string, unknown> }[];
    }).actions;
    expect(members[1]?.args["parent"]).toEqual({ $made: { member: 0, item: 0 } });
  });

  it("sends nothing for an empty list", async () => {
    const api = new FakeApi();
    const issues = await tracker(api);
    const sent = api.calls.length;

    expect(await issues.createMany([])).toEqual([]);
    expect(api.calls.length).toBe(sent);
  });

  it("is a step of the invocation the caller opened", async () => {
    const api = new FakeApi();
    const issues = await tracker(api);

    await new Tilo({ url: "https://api.test", key: api.key, fetch: api.fetch })
      .workspace("jane-doe")
      .invocation("Import the backlog", async () => {
        await issues.many("create", [{ title: "One" }, { title: "Two" }]);
      });

    const calls = groupCalls(api);
    expect(calls.length).toBe(1);
    expect(calls[0]?.headers["tilo-invocation-title"]).toBe("Import the backlog");
  });
});
