/** What the SDK raises: a refusal, an ask, or a call that broke on the way. */

import { intOf, recordOf } from "./models.ts";

/** The code of a call made through generated types for another surface. */
export const SURFACE_CHANGED = "surface_changed";
export const KEY_REVOKED = "key_revoked";
export const KEY_EXPIRED = "key_expired";
export const INVALID_TOKEN = "invalid_token";

/**
 * The host refused the call: `code` is the app's own word for why, `message`
 * its words for people, `nextSteps` the calls that would work, and
 * `retryAfterMs` how long it asked to wait first. `action` is the refused
 * action's row, with what it takes, when the call named one.
 */
export class Refused extends Error {
  readonly code: string;
  readonly nextSteps: string[];
  readonly retryAfterMs: number | null;
  readonly action: unknown;

  constructor(
    code: string,
    message = "",
    nextSteps: string[] = [],
    retryAfterMs: number | null = null,
    action: unknown = null,
  ) {
    super(message || code);
    this.name = "Refused";
    this.code = code;
    this.nextSteps = nextSteps;
    this.retryAfterMs = retryAfterMs;
    this.action = action;
  }
}

/**
 * The types a call was made through were written for another version of the
 * app's surface than the workspace publishes, so nothing ran: write them
 * again with `tilo sdk gen`.
 */
export class SurfaceChanged extends Refused {
  constructor(message: string, nextSteps: string[] = []) {
    super(SURFACE_CHANGED, message, nextSteps);
    this.name = "SurfaceChanged";
  }
}

/**
 * The call waits for the person to allow it. `url` is where they answer it
 * when the host names one; `wait()` follows the invocation until they decide
 * and answers what it finished with.
 */
export class AskPending extends Error {
  readonly url: string | null;
  readonly invocationId: string;
  readonly question: unknown;
  readonly #follow: (() => Promise<Record<string, unknown>>) | null;

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
  }

  /** Follow the invocation until the person decides, then its answer. */
  async wait(): Promise<unknown> {
    if (this.#follow === null) {
      throw new ProtocolError(
        "this ask cannot be waited on: it names no invocation to follow",
      );
    }
    return this.#follow();
  }
}

/** The call was malformed, or the answer was not the one the host promises. */
export class ProtocolError extends Error {
  /** A stable code, when the break has one (`invalid_token`, `key_revoked`). */
  readonly code: string | null;

  constructor(message: string, code: string | null = null) {
    super(message);
    this.name = "ProtocolError";
    this.code = code;
  }
}

/** The daemon or the server could not be reached, or its answer was cut short: worth asking again. */
export class Unreachable extends ProtocolError {
  constructor(message: string) {
    super(message);
    this.name = "Unreachable";
  }
}

/**
 * The host turned the key away: it reaches nothing. `code` says why, the
 * host's own word: `invalid_token` for a key it never issued (mistyped, or
 * from another host), `key_revoked` (`KeyRevoked`) and `key_expired`
 * (`KeyExpired`). `nextSteps` are what would work.
 */
export class KeyRefused extends ProtocolError {
  readonly nextSteps: string[];

  constructor(message: string, code = INVALID_TOKEN, nextSteps: string[] = []) {
    super(message, code);
    this.name = "KeyRefused";
    this.nextSteps = nextSteps;
  }
}

/**
 * The key was revoked: by its person, with the key that minted it, or when
 * the workspace it reached was deleted. `revokedAtMs` says when (Unix
 * milliseconds), when the host knows.
 */
export class KeyRevoked extends KeyRefused {
  readonly revokedAtMs: number | null;

  constructor(message = "this key was revoked", revokedAtMs: number | null = null, nextSteps: string[] = []) {
    super(message, KEY_REVOKED, nextSteps);
    this.name = "KeyRevoked";
    this.revokedAtMs = revokedAtMs;
  }
}

/**
 * The key expired at `expiredAtMs` (Unix milliseconds), when the host knows:
 * a minted key lasts at most 89 days, and a key its person gave an expiry
 * ends then.
 */
export class KeyExpired extends KeyRefused {
  readonly expiredAtMs: number | null;

  constructor(message = "this key expired", expiredAtMs: number | null = null, nextSteps: string[] = []) {
    super(message, KEY_EXPIRED, nextSteps);
    this.name = "KeyExpired";
    this.expiredAtMs = expiredAtMs;
  }
}

/**
 * The refusal a body was, from its one shape:
 * `{"error": {code, message, next_steps, retry_after_ms, action}}`.
 */
export function refusalOf(body: unknown): Error {
  const error = recordOf(body).error;
  if (error === null || typeof error !== "object" || Array.isArray(error)) {
    return new ProtocolError("the host refused the call and said nothing more");
  }
  const detail = error as Record<string, unknown>;
  const code = typeof detail.code === "string" ? detail.code : "failed";
  const message = typeof detail.message === "string" ? detail.message : code;
  const steps = Array.isArray(detail.next_steps) ? detail.next_steps.map(String) : [];
  switch (code) {
    case KEY_REVOKED:
      return new KeyRevoked(message, intOf(detail.revoked_at_ms), steps);
    case KEY_EXPIRED:
      return new KeyExpired(message, intOf(detail.expired_at_ms), steps);
    case INVALID_TOKEN:
      return new KeyRefused(message, INVALID_TOKEN, steps);
    case SURFACE_CHANGED:
      return new SurfaceChanged(message, steps);
    default:
      return new Refused(
        code,
        message,
        steps,
        intOf(detail.retry_after_ms),
        detail.action ?? null,
      );
  }
}

/** The refusal a `{error}` body is, for a key the host turned away. */
export function keyRefusalOf(
  detail: { code?: unknown; message?: unknown; next_steps?: unknown; revoked_at_ms?: unknown; expired_at_ms?: unknown },
  fallback: string,
): KeyRefused {
  const message = typeof detail.message === "string" && detail.message !== "" ? detail.message : fallback;
  const steps = Array.isArray(detail.next_steps) ? detail.next_steps.map(String) : [];
  switch (detail.code) {
    case KEY_REVOKED:
      return new KeyRevoked(message, intOf(detail.revoked_at_ms), steps);
    case KEY_EXPIRED:
      return new KeyExpired(message, intOf(detail.expired_at_ms), steps);
    default:
      return new KeyRefused(
        message,
        typeof detail.code === "string" ? detail.code : INVALID_TOKEN,
        steps,
      );
  }
}
