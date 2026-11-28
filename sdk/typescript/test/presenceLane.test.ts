/** A workspace's presence lane: an app's own daemon connection carries one, a key's client none. */

import { describe, expect, it } from "bun:test";

import { type DaemonConnection, type PresenceSaid, Refused, Tilo } from "../src/index.ts";

const WORKSPACE = "6f9619ff-8b86-d011-b42d-00c04fc964ff";

/**
 * An app's connection, as the daemon answers its presence: the words it was told, and who the
 * device read composing in each resource. A word the device refuses answers its refusal, and a
 * connection that is no app's throws one from its reading, as the addon does.
 */
class AppConnection implements DaemonConnection {
  readonly workspace = WORKSPACE;
  readonly said: PresenceSaid[] = [];
  readonly asked: string[] = [];
  composers: Record<string, Record<string, unknown>[]> = {};
  refusing: string | null = null;

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
  async carryCall(): Promise<never> {
    throw new Error("the presence lane carried a call");
  }
  async reportCall(): Promise<never> {
    throw new Error("the presence lane reported a call");
  }
  hearCall(): never {
    throw new Error("the presence lane handed a call frame");
  }
  saidCall(): never {
    throw new Error("the presence lane read a call");
  }
  async sayPresence(said: PresenceSaid) {
    if (this.refusing !== null) {
      return { refused: { error: { code: "not_permitted", message: this.refusing } } };
    }
    this.said.push(said);
    return { answer: {} };
  }
  composingAt(id: string) {
    this.asked.push(id);
    if (this.refusing !== null) {
      throw new Error(JSON.stringify({ code: "not_permitted", message: this.refusing }));
    }
    return this.composers[id] ?? [];
  }
}

describe("a presence lane", () => {
  it("is refused to a key's client", async () => {
    const lane = new Tilo({ key: "tilo_sk_test", url: "https://api.test" }).workspace(WORKSPACE).presenceLane;
    for (const attempt of [
      () => lane.say({ key: { Workspace: { id: WORKSPACE } }, payload: { Composing: { composing: true } } }),
      () => lane.composing("c1"),
    ]) {
      const refused = await attempt().catch((error: unknown) => error);
      expect(refused).toBeInstanceOf(Refused);
      expect((refused as Refused).code).toBe("not_permitted");
    }
  });

  it("reads who composes in a resource by its published address", async () => {
    const connection = new AppConnection();
    const lane = new Tilo({ daemon: connection }).workspace().presenceLane;
    connection.composers = { "tl:so.tilo.chat:c1": [{ person: "ada" }] };

    expect(await lane.composing("tl:so.tilo.chat:c1")).toEqual([{ person: "ada" }]);
    expect(await lane.composing("tl:so.tilo.chat:c2")).toEqual([]);
    expect(connection.asked).toEqual(["tl:so.tilo.chat:c1", "tl:so.tilo.chat:c2"]);
  });

  it("says the app's words, and raises what the device refused", async () => {
    const connection = new AppConnection();
    const lane = new Tilo({ daemon: connection }).workspace().presenceLane;
    const word: PresenceSaid = {
      key: { Resource: { app: "so.tilo.chat", id: "c1", kind: "TextChannel" } },
      payload: { Composing: { composing: true } },
      persona: { service: "discord", account: "3001", name: "Dana" },
    };
    await lane.say(word);
    expect(connection.said).toEqual([word]);

    connection.refusing = "the app's process ended";
    const said = await lane.say(word).catch((error: unknown) => error);
    expect(said).toBeInstanceOf(Refused);
    expect((said as Refused).message).toContain("the app's process ended");
    const read = await lane.composing("c1").catch((error: unknown) => error);
    expect(read).toBeInstanceOf(Refused);
    expect((read as Refused).code).toBe("not_permitted");
  });
});
