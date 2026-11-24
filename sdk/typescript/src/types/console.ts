/**
 * Types for Console (`so.tilo.console`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Window` is a `Resource` of the app's `tl.so.tilo.console.Window`, made by its factory with the
 * resource's path (`console/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers an `Invocation`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one invocation group.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.console@5e0e5f40d75f4d45";

/** Window — `tl.so.tilo.console.Window`. */
export interface Window extends tilo.Resource {
  e6xn3nds: string;  // place
}

/** The actions `Window` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WINDOW_ACTIONS = [] as const;

/** The `tl.so.tilo.console.Window` kind at `path`. */
export function consoleWindow(ws: tilo.Workspace, path: string): Window {
  return resourceProxy(ws, path, SURFACE, WINDOW_ACTIONS) as unknown as Window;
}

/** Console — `tl.so.tilo.console`. */
export interface Console extends tilo.Resource {
}

/** The actions `Console` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONSOLE_ACTIONS = [] as const;

/** The `tl.so.tilo.console` kind at `path`. */
export function console(ws: tilo.Workspace, path: string): Console {
  return resourceProxy(ws, path, SURFACE, CONSOLE_ACTIONS) as unknown as Console;
}

