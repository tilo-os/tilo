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

/** The code of an answer that ran nothing because the host is busy. */
export const RATE_LIMITED = "rate_limited";

/**
 * The host is busy with the person's changes and ran nothing. The client
 * asks again by itself, waiting what the host says (`retryAfterMs`), until
 * its retry deadline; this is thrown only once the deadline passed.
 */
export class RateLimited extends Refused {
  readonly retryAfterMs: number | null;

  constructor(message = "", nextSteps: string[] = [], retryAfterMs: number | null = null) {
    super(RATE_LIMITED, message, nextSteps);
    this.name = "RateLimited";
    this.retryAfterMs = retryAfterMs;
  }
}

/**
 * The call waits for the person to allow it. `url` is where they answer it
 * when the door names one; `wait()` follows the invocation until they
 * decide and answers what it finished with (a `Key` for a key that was
 * created).
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

/**
 * The door turned the key away (HTTP 401): it reaches nothing. `code` says
 * why, the door's own word: `invalid_token` for a key the door never issued
 * (mistyped, or from another door), `key_revoked` (`KeyRevoked`) and
 * `key_expired` (`KeyExpired`). `nextSteps` are what would work.
 */
export class KeyRefused extends ProtocolError {
  readonly nextSteps: string[];

  constructor(message: string, code = INVALID_TOKEN, status: number | null = 401, nextSteps: string[] = []) {
    super(message, status, code);
    this.name = "KeyRefused";
    this.nextSteps = nextSteps;
  }
}

/**
 * The key was revoked: by its person, with the key that minted it, or when
 * the workspace it reached was deleted. `revokedAtMs` says when (Unix
 * milliseconds), when the door knows.
 */
export class KeyRevoked extends KeyRefused {
  readonly revokedAtMs: number | null;

  constructor(message = "this key was revoked", revokedAtMs: number | null = null, status: number | null = 401, nextSteps: string[] = []) {
    super(message, KEY_REVOKED, status, nextSteps);
    this.name = "KeyRevoked";
    this.revokedAtMs = revokedAtMs;
  }
}

/**
 * The key expired at `expiredAtMs` (Unix milliseconds), when the door knows:
 * a minted key lasts at most 90 days, and a key its person gave an expiry
 * ends then.
 */
export class KeyExpired extends KeyRefused {
  readonly expiredAtMs: number | null;

  constructor(message = "this key expired", expiredAtMs: number | null = null, status: number | null = 401, nextSteps: string[] = []) {
    super(message, KEY_EXPIRED, status, nextSteps);
    this.name = "KeyExpired";
    this.expiredAtMs = expiredAtMs;
  }
}

export const KEY_REVOKED = "key_revoked";
export const KEY_EXPIRED = "key_expired";
export const INVALID_TOKEN = "invalid_token";

/**
 * The `KeyRefused` the door's typed refusal `error` (`{code, message,
 * next_steps, revoked_at_ms?, expired_at_ms?}`) is, if it is about the key.
 */
export function keyRefusal(error: unknown, status: number | null = 401): KeyRefused | null {
  if (!error || typeof error !== "object" || Array.isArray(error)) {
    return null;
  }
  const detail = error as Record<string, unknown>;
  const code = detail.code;
  const message = String(detail.message ?? code ?? "");
  const steps = Array.isArray(detail.next_steps) ? detail.next_steps.map(String) : [];
  const ms = (value: unknown) => (typeof value === "number" && value >= 0 ? value : null);
  if (code === KEY_REVOKED) {
    return new KeyRevoked(message, ms(detail.revoked_at_ms), status, steps);
  }
  if (code === KEY_EXPIRED) {
    return new KeyExpired(message, ms(detail.expired_at_ms), status, steps);
  }
  if (code === INVALID_TOKEN) {
    return new KeyRefused(message, INVALID_TOKEN, status, steps);
  }
  return null;
}
