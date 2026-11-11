/**
 * Types for Print Centre (`so.tilo.print-centre`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Defaults` is a `Resource` of the app's `print-centre.defaults`, made by its factory with the
 * resource's path (`print-centre/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** Print Centre — `print-centre.defaults`. */
export interface Defaults extends tilo.Resource {
  two_sided: boolean;
  scan_resolution: number;
  scan_color: string;
  scan_source: string;
  ocr_language: string;
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
}

/** The `print-centre.defaults` kind at `path`. */
export function defaults(ws: tilo.Workspace, path: string): Defaults {
  return resourceProxy(ws, path) as unknown as Defaults;
}

