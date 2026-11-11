/**
 * Types for On-Screen Keyboard (`so.tilo.osk`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Keyboard` is a `Resource` of the app's `osk.keyboard`, made by its factory with the
 * resource's path (`osk/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

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

/** The `osk.keyboard` kind at `path`. */
export function keyboard(ws: tilo.Workspace, path: string): Keyboard {
  return resourceProxy(ws, path) as unknown as Keyboard;
}

/** What the keyboard learned — `osk.learned`. */
export interface Learned extends tilo.Resource {
  readonly transitions: Record<string, Record<string, Record<string, number>>>;
  readonly kept: string[];
  readonly touch: Record<string, KeyOffset>;
  learn(args: {
    lesson: Lesson;
  }): Promise<tilo.Run | void>;
  forget(): Promise<tilo.Run | void>;
}

/** The `osk.learned` kind at `path`. */
export function learned(ws: tilo.Workspace, path: string): Learned {
  return resourceProxy(ws, path) as unknown as Learned;
}

