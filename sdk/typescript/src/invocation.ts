/** An invocation: an action that has not finished, and what to do with it. */

import type { Endpoint } from "./endpoint.ts";
import { type ActionAnswer, type Event, resultOf } from "./models.ts";
import { ProtocolError } from "./errors.ts";

/**
 * An action still running, or one that waits for the person.
 *
 * `result()` follows it to its result, `output()` follows its events,
 * `cancel()` stops it and `undo()` takes back what it changed. Once it is
 * done, `link` is the link to the record it changed, when it changed one.
 */
export class Invocation<Result = unknown> {
  readonly endpoint: Endpoint;
  readonly invocationId: string;
  path: string | null;
  link: string | null = null;
  links: string[] = [];

  constructor(endpoint: Endpoint, invocationId: string, path: string | null = null) {
    this.endpoint = endpoint;
    this.invocationId = invocationId;
    this.path = path;
  }

  toString(): string {
    return `Invocation(${this.invocationId})`;
  }

  /** The resource whose action the invocation is, `app/resource` without the action. */
  get resource(): string | null {
    if (this.path === null || !this.path.includes("#")) {
      return null;
    }
    return this.path.split("#")[0] ?? null;
  }

  /** The invocation's whole answer once it is no longer pending. */
  async answer(timeoutMs: number | null = null): Promise<ActionAnswer> {
    const answer = await this.endpoint.wait(this.invocationId, timeoutMs);
    this.note(answer);
    return answer;
  }

  /** The action's result; a refusal the host made throws `Refused`. */
  async result(timeoutMs: number | null = null): Promise<Result> {
    return resultOf(await this.answer(timeoutMs)) as Result;
  }

  /** Stop the invocation. */
  async cancel(): Promise<ActionAnswer> {
    const answer = await this.endpoint.cancel(this.invocationId);
    this.note(answer);
    return answer;
  }

  /** The invocation's events, from started to succeeded, failed or cancelled. */
  output(): AsyncGenerator<Event> {
    return this.endpoint.output(this.invocationId);
  }

  /** Take the invocation back: its app's one step. */
  async undo(): Promise<ActionAnswer> {
    if (this.path === null) {
      await this.answer();
    }
    if (this.resource === null) {
      throw new ProtocolError(
        "this invocation names no resource, so there is no app step to undo",
      );
    }
    return this.endpoint.undo(this.invocationId);
  }

  /** What the invocation's answer says about where it reached. */
  private note(answer: ActionAnswer): void {
    if (answer.path !== null && answer.path !== "") {
      this.path = answer.path;
    }
    this.links = answer.links;
    if (answer.link !== null) {
      this.link = answer.link;
    }
  }
}
