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
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** Internet speed test — `speed-test.results`. */
export interface Results extends tilo.Resource {
  readonly results: unknown;
  record(args: {
    ping: number;
    jitter: number;
    download: number;
    upload: number;
    server: string;
    device: string;
  }): Promise<tilo.Run | void>;
  clear(): Promise<tilo.Run | void>;
}

/** The `speed-test.results` kind at `path`. */
export function results(ws: tilo.Workspace, path: string): Results {
  return resourceProxy(ws, path) as unknown as Results;
}

