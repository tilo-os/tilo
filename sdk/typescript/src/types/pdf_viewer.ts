/**
 * Types for PDF Viewer (`so.tilo.pdf-viewer`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Document` is a `Resource` of the app's `pdf.document`, made by its factory with the
 * resource's path (`pdf-viewer/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

export interface AdoptedAnnotation {
  id: string;
  page_id: string;
  mark: AnnoKind;
  color: unknown;
  author: string;
  created_ms: number;
}

export interface AdoptedEntry {
  id: string;
  page: number;
  index: number;
  name: string;
  kind: string;
  print: string;
  parent: string | null;
}

export interface AdoptedReply {
  id: string;
  author: string;
  text: string;
}

export interface AdoptedThread {
  id: string;
  anno: string | null;
  page_id: string;
  author: string;
  text: string;
  date: string;
  replies: AdoptedReply[];
}

/** One of markup, ink, note, freetext: an object whose case the app tags it with. */
export type AnnoKind = Record<string, unknown>;

export interface Annotation {
  id: string;
  page_id: string;
  mark: unknown;
  color: unknown;
  author: string;
  chars: number[] | null;
  created_ms: number;
  space: GeomSpace;
}

/** One of bool, int, str: an object whose case the app tags it with. */
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

export interface Comment {
  anchor: PdfAnchor;
  author: string;
  date: string;
  text: string;
  body: ClipBlock[];
  resolved: boolean;
  replies: Reply[];
}

export interface ExistingMarkup {
  annotations: AdoptedAnnotation[];
  comments: AdoptedThread[];
  form_values: Record<string, FormEntry>;
  adopted: AdoptedEntry[];
}

/** One of text, checked, choice: an object whose case the app tags it with. */
export type FormEntry = Record<string, unknown>;

/** One of "page", "user". */
export type GeomSpace = "page" | "user";

export interface ImportedPage {
  hash: string;
  kind: string;
  page: number;
  width_milli: number;
  height_milli: number;
}

export interface MarkSpan {
  start: number;
  end: number;
  name: string;
  value: MarkValue;
}

/** One of bool, int, str: an object whose case the app tags it with. */
export type MarkValue = Record<string, unknown>;

/** One of "highlight", "underline", "strikeout", "squiggly". */
export type MarkupStyle = "highlight" | "underline" | "strikeout" | "squiggly";

export interface NewAnnotation {
  id: string;
  page_id: string;
  mark: AnnoKind;
  color: unknown;
  chars: number[] | null;
  space: GeomSpace;
}

export interface NewComment {
  id: string;
  anno: string | null;
  page_id: string | null;
  text: string;
  rich: ClipBlock[];
}

export interface NewNote {
  annotation: string;
  comment: string;
  page_id: string;
  x: number;
  y: number;
  color: unknown;
  text: string;
  space: GeomSpace;
}

export interface NewSignature {
  id: string;
  page_id: string;
  rect: PtRect;
  signer: string;
  date: string;
  ink: SignatureInk;
  space: GeomSpace;
}

export interface PageOccurrence {
  source_page: number;
  imported: unknown | null;
  rotation: number;
}

export interface PagePlanEntry {
  id: string;
  source_page: number;
  imported: ImportedPage | null;
  rotation: number;
}

export interface PdfAnchor {
  page_id: string;
  annotation: string;
}

export interface PtRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Reply {
  author: string;
  date: string;
  text: string;
  body: ClipBlock[];
}

/** One of typed, drawn: an object whose case the app tags it with. */
export type SignatureInk = Record<string, unknown>;

export interface StoredSignature {
  id: string;
  page_id: string;
  rect: unknown;
  signer: string;
  date: string;
  ink: unknown;
  space: GeomSpace;
}

/** One of "text", "checkbox", "radio", "choice". */
export type FormFieldKind = "text" | "checkbox" | "radio" | "choice";

