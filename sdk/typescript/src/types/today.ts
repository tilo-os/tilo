/**
 * Types for Today (`so.tilo.lang.today`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Today` is a `Resource` of the app's `tl.so.tilo.lang.today.Today`, made by its factory with the
 * resource's path (`today/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.lang.today@b4e754135acc9ac5";

/** Today — `tl.so.tilo.lang.today.Today`. */
export interface Today extends tilo.Resource {
}

/** The actions `Today` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TODAY_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.today.Today` kind at `path`. */
export function today(ws: tilo.Workspace, path: string): Today {
  return resourceProxy(ws, path, SURFACE, TODAY_ACTIONS) as unknown as Today;
}

/** Today — `tl.so.tilo.lang.today`. */
export interface LangToday extends tilo.Resource {
  /** Clears the finished tasks, when there are any, and files a note saying how many were done. Both changes are the Tasks and Notes apps' own, asked at their action APIs: what they did comes back as their resources' next readings. */
  morning_cleanup(args: {
    list: unknown;
    nb: unknown;
  }): Promise<number>;
}

/** The actions `LangToday` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LANGTODAY_ACTIONS = ["morning_cleanup"] as const;

/** The `tl.so.tilo.lang.today` kind at `path`. */
export function langToday(ws: tilo.Workspace, path: string): LangToday {
  return resourceProxy(ws, path, SURFACE, LANGTODAY_ACTIONS) as unknown as LangToday;
}

