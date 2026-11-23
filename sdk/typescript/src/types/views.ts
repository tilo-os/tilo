/**
 * Types for Views (`so.tilo.lang.views`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `WrittenView` is a `Resource` of the app's `tl.so.tilo.lang.views.WrittenView`, made by its factory with the
 * resource's path (`views/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.lang.views@0617cce2d7aa1574";

/** A kind of device a person chooses how things show on. One of "desktop", "tablet", "phone". */
export type Device = "desktop" | "tablet" | "phone";

/** The arguments of `keep`. */
export interface KeepArgs {
}

/** The arguments of `send`. */
export interface SendArgs {
  to: unknown;
}

/** The arguments of `discard`. */
export interface DiscardArgs {
  v: WrittenView;
}

/** The arguments of `make_default`. */
export interface MakeDefaultArgs {
  v: WrittenView;
  device?: Device;
}

/** WrittenView — `tl.so.tilo.lang.views.WrittenView`. */
export interface WrittenView extends tilo.Resource {
  "2fxqftu3": string;  // title
  fdxj35iu: string;  // question
  "3pj7zyxo": string;  // definition
  "3eioszed": string;  // author
  loizxjpf: boolean;  // kept
  bckibjl6: string;  // shows
  /** Keeps a view among the person's own. */
  keep(): Promise<void>;
  /** Runs `keep` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keepMany(items: KeepArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sends a view to a channel: anyone there can add it to their own views. */
  send(args: {
    to: unknown;
  }): Promise<void>;
  /** Runs `send` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sendMany(items: SendArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `WrittenView` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WRITTENVIEW_ACTIONS = ["keep", "keepMany", "send", "sendMany"] as const;

/** The `tl.so.tilo.lang.views.WrittenView` kind at `path`. */
export function writtenView(ws: tilo.Workspace, path: string): WrittenView {
  return resourceProxy(ws, path, SURFACE, WRITTENVIEW_ACTIONS) as unknown as WrittenView;
}

/** Default — `tl.so.tilo.lang.views.Default`. */
export interface Default extends tilo.Resource {
  "2atmuo4h": string;  // shows
  h6n4gvgn: Device;  // device
  "7rv6o4rt": WrittenView;  // chosen
}

/** The actions `Default` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DEFAULT_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.views.Default` kind at `path`. */
export function viewsDefault(ws: tilo.Workspace, path: string): Default {
  return resourceProxy(ws, path, SURFACE, DEFAULT_ACTIONS) as unknown as Default;
}

/** Views — `tl.so.tilo.lang.views.Views`. */
export interface Views extends tilo.Resource {
  bweal36x: Array<tilo.Entry<WrittenView>>;  // items
  "2tnozirr": Array<tilo.Entry<Default>>;  // defaults
  /** Records the view an agent answered with, under the name of whoever asked. It opens beside what the person pointed at; until they keep it, it is the agent's answer. */
  answer(args: {
    title: string;
    question: string;
    definition: string;
    shows?: string;
  }): Promise<WrittenView>;
  /** A view the person writes, kept among their own. */
  write(args: {
    title: string;
    definition: string;
    shows?: string;
  }): Promise<WrittenView>;
  /** Throws a view away, and with it how a type showed through it. */
  discard(args: {
    v: WrittenView;
  }): Promise<void>;
  /** Runs `discard` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  discardMany(items: DiscardArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a view how the person shows its type on a kind of device. */
  make_default(args: {
    v: WrittenView;
    device?: Device;
  }): Promise<void>;
  /** Runs `make_default` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  make_defaultMany(items: MakeDefaultArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds the view a message sent to the person's own views. Its author is who sent the message, as the log recorded them, never who adds it: what it does asks first unless the person trusts that sender. It is a copy: a later change the sender makes to theirs does not reach it. */
  add(args: {
    m: unknown;
  }): Promise<WrittenView>;
}

/** The actions `Views` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VIEWS_ACTIONS = ["answer", "write", "discard", "discardMany", "make_default", "make_defaultMany", "add"] as const;

/** The `tl.so.tilo.lang.views.Views` kind at `path`. */
export function views(ws: tilo.Workspace, path: string): Views {
  return resourceProxy(ws, path, SURFACE, VIEWS_ACTIONS) as unknown as Views;
}

/** Views — `tl.so.tilo.lang.views`. */
export interface LangViews extends tilo.Resource {
}

/** The actions `LangViews` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LANGVIEWS_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.views` kind at `path`. */
export function langViews(ws: tilo.Workspace, path: string): LangViews {
  return resourceProxy(ws, path, SURFACE, LANGVIEWS_ACTIONS) as unknown as LangViews;
}