export interface FormFieldRow {
  name: string;
  kind: FormFieldKind;
  value: FormEntry;
  path: unknown | null;
  options: string[];
  export: string | null;
  page: number | null;
  read_only: boolean;
  required: boolean;
}

export interface FormFields {
  file: string;
  fields: FormFieldRow[];
}

export interface PdfPageText {
  page: number;
  text: string;
}

export interface PdfText {
  file: string;
  page_count: number;
  pages: PdfPageText[];
  next: number | null;
}

export interface Stroke {
  xy: number[];
}

/** PDF — `pdf.document`. */
export interface Document extends tilo.Resource {
  readonly source: unknown;
  readonly source_name: string;
  readonly page_plan_seeded: boolean;
  readonly imported_source: string;
  pages: PageOccurrence[];
  annotations: Annotation[];
  comments: Comment[];
  signatures: StoredSignature[];
  form_values: Record<string, unknown>;
  readonly adopted: Record<string, unknown>;
  reading_page: string;
  reading_fraction: number;
  reading_zoom: number;
  recordReadingPosition(args: {
    page: string;
    fraction: number;
    zoom: number;
  }): Promise<tilo.Run | void>;
  startPagePlan(args: {
    pages: number;
  }): Promise<tilo.Run | void>;
  pinSource(args: {
    hash: string;
    len: number;
    name: string;
  }): Promise<tilo.Run | void>;
  adoptExisting(args: {
    source: string;
    markup: ExistingMarkup;
  }): Promise<tilo.Run | void>;
  importPages(args: {
    pages: PagePlanEntry[];
    after?: string;
  }): Promise<tilo.Run | void>;
  duplicatePage(args: {
    page: string;
    copy?: string;
  }): Promise<tilo.Run | void>;
  deletePage(args: {
    page: string;
  }): Promise<tilo.Run | void>;
  movePage(args: {
    page: string;
    after?: string;
  }): Promise<tilo.Run | void>;
  rotatePage(args: {
    page: string;
  }): Promise<tilo.Run | void>;
  addAnnotation(args: {
    annotation: NewAnnotation;
  }): Promise<tilo.Run | void>;
  deleteAnnotation(args: {
    annotation: string;
  }): Promise<tilo.Run | void>;
  createNote(args: {
    note: NewNote;
  }): Promise<tilo.Run | void>;
  addNote(args: {
    text: string;
    page?: number;
  }): Promise<tilo.Run | void>;
  addComment(args: {
    comment: NewComment;
  }): Promise<tilo.Run | void>;
  reply(args: {
    comment: string;
    text: string;
    id?: string;
    rich?: ClipBlock[];
  }): Promise<tilo.Run | void>;
  deleteComment(args: {
    comment: string;
  }): Promise<tilo.Run | void>;
  addSignature(args: {
    signature: NewSignature;
  }): Promise<tilo.Run | void>;
  addTypedSignature(args?: {
    signer?: string;
    page?: number;
  }): Promise<tilo.Run | void>;
  deleteSignature(args: {
    signature: string;
  }): Promise<tilo.Run | void>;
  readText(args?: {
    from_page?: number;
    to_page?: number;
  }): Promise<PdfText>;
  readFormFields(): Promise<FormFields>;
  lookPage(args: {
    page: number;
    size?: number;
  }): Promise<void>;
}

/** The `pdf.document` kind at `path`. */
export function pdfDocument(ws: tilo.Workspace, path: string): Document {
  return resourceProxy(ws, path) as unknown as Document;
}

/** Signature — `pdf.signature-profile`. */
export interface SignatureProfile extends tilo.Resource {
  signer: string;
  strokes: Stroke[];
  setSigner(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  setStrokes(args: {
    strokes: unknown[][];
  }): Promise<tilo.Run | void>;
}

/** The `pdf.signature-profile` kind at `path`. */
export function signatureProfile(ws: tilo.Workspace, path: string): SignatureProfile {
  return resourceProxy(ws, path) as unknown as SignatureProfile;
}

