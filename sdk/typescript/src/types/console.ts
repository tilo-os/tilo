/**
 * Types for Console (`so.tilo.console`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `View` is a `Resource` of the app's `console.view`, made by its factory with the
 * resource's path (`console/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** Agent runs shown — `console.view`. */
export interface View extends tilo.Resource {
  agent: unknown;
  // the app's `workspace` field is the SDK's own `workspace`.
  showEverything(): Promise<tilo.Run | void>;
}

/** The `console.view` kind at `path`. */
export function view(ws: tilo.Workspace, path: string): View {
  return resourceProxy(ws, path) as unknown as View;
}

