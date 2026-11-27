/** The refusal body a vault or call lane call may answer instead of a result. */
export interface Refusal {
  error: {
    code?: string;
    message?: string;
    next_steps?: string[];
    retry_after_ms?: number;
    action?: unknown;
  };
}

/** One call's answer: what it answered, or the refusal it was. */
export type CallAnswer = { answer: unknown } | { refused: Refusal };

/** What the daemon's API answered one request with. */
export interface Reply {
  readonly status: number;
  /** Each header as `[name, value]`. */
  readonly headers: [string, string][];
  /** The next piece of the body; `null` once it ended. Rejects when the connection closed first. */
  next(): Promise<Buffer | null>;
}

/** A connection to this computer's daemon. */
export interface Daemon {
  /** The workspace this connection acts in when a call names none. */
  readonly workspace: string | null;
  /**
   * Have the daemon answer `method` on `target` (a `/v1` path with its query) with `headers` and
   * `body`, as its API answers this connection: an app with its own grants, never its person's.
   * Rejects when the daemon did not answer at all; a refusal is a reply with its status.
   */
  request(
    method: string,
    target: string,
    headers: [string, string][],
    body: Buffer | null,
  ): Promise<Reply>;
  /**
   * Make the web call `args` names with the key of the vault item it names, in `workspace`
   * (`null`: the one the daemon serves): `{answer}`, or the refusal. An app's own connection
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
  /**
   * Carry the outside call `pair` names into the workspace call it names, or let go of the one
   * carried (`null`), on an app's connection: `{answer: {}}`, or the refusal.
   */
  carryCall(pair: { outside: string; tilo: string } | null): Promise<CallAnswer>;
  /** Say who is in the outside calls and where the app stands in them: `{answer: {}}`, or the
   * refusal. */
  reportCall(report: object): Promise<CallAnswer>;
  /** Hand the workspace's call one Opus frame `speaker` said: `null`, or `{refused}`. */
  hearCall(speaker: string, opus: Uint8Array): { refused: Refusal } | null;
  /**
   * The frames the workspace's call said since the last time, and where the device asks the app
   * to be, when that is new. Throws the refusal's body, as JSON, on a connection that is no app's.
   */
  saidCall(): {
    frames: { speaker: string; opus: Buffer; skipped: number }[];
    asked?: { call?: string | null; status?: string | null } | null;
  };
}

/**
 * Connect to the daemon's native endpoint: the connection the launcher handed an app it
 * started, else `socket`, else this computer's daemon. Throws when no daemon answers.
 */
export declare function connect(
  socket: string | null,
  workspace: string | null,
): Promise<Daemon>;
