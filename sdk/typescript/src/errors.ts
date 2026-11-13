/** What the SDK raises: a refusal, an ask, or a call that broke on the way. */

/** The code of an answer that ran nothing because the host is busy. */
export const RATE_LIMITED = "rate_limited";
/** The code of a call made through generated types for another surface. */
export const SURFACE_CHANGED = "surface_changed";
export const KEY_REVOKED = "key_revoked";
export const KEY_EXPIRED = "key_expired";
export const INVALID_TOKEN = "invalid_token";

/**
 * The host refused the call: `code` is the app's or the API's own, `message`
 * its words for people, and `nextSteps` the calls that would work.
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
 * The types a call was made through were written for another version of the
 * app's surface than the workspace publishes, so nothing ran: write them
 * again with `tilo sdk gen`.
 */
export class SurfaceChanged extends Refused {
  constructor(message: string, nextSteps: string[] = []) {
    super(
      SURFACE_CHANGED,
      `${message}; run \`tilo sdk gen\` to write these types again`,
      nextSteps,
    );
    this.name = "SurfaceChanged";
  }
}

/**
 * The call waits for the person to allow it. `url` is where they answer it
 * when the host names one; `wait()` follows the run until they decide and
 * answers what it finished with (a `Key` for a key that was created).
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
      `waiting for the person${invocationId ? `: run ${invocationId}` : ""}${url ? ` at ${url}` : ""}`,
    );
    this.name = "AskPending";
    this.url = url;
    this.invocationId = invocationId;
    this.question = question;
    this.#follow = follow;
  }

  /** Follow the run until the person decides, then its answer. */
  async wait(): Promise<unknown> {
    if (this.#follow === null) {
      throw new ProtocolError("this ask cannot be waited on: it names no run to follow");
    }
    return this.#follow();
  }
}

/** The call was malformed, or the answer was not the one the API promises. */
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

/** The API server could not be reached, or its answer was cut short: worth asking again. */
export class NetworkError extends ProtocolError {
  constructor(message: string) {
    super(message);
    this.name = "NetworkError";
  }
}

/**
 * The host turned the key away (HTTP 401): it reaches nothing. `code` says
 * why, the API's own word: `invalid_token` for a key the API server never
 * issued (mistyped, or from another host), `key_revoked` (`KeyRevoked`) and
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
 * milliseconds), when the host knows.
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
 * The key expired at `expiredAtMs` (Unix milliseconds), when the host knows:
 * a minted key lasts at most 89 days, and a key its person gave an expiry
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

/** The `error` body every refusal of the API carries. */
export interface RefusalBody {
  code?: unknown;
  message?: unknown;
  next_steps?: unknown;
  retry_after_ms?: unknown;
  revoked_at_ms?: unknown;
  expired_at_ms?: unknown;
}

/**
 * The refusal an answer of `status` was, from its one JSON shape:
 * `{"error": {code, message, next_steps, retry_after_ms, …}}`.
 */
export function refusalOf(status: number, body: Record<string, unknown> | null): Error {
  const error = body?.error;
  if (!error || typeof error !== "object" || Array.isArray(error)) {
    return new ProtocolError(`the API answered ${status}`, status);
  }
  const detail = error as RefusalBody;
  const code = typeof detail.code === "string" ? detail.code : "failed";
  const message = typeof detail.message === "string" ? detail.message : code;
  const steps = Array.isArray(detail.next_steps) ? detail.next_steps.map(String) : [];
  switch (code) {
    case KEY_REVOKED:
      return new KeyRevoked(message, intOf(detail.revoked_at_ms), status, steps);
    case KEY_EXPIRED:
      return new KeyExpired(message, intOf(detail.expired_at_ms), status, steps);
    case INVALID_TOKEN:
      return new KeyRefused(message, INVALID_TOKEN, status, steps);
    case RATE_LIMITED:
      return new RateLimited(message, steps, intOf(detail.retry_after_ms));
    case SURFACE_CHANGED:
      return new SurfaceChanged(message, steps);
    default:
      return new Refused(code, message, steps);
  }
}

function intOf(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
}
