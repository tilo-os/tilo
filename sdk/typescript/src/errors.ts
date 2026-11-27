/** A refusal: what the API, the daemon or the vault answered instead of doing what was asked. */

import type { ErrorBody } from "./types/api-v1.ts";

/**
 * The call was refused, in the API's one refusal shape (`Error`): `code` is stable and typed,
 * what a program branches on (`not_permitted`, `key_revoked`, `surface_changed`, …), `message`
 * is for people, and `error` is the whole body, with `next_steps`, `retry_after_ms` and the
 * refused action's row where there are any. `status` is the HTTP status it came with, 0 for a
 * refusal of the daemon's own connection.
 */
export class Refused extends Error {
  /** The refusal as the API answered it. */
  readonly error: ErrorBody;
  /** The HTTP status it came with; 0 when it came over no HTTP. */
  readonly status: number;

  constructor(error: ErrorBody, status = 0) {
    super(error.message || error.code);
    this.name = "Refused";
    this.error = error;
    this.status = status;
  }

  /** What a program branches on: `not_found`, `not_permitted`, `key_revoked`, … */
  get code(): string {
    return this.error.code;
  }
}

/** The refusal `body` is, as a daemon's connection or an answer's `error` carries one. */
export function refusalOf(body: unknown, status = 0): Refused {
  const error = (body as { error?: Partial<ErrorBody> } | null)?.error;
  return new Refused(
    {
      ...error,
      code: typeof error?.code === "string" ? error.code : "failed",
      message: typeof error?.message === "string" ? error.message : "the call was refused",
    },
    status,
  );
}
