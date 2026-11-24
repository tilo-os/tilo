/**
 * Types for Slides (`so.tilo.slides`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Deck` is a `Resource` of the app's `slides.deck`, made by its factory with the
 * resource's path (`slides/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.slides@6a033983332a1bd4";

/** One of "surface", "accent", "deep". */
export type Background = "surface" | "accent" | "deep";

/** One of Bool, Int, Str: an object whose case the app tags it with. */
export type ClipAttr = Record<string, unknown>;

export interface ClipBlock {
  kind: string;
  text: string;
  marks: MarkSpan[];
  level: number | null;
  ordered: boolean | null;
  align: string | null;
  attrs: Record<string, ClipAttr>;
}

/** One of "light", "dark". */
export type DeckLook = "light" | "dark";

export interface Element {
  kind: ElementKind;
  x: number;
  y: number;
  w: number;
  h: number;
  body: string;
  src: string;
  fill: string;
  stroke: string;
  stroke_width: number;
  font_size: number;
  align: TextAlign;
  bold: boolean;
  italic: boolean;
  radius: number;
}

export interface ElementData {
  id: string;
  kind: ElementKind;
  x: number;
  y: number;
  w: number;
  h: number;
  blocks: ClipBlock[];
  src: string;
  fill: string;
  stroke: string;
  stroke_width: number;
  font_size: number;
  align: TextAlign;
  bold: boolean;
  italic: boolean;
  radius: number;
}

/** One of "text", "rect", "ellipse", "line", "image". */
export type ElementKind = "text" | "rect" | "ellipse" | "line" | "image";

/** One of "title", "title_content", "two_column", "section", "blank". */
export type Layout = "title" | "title_content" | "two_column" | "section" | "blank";

export interface MarkSpan {
  start: number;
  end: number;
  name: string;
  value: MarkValue;
}

/** One of Bool, Int, Str: an object whose case the app tags it with. */
export type MarkValue = Record<string, unknown>;

export interface NewElement {
  kind: ElementKind;
  x: number;
  y: number;
  w: number;
  h: number;
  text: string | null;
  src: string | null;
  fill: string | null;
  text_color: string | null;
  stroke: string | null;
  stroke_width: number | null;
  font_size: number | null;
  align: TextAlign | null;
  bold: boolean | null;
  italic: boolean | null;
  radius: number | null;
}

export interface OutlinedSlide {
  title: string | null;
  body: string | null;
  layout: Layout | null;
  notes: string | null;
}

/** One of "indigo", "emerald", "amber", "rose", "sky". */
export type PaletteKind = "indigo" | "emerald" | "amber" | "rose" | "sky";

export interface Slide {
  layout: Layout;
  background: Background;
  notes: string;
  elements: Array<tilo.Entry<Element>>;
}

export interface SlideData {
  id: string;
  layout: Layout;
  background: Background;
  notes: string;
  elements: ElementData[];
}

/** One of "left", "center", "right", "justify". */
export type TextAlign = "left" | "center" | "right" | "justify";

/** The arguments of `insert-slide`. */
export interface InsertSlideArgs {
  at: number;
  slide: SlideData;
}

/** The arguments of `duplicate-slide`. */
export interface DuplicateSlideArgs {
  slide: string;
}

/** The arguments of `delete-slide`. */
export interface DeleteSlideArgs {
  slide: string;
}

/** The arguments of `insert-element`. */
export interface InsertElementArgs {
  slide: string;
  element: ElementData;
}

/** The arguments of `delete-element`. */
export interface DeleteElementArgs {
  slide: string;
  element: string;
}

/** The arguments of `bring-to-front`. */
export interface BringToFrontArgs {
  slide: string;
  element: string;
}

/** The arguments of `send-to-back`. */
export interface SendToBackArgs {
  slide: string;
  element: string;
}

/** The arguments of `replace-deck`. */
export interface ReplaceDeckArgs {
  title: string;
  palette: PaletteKind;
  slides: SlideData[];
  look?: DeckLook;
}

/** Deck — `slides.deck`. */
export interface Deck extends tilo.Resource {
  title: string;
  palette: PaletteKind;
  look: DeckLook;
  slides: Array<tilo.Entry<Slide>>;
  addSlide(args?: {
    title?: string;
    body?: string;
    layout?: Layout;
    after?: string;
  }): Promise<unknown>;
  addSlides(args: {
    slides: OutlinedSlide[];
    after?: string;
  }): Promise<unknown>;
  insertSlide(args: {
    at: number;
    slide: SlideData;
  }): Promise<void>;
  /** Runs `insert-slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertSlideMany(items: InsertSlideArgs[]): Promise<tilo.ActionAnswer[]>;
  duplicateSlide(args: {
    slide: string;
  }): Promise<void>;
  /** Runs `duplicate-slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  duplicateSlideMany(items: DuplicateSlideArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteSlide(args: {
    slide: string;
  }): Promise<void>;
  /** Runs `delete-slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteSlideMany(items: DeleteSlideArgs[]): Promise<tilo.ActionAnswer[]>;
  moveSlide(args: {
    slide: string;
    position: number;
  }): Promise<unknown>;
  setText(args: {
    slide: string;
    title?: string;
    body?: string;
  }): Promise<unknown>;
  addElement(args: {
    slide: string;
    element: NewElement;
  }): Promise<unknown>;
  insertElement(args: {
    slide: string;
    element: ElementData;
  }): Promise<void>;
  /** Runs `insert-element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertElementMany(items: InsertElementArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteElement(args: {
    slide: string;
    element: string;
  }): Promise<void>;
  /** Runs `delete-element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteElementMany(items: DeleteElementArgs[]): Promise<tilo.ActionAnswer[]>;
  bringToFront(args: {
    slide: string;
    element: string;
  }): Promise<void>;
  /** Runs `bring-to-front` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  bringToFrontMany(items: BringToFrontArgs[]): Promise<tilo.ActionAnswer[]>;
  sendToBack(args: {
    slide: string;
    element: string;
  }): Promise<void>;
  /** Runs `send-to-back` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sendToBackMany(items: SendToBackArgs[]): Promise<tilo.ActionAnswer[]>;
  replaceDeck(args: {
    title: string;
    palette: PaletteKind;
    slides: SlideData[];
    look?: DeckLook;
  }): Promise<void>;
  /** Runs `replace-deck` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replaceDeckMany(items: ReplaceDeckArgs[]): Promise<tilo.ActionAnswer[]>;
  readDeck(): Promise<void>;
  exportPptxContent(): Promise<void>;
}

/** The actions `Deck` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DECK_ACTIONS = ["addSlide", "addSlides", "insertSlide", "insertSlideMany", "duplicateSlide", "duplicateSlideMany", "deleteSlide", "deleteSlideMany", "moveSlide", "setText", "addElement", "insertElement", "insertElementMany", "deleteElement", "deleteElementMany", "bringToFront", "bringToFrontMany", "sendToBack", "sendToBackMany", "replaceDeck", "replaceDeckMany", "readDeck", "exportPptxContent"] as const;

/** The `slides.deck` kind at `path`. */
export function deck(ws: tilo.Workspace, path: string): Deck {
  return resourceProxy(ws, path, SURFACE, DECK_ACTIONS) as unknown as Deck;
}

