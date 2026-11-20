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
export const SURFACE = "so.tilo.pdf-viewer@de7f897cc4391b52";

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
  author: Author;
  chars: number[] | null;
  created_ms: number;
  space: GeomSpace;
}

/** One of member, named: an object whose case the app tags it with. */
export type Author = Record<string, unknown>;

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

export interface Comment {
  anchor: PdfAnchor;
  author: Author;
  date: string;
  text: string;
  body: ClipBlock[];
  resolved: boolean;
  replies: Array<tilo.Entry<Reply>>;
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

/** One of Bool, Int, Str: an object whose case the app tags it with. */
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
  author: Author;
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
  without_text: number[];
}

export interface Stroke {
  xy: number[];
}

/** The arguments of `record-reading-position`. */
export interface RecordReadingPositionArgs {
  page: string;
  fraction: number;
  zoom: number;
}

/** The arguments of `start-page-plan`. */
export interface StartPagePlanArgs {
  pages: number;
}

/** The arguments of `pin-source`. */
export interface PinSourceArgs {
  source: unknown;
  name: string;
}

/** The arguments of `adopt-existing`. */
export interface AdoptExistingArgs {
  source: string;
  markup: ExistingMarkup;
}

/** The arguments of `import-pages`. */
export interface ImportPagesArgs {
  pages: PagePlanEntry[];
  after?: string;
}

/** The arguments of `duplicate-page`. */
export interface DuplicatePageArgs {
  page: string;
  copy?: string;
}

/** The arguments of `delete-page`. */
export interface DeletePageArgs {
  page: string;
}

/** The arguments of `move-page`. */
export interface MovePageArgs {
  page: string;
  after?: string;
}

/** The arguments of `rotate-page`. */
export interface RotatePageArgs {
  page: string;
}

/** The arguments of `add-annotation`. */
export interface AddAnnotationArgs {
  annotation: NewAnnotation;
}

/** The arguments of `delete-annotation`. */
export interface DeleteAnnotationArgs {
  annotation: string;
}

/** The arguments of `create-note`. */
export interface CreateNoteArgs {
  note: NewNote;
}

/** The arguments of `add-note`. */
export interface AddNoteArgs {
  text: string;
  page?: number;
}

/** The arguments of `add-comment`. */
export interface AddCommentArgs {
  comment: NewComment;
}

/** The arguments of `reply`. */
export interface ReplyArgs {
  comment: string;
  text: string;
  id?: string;
  rich?: ClipBlock[];
}

/** The arguments of `delete-comment`. */
export interface DeleteCommentArgs {
  comment: string;
}

/** The arguments of `add-signature`. */
export interface AddSignatureArgs {
  signature: NewSignature;
}

/** The arguments of `add-typed-signature`. */
export interface AddTypedSignatureArgs {
  signer: string;
  page?: number;
}

/** The arguments of `delete-signature`. */
export interface DeleteSignatureArgs {
  signature: string;
}

/** The arguments of `set-signer`. */
export interface SetSignerArgs {
  name: string;
}

/** The arguments of `set-strokes`. */
export interface SetStrokesArgs {
  strokes: unknown[][];
}

