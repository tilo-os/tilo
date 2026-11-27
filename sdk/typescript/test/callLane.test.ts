/** A workspace's call lane: an app's own daemon connection carries one, a key's client none. */

import { describe, expect, it } from "bun:test";

import {
  type CallFrame,
  type CallReport,
  type CarriedCall,
  type DaemonConnection,
  Refused,
  Tilo,
} from "../src/index.ts";

const WORKSPACE = "6f9619ff-8b86-d011-b42d-00c04fc964ff";

/** An app's connection, as the daemon answers its lane: what it was told, and what it hands back. */
class AppConnection implements DaemonConnection {
  readonly workspace = WORKSPACE;
  readonly carried: (CarriedCall | null)[] = [];
  readonly reports: CallReport[] = [];
  readonly heard: CallFrame[] = [];
  pending: ReturnType<DaemonConnection["saidCall"]>[] = [];

  async request(): Promise<never> {
    throw new Error("the lane made a request of the API");
  }
  async webCall(): Promise<never> {
    throw new Error("the lane made a web call");
  }
  async release(): Promise<never> {
    throw new Error("the lane released a secret");
  }
  async find(): Promise<never> {
    throw new Error("the lane looked for a secret");
  }
  async carryCall(pair: CarriedCall | null) {
    this.carried.push(pair);
    return { answer: {} };
  }
  async reportCall(report: CallReport) {
    this.reports.push(report);
    return { answer: {} };
  }
  hearCall(speaker: string, opus: Uint8Array) {
    this.heard.push({ speaker, opus });
    return null;
  }
  saidCall() {
    return this.pending.shift() ?? { frames: [], asked: null };
  }
}

describe("a call lane", () => {
  it("is refused to a key's client", async () => {
    const lane = new Tilo({ key: "tilo_sk_test", url: "https://api.test" }).workspace(WORKSPACE).callLane;
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
    const connection = new AppConnection();
    const workspace = new Tilo({ daemon: connection }).workspace();
    const lane = workspace.callLane;
    expect(await workspace.id()).toBe(WORKSPACE);

    await lane.carry({ outside: "458", tilo: "voice-tl" });
    await lane.report({
      calls: [{ call: "458", people: [{ account: "7001", name: "Dana", muted: true }] }],
      link: { link: "joined", call: "458" },
    });
    await lane.heard({ speaker: "7001", opus: new Uint8Array([1, 2]) });
    expect(connection.carried).toEqual([{ outside: "458", tilo: "voice-tl" }]);
    expect(connection.reports[0]?.calls[0]?.people[0]?.name).toBe("Dana");
    expect(connection.heard.map((frame) => [...frame.opus])).toEqual([[1, 2]]);

    // One take hands over both the frames and an ask: the frames are said, the ask kept.
    connection.pending.push({
      frames: [{ speaker: "aa", opus: new Uint8Array([3]) }],
      asked: { call: "458", status: "On Tilo: Grace" },
    });
    expect((await lane.said()).map((frame) => [...frame.opus])).toEqual([[3]]);
    expect(await lane.asked()).toEqual({ call: "458", status: "On Tilo: Grace" });
    expect(await lane.asked()).toBeNull();

    // An ask taken while asking keeps the frames for the next `said`.
    connection.pending.push({
      frames: [{ speaker: "aa", opus: new Uint8Array([4]) }],
      asked: { call: null, status: null },
    });
    expect(await lane.asked()).toEqual({ call: null, status: null });
    expect((await lane.said()).map((frame) => [...frame.opus])).toEqual([[4]]);

    await lane.carry(null);
    expect(connection.carried.at(-1)).toBeNull();
  });
});
