/** A watch its reader stops ends then, however quiet the workspace is. */

import { describe, expect, it } from "bun:test";

import { type Group, type Transport, Tilo } from "../src/index.ts";

const WORKSPACE = "6f9619ff-8b86-d011-b42d-00c04fc964ff";

/** A workspace where nothing happens: each watch call waits its while and answers no event. */
class QuietTransport implements Transport {
  watches = 0;

  async workspace(): Promise<string | null> {
    return WORKSPACE;
  }

  async call(
    _workspace: string | null,
    tool: string,
    _args: Record<string, unknown>,
    _group: Group | null,
  ): Promise<Record<string, unknown>> {
    if (tool !== "tilo_watch") {
      throw new Error(`the watch asked the transport for ${tool}`);
    }
    this.watches += 1;
    await new Promise((resolve) => setTimeout(resolve, 20));
    return { events: [], cursor: `c${this.watches}` };
  }
}

describe("a watch", () => {
  it("ends when its reader stops it while it waits for an event", async () => {
    const transport = new QuietTransport();
    const events = new Tilo({ transport }).workspace(WORKSPACE).watch();
    const reading = (async () => {
      const seen = [];
      for await (const event of events) {
        seen.push(event);
      }
      return seen;
    })();
    await new Promise((resolve) => setTimeout(resolve, 100));

    await events.return(undefined);
    expect(await reading).toEqual([]);

    // It asks the workspace for no more once the call it was waiting on answers.
    await new Promise((resolve) => setTimeout(resolve, 60));
    const asked = transport.watches;
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(transport.watches).toBe(asked);
  });
});