/** PDF — `pdf.document`. */
export interface Document extends tilo.Resource {
  readonly source: unknown;
  readonly source_name: string;
  readonly page_plan_seeded: boolean;
  readonly imported_source: string;
  pages: Array<tilo.Entry<PageOccurrence>>;
  annotations: Array<tilo.Entry<Annotation>>;
  comments: Array<tilo.Entry<Comment>>;
  signatures: Array<tilo.Entry<StoredSignature>>;
  form_values: Record<string, unknown>;
  readonly adopted: Record<string, unknown>;
  reading_page: string;
  reading_fraction: number;
  reading_zoom: number;
  recordReadingPosition(args: {
    page: string;
    fraction: number;
    zoom: number;
  }): Promise<void>;
  /** Runs `record-reading-position` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordReadingPositionMany(items: RecordReadingPositionArgs[]): Promise<tilo.ActionAnswer[]>;
  startPagePlan(args: {
    pages: number;
  }): Promise<void>;
  /** Runs `start-page-plan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  startPagePlanMany(items: StartPagePlanArgs[]): Promise<tilo.ActionAnswer[]>;
  pinSource(args: {
    source: unknown;
    name: string;
  }): Promise<void>;
  /** Runs `pin-source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pinSourceMany(items: PinSourceArgs[]): Promise<tilo.ActionAnswer[]>;
  adoptExisting(args: {
    source: string;
    markup: ExistingMarkup;
  }): Promise<void>;
  /** Runs `adopt-existing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  adoptExistingMany(items: AdoptExistingArgs[]): Promise<tilo.ActionAnswer[]>;
  importPages(args: {
    pages: PagePlanEntry[];
    after?: string;
  }): Promise<void>;
  /** Runs `import-pages` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importPagesMany(items: ImportPagesArgs[]): Promise<tilo.ActionAnswer[]>;
  duplicatePage(args: {
    page: string;
    copy?: string;
  }): Promise<void>;
  /** Runs `duplicate-page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  duplicatePageMany(items: DuplicatePageArgs[]): Promise<tilo.ActionAnswer[]>;
  deletePage(args: {
    page: string;
  }): Promise<void>;
  /** Runs `delete-page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deletePageMany(items: DeletePageArgs[]): Promise<tilo.ActionAnswer[]>;
  movePage(args: {
    page: string;
    after?: string;
  }): Promise<void>;
  /** Runs `move-page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  movePageMany(items: MovePageArgs[]): Promise<tilo.ActionAnswer[]>;
  rotatePage(args: {
    page: string;
  }): Promise<void>;
  /** Runs `rotate-page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rotatePageMany(items: RotatePageArgs[]): Promise<tilo.ActionAnswer[]>;
  addAnnotation(args: {
    annotation: NewAnnotation;
  }): Promise<void>;
  /** Runs `add-annotation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addAnnotationMany(items: AddAnnotationArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteAnnotation(args: {
    annotation: string;
  }): Promise<void>;
  /** Runs `delete-annotation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteAnnotationMany(items: DeleteAnnotationArgs[]): Promise<tilo.ActionAnswer[]>;
  createNote(args: {
    note: NewNote;
  }): Promise<void>;
  /** Runs `create-note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  createNoteMany(items: CreateNoteArgs[]): Promise<tilo.ActionAnswer[]>;
  addNote(args: {
    text: string;
    page?: number;
  }): Promise<void>;
  /** Runs `add-note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addNoteMany(items: AddNoteArgs[]): Promise<tilo.ActionAnswer[]>;
  addComment(args: {
    comment: NewComment;
  }): Promise<void>;
  /** Runs `add-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addCommentMany(items: AddCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  reply(args: {
    comment: string;
    text: string;
    id?: string;
    rich?: ClipBlock[];
  }): Promise<void>;
  /** Runs `reply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replyMany(items: ReplyArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteComment(args: {
    comment: string;
  }): Promise<void>;
  /** Runs `delete-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteCommentMany(items: DeleteCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  addSignature(args: {
    signature: NewSignature;
  }): Promise<void>;
  /** Runs `add-signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addSignatureMany(items: AddSignatureArgs[]): Promise<tilo.ActionAnswer[]>;
  addTypedSignature(args: {
    signer: string;
    page?: number;
  }): Promise<void>;
  /** Runs `add-typed-signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addTypedSignatureMany(items: AddTypedSignatureArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteSignature(args: {
    signature: string;
  }): Promise<void>;
  /** Runs `delete-signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteSignatureMany(items: DeleteSignatureArgs[]): Promise<tilo.ActionAnswer[]>;
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

/** The actions `Document` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOCUMENT_ACTIONS = ["recordReadingPosition", "recordReadingPositionMany", "startPagePlan", "startPagePlanMany", "pinSource", "pinSourceMany", "adoptExisting", "adoptExistingMany", "importPages", "importPagesMany", "duplicatePage", "duplicatePageMany", "deletePage", "deletePageMany", "movePage", "movePageMany", "rotatePage", "rotatePageMany", "addAnnotation", "addAnnotationMany", "deleteAnnotation", "deleteAnnotationMany", "createNote", "createNoteMany", "addNote", "addNoteMany", "addComment", "addCommentMany", "reply", "replyMany", "deleteComment", "deleteCommentMany", "addSignature", "addSignatureMany", "addTypedSignature", "addTypedSignatureMany", "deleteSignature", "deleteSignatureMany", "readText", "readFormFields", "lookPage"] as const;

/** The `pdf.document` kind at `path`. */
export function pdfDocument(ws: tilo.Workspace, path: string): Document {
  return resourceProxy(ws, path, SURFACE, DOCUMENT_ACTIONS) as unknown as Document;
}

/** Signature — `pdf.signature-profile`. */
export interface SignatureProfile extends tilo.Resource {
  signer: string;
  strokes: Stroke[];
  readonly restored_previous: boolean;
  setSigner(args: {
    name: string;
  }): Promise<void>;
  /** Runs `set-signer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setSignerMany(items: SetSignerArgs[]): Promise<tilo.ActionAnswer[]>;
  setStrokes(args: {
    strokes: unknown[][];
  }): Promise<void>;
  /** Runs `set-strokes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setStrokesMany(items: SetStrokesArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `SignatureProfile` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SIGNATUREPROFILE_ACTIONS = ["setSigner", "setSignerMany", "setStrokes", "setStrokesMany"] as const;

/** The `pdf.signature-profile` kind at `path`. */
export function signatureProfile(ws: tilo.Workspace, path: string): SignatureProfile {
  return resourceProxy(ws, path, SURFACE, SIGNATUREPROFILE_ACTIONS) as unknown as SignatureProfile;
}

