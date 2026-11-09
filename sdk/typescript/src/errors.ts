/** What the SDK raises: a refusal, an ask, or a call that broke the wire. */

/**
 * The host refused the call: `code` is the app's own, `message` its words
 * for people, and `nextSteps` the calls that would work.
 */
export class Refused extends Error {
  readonly code: string;
  readonly nextSteps: string[];

  constructor(code: string, message = "", nextSteps: string[] = []) {
    super(message || code);
    this.name = "Refused";
    this.code = code;
    this.nextSteps = nextSteps;
  }
}

/**
 * The call waits for the person to allow it. `url` is where they answer it
 * when the door names one; `wait()` follows the invocation until they
 * decide and answers its result (a `Key` for a key that was created).
 */
export class AskPending extends Error {
  readonly url: string | null;
  readonly invocationId: string;
  readonly question: unknown;
  #follow: (() => Promise<Record<string, unknown>>) | null;
  #convert: ((answer: Record<string, unknown>) => unknown) | null;

  constructor(
    url: string | null,
    invocationId: string,
    question: unknown = null,
    follow: (() => Promise<Record<string, unknown>>) | null = null,
  ) {
    super(
      `waiting for the person${invocationId ? `: invocation ${invocationId}` : ""}${url ? ` at ${url}` : ""}`,
    );
    this.name = "AskPending";
    this.url = url;
    this.invocationId = invocationId;
    this.question = question;
    this.#follow = follow;
    this.#convert = null;
  }

  /** How `wait` reads the answer, when its result is not the whole of it. */
  then(convert: (answer: Record<string, unknown>) => unknown): this {
    this.#convert = convert;
    return this;
  }

  async wait(): Promise<unknown> {
    if (this.#follow === null) {
      throw new ProtocolError("this ask cannot be waited on: its invocation has no session");
    }
    const answer = await this.#follow();
    return this.#convert === null ? answer : this.#convert(answer);
  }
}

/** The call was malformed, or the door answered something else. */
export class ProtocolError extends Error {
  readonly status: number | null;
  readonly code: number | string | null;

  constructor(message: string, status: number | null = null, code: number | string | null = null) {
    super(message);
    this.name = "ProtocolError";
    this.status = status;
    this.code = code;
  }
}
