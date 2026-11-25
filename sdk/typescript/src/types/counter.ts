/**
 * Types for Counter (`so.tilo.lang.counter`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`counter/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.lang.counter@ce1645e68d884a2d";

/** The arguments of `increment`. */
export interface IncrementArgs {
}

/** The arguments of `decrement`. */
export interface DecrementArgs {
}

/** Notice — `tl.tilo.host.Notice`. */
export interface Notice extends tilo.Resource {
  notice_to: string;  // to
  notice_title: string;  // title
  notice_body: string;  // body
  notice_opens: unknown;  // opens
  notice_seen: boolean;  // seen
  notice_device: string;  // device
  notice_actions: string[];  // actions
}

/** The actions `Notice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTICE_ACTIONS = [] as const;

/** The `tl.tilo.host.Notice` kind at `path`. */
export function notice(ws: tilo.Workspace, path: string): Notice {
  return resourceProxy(ws, path, SURFACE, NOTICE_ACTIONS) as unknown as Notice;
}

/** Counter — `tl.so.tilo.lang.counter.Counter`. */
export interface Counter extends tilo.Resource {
  "3wkdueca": number;  // value
  /** Add one to the counter. */
  increment(): Promise<void>;
  /** Runs `increment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  incrementMany(items: IncrementArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Take one away from the counter. */
  decrement(): Promise<void>;
  /** Runs `decrement` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  decrementMany(items: DecrementArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Counter` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COUNTER_ACTIONS = ["increment", "incrementMany", "decrement", "decrementMany"] as const;

/** The `tl.so.tilo.lang.counter.Counter` kind at `path`. */
export function counter(ws: tilo.Workspace, path: string): Counter {
  return resourceProxy(ws, path, SURFACE, COUNTER_ACTIONS) as unknown as Counter;
}

/** Counter — `tl.so.tilo.lang.counter`. */
export interface LangCounter extends tilo.Resource {
}

/** The actions `LangCounter` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LANGCOUNTER_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.counter` kind at `path`. */
export function langCounter(ws: tilo.Workspace, path: string): LangCounter {
  return resourceProxy(ws, path, SURFACE, LANGCOUNTER_ACTIONS) as unknown as LangCounter;
}

