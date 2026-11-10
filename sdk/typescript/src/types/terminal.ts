/**
 * Types for Terminal (`so.tilo.terminal`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Session` is a `Resource` of the app's `terminal.session`, made by its factory with the
 * resource's path (`terminal/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

/** One of "error", "warning", "location", "url", "test-passed", "test-failed", "test-ignored", "change". */
export type ItemKind = "error" | "warning" | "location" | "url" | "test-passed" | "test-failed" | "test-ignored" | "change";

export interface Run {
  command: string;
  cwd: string;
  environment: string | null;
  actor: string;
  for_person: string | null;
  started_ms: number;
  ended_ms: number;
  exit: number;
  output: unknown;
  truncated: boolean;
  tool: string;
  undoable: boolean;
  items: RunItem[];
}

export interface RunItem {
  kind: ItemKind;
  text: string;
  path: string | null;
  line: number | null;
  column: number | null;
  code: string | null;
}

export interface RunRequest {
  command: string;
  requested_by: string;
  for_person: string | null;
  pane: string;
  agent: boolean;
}

export interface Remembered {
  command: string;
  cwd: string;
  environment: string | null;
  exit: number;
  ended_ms: number;
  actor: string;
  session: string;
  run: string;
}

/** Terminal — `terminal.session`. */
export interface Session extends tilo.Resource {
  readonly target: string | null;
  readonly relay_session: string | null;
  readonly pane: string | null;
  readonly device: string | null;
  readonly holder: string | null;
  readonly runs: Run[];
  run(args: {
    command: string;
  }): Promise<unknown | tilo.Run>;
  rerun(args: {
    runs: string;
  }): Promise<unknown | tilo.Run>;
  rerunTest(args: {
    runs: string;
    test: string;
  }): Promise<unknown | tilo.Run>;
  stop(): Promise<tilo.Run | void>;
  recordRun(args: {
    command: string;
    cwd: string;
    started_ms: number;
    ended_ms: number;
    exit: number;
    output_len: number;
    truncated: boolean;
    tool: string;
    undoable: boolean;
    items: unknown[];
    run: string;
    output?: string;
    request?: string;
  }): Promise<tilo.Run | void>;
  recordTarget(args: {
    target: string;
    device?: string;
  }): Promise<tilo.Run | void>;
  recordRelaySession(args?: {
    session?: string;
  }): Promise<tilo.Run | void>;
  claimPane(args: {
    pane: string;
    device?: string;
  }): Promise<tilo.Run | void>;
  inspectSession(): Promise<void>;
  readScreen(): Promise<void>;
  inspectEditor(): Promise<void>;
  copyVisible(): Promise<void>;
  copySelection(): Promise<void>;
  selectAll(): Promise<void>;
  historyTop(): Promise<void>;
  historyLive(): Promise<void>;
  openLink(args: {
    url: string;
  }): Promise<void>;
}

/** The `terminal.session` kind at `path`. */
export function session(ws: tilo.Workspace, path: string): Session {
  return resourceProxy(ws, path) as unknown as Session;
}

/** Command history — `terminal.history`. */
export interface History extends tilo.Resource {
  readonly runs: Remembered[];
  remember(args: {
    command: string;
    cwd: string;
    exit: number;
    ended_ms: number;
    actor: string;
    session: string;
    run: string;
    environment?: string;
  }): Promise<tilo.Run | void>;
}

/** The `terminal.history` kind at `path`. */
export function terminalHistory(ws: tilo.Workspace, path: string): History {
  return resourceProxy(ws, path) as unknown as History;
}

/** Recent environments — `terminal.recent-environments`. */
export interface RecentEnvironments extends tilo.Resource {
  devices: Record<string, string>;
  used(args: {
    device: string;
    target: string;
  }): Promise<tilo.Run | void>;
}

/** The `terminal.recent-environments` kind at `path`. */
export function recentEnvironments(ws: tilo.Workspace, path: string): RecentEnvironments {
  return resourceProxy(ws, path) as unknown as RecentEnvironments;
}

