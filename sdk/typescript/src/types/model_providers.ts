/**
 * Types for Model Providers (`so.tilo.model-providers`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Root` is a `Resource` of the app's `model-providers.root`, made by its factory with the
 * resource's path (`model-providers/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** Model providers — `model-providers.root`. */
export interface Root extends tilo.Resource {
  listPresets(): Promise<void>;
}

/** The `model-providers.root` kind at `path`. */
export function root(ws: tilo.Workspace, path: string): Root {
  return resourceProxy(ws, path) as unknown as Root;
}

