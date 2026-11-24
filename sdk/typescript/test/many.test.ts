/** A bulk call: one all-or-nothing step per call, and every call one invocation group. */

import { describe, expect, it } from "bun:test";

import { type Transport, type Group, Tilo } from "../src/index.ts";

const WORKSPACE = "6f9619ff-8b86-d011-b42d-00c04fc964ff";
const RESOURCE = "tasks/issues";

interface Call {
  workspace: string | null;
  tool: string;
  args: Record<string, unknown>;
  group: Group | null;
}

/** A transport that records the tools it is asked for and answers a group call's count. */
class RecordingTransport implements Transport {
  readonly calls: Call[] = [];

  async workspace(): Promise<string | null> {
    return WORKSPACE;
  }

  async call(
    workspace: string | null,
    tool: string,
    args: Record<string, unknown>,
    group: Group | null,
  ): Promise<Record<string, unknown>> {
    this.calls.push({ workspace, tool, args, group });
    if (tool !== "tilo_do") {
      throw new Error(`the bulk call asked the transport for ${tool}`);
    }
    const inner = args.args !== null && typeof args.args === "object" ? (args.args as Record<string, unknown>) : {};
    const members = Array.isArray(inner.actions) ? inner.actions : [];
    return {
      status: "ok",
      invocation_id: `run-${this.calls.length}`,
      result: { applied: members.length },
    };
  }
}

function issues(transport: RecordingTransport) {
  return new Tilo({ transport }).workspace(WORKSPACE).resource(RESOURCE);
}

describe("a bulk call", () => {
  it("sends a few group calls of one invocation", async () => {
    const transport = new RecordingTransport();
    const items = Array.from({ length: 1200 }, (_, index) => ({ title: `Issue ${index}` }));

    const answers = await issues(transport).many("create", items);

    expect(transport.calls).toHaveLength(3);
    const sent = transport.calls.map((call) => {
      const inner = call.args.args as { actions: unknown[] };
      return inner.actions;
    });
    expect(sent.map((actions) => actions.length)).toEqual([500, 500, 200]);
    expect(sent.flat()).toEqual(items.map((args) => ({ action: "create", args })));
    expect(new Set(transport.calls.map((call) => call.group?.id)).size).toBe(1);
    expect(new Set(transport.calls.map((call) => call.group?.title))).toEqual(new Set(["create (1200)"]));
    expect(transport.calls.map((call) => call.workspace)).toEqual([WORKSPACE, WORKSPACE, WORKSPACE]);
    expect(answers.map((answer) => answer.result)).toEqual([
      { applied: 500 },
      { applied: 500 },
      { applied: 200 },
    ]);
  });

  it("sends nothing for an empty list", async () => {
    const transport = new RecordingTransport();
    const workspace = new Tilo({ transport }).workspace(WORKSPACE);

    expect(await workspace.resource(RESOURCE).many("create", [])).toEqual([]);
    expect(transport.calls).toEqual([]);
  });

  it("is a step of the invocation the caller opened", async () => {
    const transport = new RecordingTransport();
    const workspace = new Tilo({ transport }).workspace(WORKSPACE);

    await workspace.invocation("Import the backlog", async () => {
      await workspace.resource(RESOURCE).many("create", [{ title: "One" }, { title: "Two" }]);
    });

    expect(transport.calls).toHaveLength(1);
    expect(transport.calls[0]?.group?.title).toBe("Import the backlog");
  });
});
