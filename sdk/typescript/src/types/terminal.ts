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
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.terminal@54ffe4e754ac58fe";

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

/** The arguments of `stop`. */
export interface StopArgs {
}

/** The arguments of `record-run`. */
export interface RecordRunArgs {
  command: string;
  cwd: string;
  started_ms: number;
  ended_ms: number;
  exit: number;
  truncated: boolean;
  tool: string;
  undoable: boolean;
  items: unknown[];
  run: string;
  output?: unknown;
  request?: string;
}

/** The arguments of `record-target`. */
export interface RecordTargetArgs {
  target: string;
  device?: string;
}

/** The arguments of `record-relay-session`. */
export interface RecordRelaySessionArgs {
  session?: string;
}

/** The arguments of `claim-pane`. */
export interface ClaimPaneArgs {
  pane: string;
  device?: string;
}

/** The arguments of `remember`. */
export interface RememberArgs {
  command: string;
  cwd: string;
  exit: number;
  ended_ms: number;
  actor: string;
  session: string;
  run: string;
  environment?: string;
}

/** The arguments of `used`. */
export interface UsedArgs {
  device: string;
  target: string;
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
  /** Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  stopMany(items: StopArgs[]): Promise<tilo.ActionAnswer[]>;
  recordRun(args: {
    command: string;
    cwd: string;
    started_ms: number;
    ended_ms: number;
    exit: number;
    truncated: boolean;
    tool: string;
    undoable: boolean;
    items: unknown[];
    run: string;
    output?: unknown;
    request?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `record-run` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordRunMany(items: RecordRunArgs[]): Promise<tilo.ActionAnswer[]>;
  recordTarget(args: {
    target: string;
    device?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `record-target` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordTargetMany(items: RecordTargetArgs[]): Promise<tilo.ActionAnswer[]>;
  recordRelaySession(args?: {
    session?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `record-relay-session` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordRelaySessionMany(items: RecordRelaySessionArgs[]): Promise<tilo.ActionAnswer[]>;
  claimPane(args: {
    pane: string;
    device?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `claim-pane` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  claimPaneMany(items: ClaimPaneArgs[]): Promise<tilo.ActionAnswer[]>;
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
  return resourceProxy(ws, path, SURFACE) as unknown as Session;
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
  /** Runs `remember` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  rememberMany(items: RememberArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `terminal.history` kind at `path`. */
export function terminalHistory(ws: tilo.Workspace, path: string): History {
  return resourceProxy(ws, path, SURFACE) as unknown as History;
}

/** Recent environments — `terminal.recent-environments`. */
export interface RecentEnvironments extends tilo.Resource {
  devices: Record<string, string>;
  used(args: {
    device: string;
    target: string;
  }): Promise<tilo.Run | void>;
  /** Runs `used` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  usedMany(items: UsedArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `terminal.recent-environments` kind at `path`. */
export function recentEnvironments(ws: tilo.Workspace, path: string): RecentEnvironments {
  return resourceProxy(ws, path, SURFACE) as unknown as RecentEnvironments;
}

