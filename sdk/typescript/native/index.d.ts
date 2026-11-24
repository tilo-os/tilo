/** The invocation group a call is a step of: what History lists it under. */
export interface Group {
  id: string;
  title: string;
}

/** The refusal body a call may answer instead of a result. */
export interface Refusal {
  error: {
    code?: string;
    message?: string;
    next_steps?: string[];
    retry_after_ms?: number;
    revoked_at_ms?: number;
    expired_at_ms?: number;
    action?: unknown;
  };
}

/** One call's answer: what it answered, or the refusal it was. */
export type CallAnswer = { answer: unknown } | { refused: Refusal };

/** A connection to this computer's daemon. */
export interface Daemon {
  /** The workspace this connection acts in when a call names none. */
  readonly workspace: string | null;
  /** Call `tool` with `args` in `workspace` (`null`: the one the daemon serves), in `group`. */
  call(
    workspace: string | null,
    tool: string,
    args: object,
    group: Group | null,
  ): Promise<CallAnswer>;
}

/**
 * Connect to the daemon's native endpoint: the connection the launcher handed an app it
 * started, else `socket`, else this computer's daemon. Throws when no daemon answers.
 */
export declare function connect(
  socket: string | null,
  workspace: string | null,
): Promise<Daemon>;
