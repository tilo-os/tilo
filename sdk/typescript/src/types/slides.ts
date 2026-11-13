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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.slides@ecf6ae57e9051fc7";

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

/** One of "indigo", "emerald", "amber", "rose", "sky". */
export type PaletteKind = "indigo" | "emerald" | "amber" | "rose" | "sky";

export interface Slide {
  layout: Layout;
  background: Background;
  notes: string;
  elements: Element[];
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

/** Deck — `slides.deck`. */
export interface Deck extends tilo.Resource {
  title: string;
  palette: PaletteKind;
  look: DeckLook;
  slides: Slide[];
  addSlide(args?: {
    title?: string;
    body?: string;
    layout?: Layout;
    after?: string;
  }): Promise<unknown | tilo.Run>;
  insertSlide(args: {
    at: number;
    slide: SlideData;
  }): Promise<tilo.Run | void>;
  duplicateSlide(args: {
    slide: string;
  }): Promise<tilo.Run | void>;
  deleteSlide(args: {
    slide: string;
  }): Promise<tilo.Run | void>;
  moveSlide(args: {
    slide: string;
    position: number;
  }): Promise<unknown | tilo.Run>;
  setText(args: {
    slide: string;
    title?: string;
    body?: string;
  }): Promise<unknown | tilo.Run>;
  addElement(args: {
    slide: string;
    element: NewElement;
  }): Promise<unknown | tilo.Run>;
  insertElement(args: {
    slide: string;
    element: ElementData;
  }): Promise<tilo.Run | void>;
  deleteElement(args: {
    slide: string;
    element: string;
  }): Promise<tilo.Run | void>;
  bringToFront(args: {
    slide: string;
    element: string;
  }): Promise<tilo.Run | void>;
  sendToBack(args: {
    slide: string;
    element: string;
  }): Promise<tilo.Run | void>;
  replaceDeck(args: {
    title: string;
    palette: PaletteKind;
    slides: SlideData[];
    look?: DeckLook;
  }): Promise<tilo.Run | void>;
  readDeck(): Promise<void>;
  exportPptxContent(): Promise<void>;
}

/** The `slides.deck` kind at `path`. */
export function deck(ws: tilo.Workspace, path: string): Deck {
  return resourceProxy(ws, path, SURFACE) as unknown as Deck;
}

