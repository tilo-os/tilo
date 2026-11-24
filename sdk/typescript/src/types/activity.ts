/**
 * Types for Activity (`so.tilo.activity`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Window` is a `Resource` of the app's `tl.so.tilo.activity.Window`, made by its factory with the
 * resource's path (`activity/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.activity@b9adf3436f2f77fa";

/** Window — `tl.so.tilo.activity.Window`. */
export interface Window extends tilo.Resource {
  ukfxyqw3: string;  // place
}

/** The actions `Window` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WINDOW_ACTIONS = [] as const;

/** The `tl.so.tilo.activity.Window` kind at `path`. */
export function activityWindow(ws: tilo.Workspace, path: string): Window {
  return resourceProxy(ws, path, SURFACE, WINDOW_ACTIONS) as unknown as Window;
}

/** Activity — `tl.so.tilo.activity`. */
export interface Activity extends tilo.Resource {
}

/** The actions `Activity` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACTIVITY_ACTIONS = [] as const;

/** The `tl.so.tilo.activity` kind at `path`. */
export function activity(ws: tilo.Workspace, path: string): Activity {
  return resourceProxy(ws, path, SURFACE, ACTIVITY_ACTIONS) as unknown as Activity;
}

