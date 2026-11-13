/**
 * Types for Notes (`so.tilo.notes`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notebook` is a `Resource` of the app's `notes.notebook`, made by its factory with the
 * resource's path (`notes/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.notes@8ce7fd71d646d813";

export interface Page {
  title: string;
  parent: string | null;
  body: string;
  tags: string[];
  creator: string | null;
  reactions: unknown;
}

export interface Tag {
  name: string;
}

/** Notebook — `notes.notebook`. */
export interface Notebook extends tilo.Resource {
  pages: Page[];
  tags: Tag[];
  readonly images: Record<string, unknown>;
  newNote(args: {
    title: string;
    parent?: string;
  }): Promise<unknown | tilo.Run>;
  newChild(args: {
    page: string;
    title: string;
  }): Promise<unknown | tilo.Run>;
  rename(args: {
    page: string;
    title: string;
  }): Promise<tilo.Run | void>;
  delete(args: {
    page: string;
  }): Promise<tilo.Run | void>;
  moveNote(args: {
    page: string;
    parent?: string;
    before?: string;
  }): Promise<tilo.Run | void>;
  moveUp(args: {
    page: string;
  }): Promise<tilo.Run | void>;
  moveDown(args: {
    page: string;
  }): Promise<tilo.Run | void>;
  indent(args: {
    page: string;
  }): Promise<tilo.Run | void>;
  outdent(args: {
    page: string;
  }): Promise<tilo.Run | void>;
  duplicate(args: {
    page: string;
  }): Promise<tilo.Run | void>;
  newTag(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  tag(args: {
    page: string;
    name: string;
  }): Promise<tilo.Run | void>;
  deleteTag(args: {
    tag: string;
  }): Promise<tilo.Run | void>;
  keepImage(args: {
    bytes: unknown;
  }): Promise<tilo.Run | void>;
  placeImage(args: {
    page: string;
    bytes: unknown;
    alt?: string;
  }): Promise<tilo.Run | void>;
  createPage(args: {
    title: string;
    markdown?: string;
  }): Promise<unknown | tilo.Run>;
  importMarkdown(args: {
    title: string;
    markdown: string;
    images: unknown[];
    note?: string;
  }): Promise<tilo.Run | void>;
  appendToNote(args: {
    markdown: string;
    title?: string;
  }): Promise<unknown | tilo.Run>;
  exportMarkdownContent(args?: {
    page?: string;
    title?: string;
  }): Promise<void>;
}

/** The `notes.notebook` kind at `path`. */
export function notebook(ws: tilo.Workspace, path: string): Notebook {
  return resourceProxy(ws, path, SURFACE) as unknown as Notebook;
}

