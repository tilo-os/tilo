/**
 * Types for Speed Test (`so.tilo.speed-test`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Results` is a `Resource` of the app's `speed-test.results`, made by its factory with the
 * resource's path (`speed-test/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.speed-test@303db65a9e1defad";

export interface Measurement {
  at_ms: number;
  ping: number;
  jitter: number;
  download: number;
  upload: number;
  server: string;
  device: string;
}

/** The arguments of `record`. */
export interface RecordArgs {
  ping: number;
  jitter: number;
  download: number;
  upload: number;
  server: string;
  device: string;
}

/** The arguments of `clear`. */
export interface ClearArgs {
}

/** Internet speed test — `speed-test.results`. */
export interface Results extends tilo.Resource {
  readonly results: Array<tilo.Entry<Measurement>>;
  record(args: {
    ping: number;
    jitter: number;
    download: number;
    upload: number;
    server: string;
    device: string;
  }): Promise<void>;
  /** Runs `record` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordMany(items: RecordArgs[]): Promise<tilo.ActionAnswer[]>;
  clear(): Promise<void>;
  /** Runs `clear` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  clearMany(items: ClearArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Results` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RESULTS_ACTIONS = ["record", "recordMany", "clear", "clearMany"] as const;

/** The `speed-test.results` kind at `path`. */
export function results(ws: tilo.Workspace, path: string): Results {
  return resourceProxy(ws, path, SURFACE, RESULTS_ACTIONS) as unknown as Results;
}

