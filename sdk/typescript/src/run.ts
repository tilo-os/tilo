/** A run: an invocation that has not finished, and what to do with it. */

import type { Endpoint } from "./endpoint.ts";
import { resultOf, type Answer } from "./wire.ts";
import type { Event } from "./models.ts";
import { ProtocolError } from "./errors.ts";

/**
 * An action still running, or one that waits for the person.
 *
 * `result()` follows it to its result, `output()` follows its events,
 * `cancel()` stops it and `undo()` takes back what it changed. Once it is
 * done, `link` is the link to the record it changed, when it changed one.
 */
export class Run<Result = unknown> {
  readonly endpoint: Endpoint;
  readonly invocationId: string;
  path: string | null;
  link: string | null = null;

  constructor(endpoint: Endpoint, invocationId: string, path: string | null = null) {
    this.endpoint = endpoint;
    this.invocationId = invocationId;
    this.path = path;
  }

  /** The resource whose action the run is, `app/name` without the action. */
  get resource(): string | null {
    if (this.path === null || !this.path.includes("#")) {
      return null;
    }
    return this.path.split("#")[0] ?? null;
  }

  /** The whole typed answer once the invocation is done. */
  async answer(timeoutMs: number | null = null): Promise<Answer> {
    const answer = await this.endpoint.wait(this.invocationId, timeoutMs);
    const path = answer.path;
    if (typeof path === "string" && path !== "") {
      this.path = path;
    }
    if (typeof answer.link === "string" && answer.link !== "") {
      this.link = answer.link;
    }
    return answer;
  }

  /** The action's result; a refusal the host made throws `Refused`. */
  async result(timeoutMs: number | null = null): Promise<Result> {
    const answer = await this.answer(timeoutMs);
    return resultOf(answer) as Result;
  }

  /** Stop the invocation. */
  cancel(): Promise<Answer> {
    return this.endpoint.status(this.invocationId, true);
  }

  /** The run's events, from started to succeeded, failed or cancelled. */
  async *output(): AsyncGenerator<Event> {
    for await (const event of this.endpoint.watch()) {
      if (event.kind !== "run" || event.invocationId !== this.invocationId) {
        continue;
      }
      yield event;
      if (event.phase === "succeeded" || event.phase === "failed" || event.phase === "cancelled") {
        return;
      }
    }
  }

  /** Take the run back: its app's one step, or the whole task. */
  async undo(): Promise<unknown> {
    if (this.path === null) {
      await this.answer();
    }
    const resource = this.resource;
    if (resource === null) {
      throw new ProtocolError("this run names no resource, so there is no app step to undo");
    }
    return this.endpoint.outcome(`${resource}#undo`, { steps: 1 });
  }
}
