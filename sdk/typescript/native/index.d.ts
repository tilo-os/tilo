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
  /**
   * Make the web call `args` names with the key of the vault item it names, in `workspace`
   * (`null`: the one the daemon serves): `{answer}`, or the refusal. An app's held connection
   * has the vault add the key to the request; the secret never reaches the app.
   */
  webCall(workspace: string | null, args: Record<string, unknown>): Promise<CallAnswer>;
  /**
   * The secret of the vault item `item` in `workspace` (`null`: the one the daemon serves),
   * released to this app's process: `{answer: {secret}}`, or the refusal.
   */
  release(workspace: string | null, item: string): Promise<CallAnswer>;
  /**
   * The reference of the vault item tagged for `app` and `purpose` in `workspace` (`null`: the
   * one the daemon serves), found on this app's process: `{answer: {reference}}` or
   * `{answer: {}}`, or the refusal.
   */
  find(workspace: string | null, app: string, purpose: string): Promise<CallAnswer>;
}

/**
 * Connect to the daemon's native endpoint: the connection the launcher handed an app it
 * started, else `socket`, else this computer's daemon. Throws when no daemon answers.
 */
export declare function connect(
  socket: string | null,
  workspace: string | null,
): Promise<Daemon>;
