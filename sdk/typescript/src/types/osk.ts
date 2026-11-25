/**
 * Types for On-Screen Keyboard (`so.tilo.osk`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Keyboard` is a `Resource` of the app's `osk.keyboard`, made by its factory with the
 * resource's path (`osk/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.osk@05e8fcea8c336e29";

export interface KeyOffset {
  dx: number;
  dy: number;
  samples: number;
}

export interface Lesson {
  language: string;
  transitions: unknown[];
  kept: string[];
  touch: unknown[];
}

/** The arguments of `learn`. */
export interface LearnArgs {
  lesson: Lesson;
}

/** The arguments of `forget`. */
export interface ForgetArgs {
}

/** On-screen keyboard — `osk.keyboard`. */
export interface Keyboard extends tilo.Resource {
  setLayout(args: {
    value: string;
  }): Promise<void>;
  setSize(args: {
    value: string;
  }): Promise<void>;
  setSuggestions(args: {
    value: boolean;
  }): Promise<void>;
  setAutocorrect(args: {
    value: boolean;
  }): Promise<void>;
  setTypingLanguage(args: {
    value: string;
  }): Promise<void>;
  setPersonalization(args: {
    value: boolean;
  }): Promise<void>;
  setEmojiRecents(args: {
    value: string[];
  }): Promise<void>;
}

/** The actions `Keyboard` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const KEYBOARD_ACTIONS = ["setLayout", "setSize", "setSuggestions", "setAutocorrect", "setTypingLanguage", "setPersonalization", "setEmojiRecents"] as const;

/** The `osk.keyboard` kind at `path`. */
export function keyboard(ws: tilo.Workspace, path: string): Keyboard {
  return resourceProxy(ws, path, SURFACE, KEYBOARD_ACTIONS) as unknown as Keyboard;
}

/** What the keyboard learned — `osk.learned`. */
export interface Learned extends tilo.Resource {
  readonly transitions: Record<string, Record<string, Record<string, number>>>;
  readonly kept: string[];
  readonly touch: Record<string, KeyOffset>;
  readonly restored_previous: boolean;
  learn(args: {
    lesson: Lesson;
  }): Promise<void>;
  /** Runs `learn` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  learnMany(items: LearnArgs[]): Promise<tilo.ActionAnswer[]>;
  forget(): Promise<void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Learned` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LEARNED_ACTIONS = ["learn", "learnMany", "forget", "forgetMany"] as const;

/** The `osk.learned` kind at `path`. */
export function learned(ws: tilo.Workspace, path: string): Learned {
  return resourceProxy(ws, path, SURFACE, LEARNED_ACTIONS) as unknown as Learned;
}

