/** A workspace's call lane: an app's held connection carries one, any other connection none. */

import { describe, expect, it } from "bun:test";

import {
  type CallFrame,
  type CallReport,
  type CallTaken,
  type CarriedCall,
  type Group,
  Refused,
  Tilo,
  type Transport,
} from "../src/index.ts";

const WORKSPACE = "6f9619ff-8b86-d011-b42d-00c04fc964ff";

/** A connection that is no app's: it answers tools and carries no call. */
class PlainTransport implements Transport {
  async workspace(): Promise<string | null> {
    return WORKSPACE;
  }

  async call(
    _workspace: string | null,
    tool: string,
    _args: Record<string, unknown>,
    _group: Group | null,
  ): Promise<Record<string, unknown>> {
    throw new Error(`the lane asked the transport for ${tool}`);
  }
}

/** An app's connection, as the daemon answers its lane: what it was told, and what it hands back. */
class LaneTransport extends PlainTransport {
  readonly carried: (CarriedCall | null)[] = [];
  readonly reports: CallReport[] = [];
  readonly heard: CallFrame[] = [];
  pending: CallTaken[] = [];

  async carryCall(pair: CarriedCall | null): Promise<void> {
    this.carried.push(pair);
  }

  async reportCall(report: CallReport): Promise<void> {
    this.reports.push(report);
  }

  hearCall(frame: CallFrame): void {
    this.heard.push(frame);
  }

  async takeCall(): Promise<CallTaken> {
    return this.pending.shift() ?? { frames: [], asked: null };
  }
}

describe("a call lane", () => {
  it("is refused on a connection that is no app's", async () => {
    const lane = new Tilo({ transport: new PlainTransport() }).workspace(WORKSPACE).callLane;
    for (const attempt of [
      () => lane.carry({ outside: "458", tilo: "voice-tl" }),
      () => lane.report({ calls: [], link: { link: "idle" } }),
      () => lane.heard({ speaker: "7001", opus: new Uint8Array([1]) }),
      () => lane.said(),
      () => lane.asked(),
    ]) {
      const refused = await attempt().catch((error: unknown) => error);
      expect(refused).toBeInstanceOf(Refused);
      expect((refused as Refused).code).toBe("not_permitted");
    }
  });

  it("carries a call, and keeps an ask it took while taking frames", async () => {
    const transport = new LaneTransport();
    const lane = new Tilo({ transport }).workspace(WORKSPACE).callLane;
    await lane.carry({ outside: "458", tilo: "voice-tl" });
    await lane.report({
      calls: [{ call: "458", people: [{ account: "7001", name: "Dana", muted: true }] }],
      link: { link: "joined", call: "458" },
    });
    await lane.heard({ speaker: "7001", opus: new Uint8Array([1, 2]) });
    expect(transport.carried).toEqual([{ outside: "458", tilo: "voice-tl" }]);
    expect(transport.reports[0]?.calls[0]?.people[0]?.name).toBe("Dana");
    expect(transport.heard.map((frame) => [...frame.opus])).toEqual([[1, 2]]);

    // One take hands over both the frames and an ask: the frames are said, the ask kept.
    transport.pending.push({
      frames: [{ speaker: "aa", opus: new Uint8Array([3]) }],
      asked: { call: "458", status: "On Tilo: Grace" },
    });
    const said = await lane.said();
    expect(said.map((frame) => [...frame.opus])).toEqual([[3]]);
    expect(await lane.asked()).toEqual({ call: "458", status: "On Tilo: Grace" });
    expect(await lane.asked()).toBeNull();

    // An ask taken while asking keeps the frames for the next `said`.
    transport.pending.push({
      frames: [{ speaker: "aa", opus: new Uint8Array([4]) }],
      asked: { call: null, status: null },
    });
    expect(await lane.asked()).toEqual({ call: null, status: null });
    expect((await lane.said()).map((frame) => [...frame.opus])).toEqual([[4]]);

    await lane.carry(null);
    expect(transport.carried.at(-1)).toBeNull();
  });
});
