/**
 * Types for Slides (`so.tilo.slides`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Deck` is a `Resource` of the app's `tl.so.tilo.slides.Deck`, made by its factory with the
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
export const SURFACE = "so.tilo.slides@cd42c8463e98a698";

/** How a text box sets its words. One of "left", "center", "right", "justify". */
export type Align = "left" | "center" | "right" | "justify";

/** What a slide stands on: the deck's surface, its accent, or its deep tone. One of "surface", "accent", "deep". */
export type Background = "surface" | "accent" | "deep";

/** What an element is: words, a shape, a line or a picture. One of "text", "rect", "ellipse", "line", "image". */
export type ElementKind = "text" | "rect" | "ellipse" | "line" | "image";

/** How a slide arranges what it holds: the shape its content boxes start in. One of "title", "title_content", "two_column", "section", "blank". */
export type Layout = "title" | "title_content" | "two_column" | "section" | "blank";

/** A deck's own light or dark surfaces, so it looks the same on every screen and in a saved file. One of "light", "dark". */
export type Look = "light" | "dark";

/** Which hue drives the deck's accent. Every other colour comes from the deck's look. One of "indigo", "emerald", "amber", "rose", "sky". */
export type Palette = "indigo" | "emerald" | "amber" | "rose" | "sky";

/** The arguments of `duplicate_slide`. */
export interface DuplicateSlideArgs {
  s: Slide;
}

/** The arguments of `duplicate_current_slide`. */
export interface DuplicateCurrentSlideArgs {
  showing: string;
}

/** The arguments of `delete_slide`. */
export interface DeleteSlideArgs {
  s: Slide;
}

/** The arguments of `move_slide`. */
export interface MoveSlideArgs {
  s: Slide;
  at: number;
}

/** The arguments of `set_text`. */
export interface SetTextArgs {
  s: Slide;
  title?: string;
  body?: string;
}

/** The arguments of `set_palette`. */
export interface SetPaletteArgs {
  palette: Palette;
}

/** The arguments of `set_look`. */
export interface SetLookArgs {
  look: Look;
}

/** The arguments of `set_title`. */
export interface SetTitleArgs {
  title: string;
}

/** The arguments of `open_presentation`. */
export interface OpenPresentationArgs {
  file: unknown;
}

/** The arguments of `adopt`. */
export interface AdoptArgs {
  file: unknown;
}

/** The arguments of `clear_notice`. */
export interface ClearNoticeArgs {
}

/** The arguments of `import_file`. */
export interface ImportFileArgs {
  file: unknown;
}

/** The arguments of `import_files`. */
export interface ImportFilesArgs {
  files: unknown[];
}

/** The arguments of `begin_presenting`. */
export interface BeginPresentingArgs {
}

/** The arguments of `end_presenting`. */
export interface EndPresentingArgs {
}

/** The arguments of `pause_clock`. */
export interface PauseClockArgs {
}

/** The arguments of `resume_clock`. */
export interface ResumeClockArgs {
}

/** The arguments of `adopt_slide`. */
export interface AdoptSlideArgs {
  f: SlideFile;
}

/** The arguments of `edit-block`. */
export interface EditBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `insert-block`. */
export interface InsertBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `delete-block`. */
export interface DeleteBlockArgs {
  block: string;
}

/** The arguments of `duplicate_element`. */
export interface DuplicateElementArgs {
  e: Element;
}

/** The arguments of `delete_element`. */
export interface DeleteElementArgs {
  e: Element;
}

/** The arguments of `bring_to_front`. */
export interface BringToFrontArgs {
  e: Element;
}

/** The arguments of `send_to_back`. */
export interface SendToBackArgs {
  e: Element;
}

/** The arguments of `move_element`. */
export interface MoveElementArgs {
  e: Element;
  x: number;
  y: number;
}

/** The arguments of `move_element_to`. */
export interface MoveElementToArgs {
  target: string;
  at: number[];
}

/** The arguments of `nudge_element_to`. */
export interface NudgeElementToArgs {
  target: string;
  dx: number;
  dy: number;
}

/** The arguments of `resize_element`. */
export interface ResizeElementArgs {
  e: Element;
  at: number[];
}

/** The arguments of `resize_element_to`. */
export interface ResizeElementToArgs {
  target: string;
  at: number[];
}

/** The arguments of `set_layout`. */
export interface SetLayoutArgs {
  layout: Layout;
}

/** The arguments of `set_background`. */
export interface SetBackgroundArgs {
  background: Background;
}

/** The arguments of `set_notes`. */
export interface SetNotesArgs {
  notes: string;
}

/** The arguments of `paste_element`. */
export interface PasteElementArgs {
}

/** The arguments of `add_picture`. */
export interface AddPictureArgs {
  file: unknown;
}

/** The arguments of `edit-block`. */
export interface ElementEditBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `insert-block`. */
export interface ElementInsertBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `delete-block`. */
export interface ElementDeleteBlockArgs {
  block: string;
}

/** The arguments of `set_style`. */
export interface SetStyleArgs {
  fill?: string;
  stroke?: string;
  stroke_width?: number;
  font_size?: number;
  align?: Align;
  bold?: boolean;
  italic?: boolean;
  radius?: number;
}

/** Deck — `tl.so.tilo.slides.Deck`. */
export interface Deck extends tilo.Resource {
  pwdtrcuk: string;  // title
  ep3yewja: Palette;  // palette
  "3xehcyrf": Look;  // look
  nhumnzoj: Array<tilo.Entry<Slide>>;  // slides
  weqf5aou: string | null;  // presenting_since
  u6doynxu: number;  // clock_held
  xwjxu7wj: boolean;  // clock_paused
  sy2pbelg: string;  // notice
  /** Adds a slide after `after`, or at the end, from the template its layout names. */
  add_slide(args?: {
    title?: string;
    body?: string;
    layout?: Layout;
    after?: Slide;
  }): Promise<Slide>;
  /** Adds a slide from the template its layout names, and answers it. */
  add_authored_slide(args: {
    title: string;
    body: string;
    fallback: Layout;
    layout?: Layout;
    notes?: string;
    after?: Slide;
  }): Promise<Slide>;
  /** Adds several slides in one step, in the order given: a deck written from an outline, as the JSON a person or an agent hands over. */
  add_slides(args: {
    outline: string;
    after?: Slide;
  }): Promise<unknown>;
  /** Copies a slide and everything on it, right after it. */
  duplicate_slide(args: {
    s: Slide;
  }): Promise<void>;
  /** Runs `duplicate_slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  duplicate_slideMany(items: DuplicateSlideArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies the slide the window shows, right after it. */
  duplicate_current_slide(args: {
    showing: string;
  }): Promise<void>;
  /** Runs `duplicate_current_slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  duplicate_current_slideMany(items: DuplicateCurrentSlideArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a slide. A deck keeps at least one. */
  delete_slide(args: {
    s: Slide;
  }): Promise<void>;
  /** Runs `delete_slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_slideMany(items: DeleteSlideArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a slide to `at`, the place among the slides a drag reports. */
  move_slide(args: {
    s: Slide;
    at: number;
  }): Promise<void>;
  /** Runs `move_slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_slideMany(items: MoveSlideArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes a slide's title and body: the first two text boxes, as the templates make them. */
  set_text(args: {
    s: Slide;
    title?: string;
    body?: string;
  }): Promise<void>;
  /** Runs `set_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_textMany(items: SetTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the deck's accent hue. */
  set_palette(args: {
    palette: Palette;
  }): Promise<void>;
  /** Runs `set_palette` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_paletteMany(items: SetPaletteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the deck's light or dark surfaces. */
  set_look(args: {
    look: Look;
  }): Promise<void>;
  /** Runs `set_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_lookMany(items: SetLookArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Names the deck. */
  set_title(args: {
    title: string;
  }): Promise<void>;
  /** Runs `set_title` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_titleMany(items: SetTitleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A deck as text: its slides in order, each title, body and other elements with its id, and its speaker notes. An agent reads a deck without the window. */
  read_deck(): Promise<string>;
  /** Replaces the deck with the one a presentation file holds. The file is opened by the app that opens its kind. */
  open_presentation(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `open_presentation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_presentationMany(items: OpenPresentationArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The deck as an OpenDocument presentation. */
  export_odp(): Promise<unknown>;
  /** The deck as a PowerPoint presentation. */
  export_pptx(): Promise<unknown>;
  /** Reads a deck file into this deck: its slides replace what the deck holds. */
  adopt(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `adopt` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  adoptMany(items: AdoptArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Closes the message a deck shows about a file it could not open. */
  clear_notice(): Promise<void>;
  /** Runs `clear_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_noticeMany(items: ClearNoticeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The deck a file holds, kept as this deck: its slides replace what it held. */
  import_file(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `import_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_fileMany(items: ImportFileArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the first presentation of a set dropped on the window. */
  import_files(args: {
    files: unknown[];
  }): Promise<void>;
  /** Runs `import_files` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_filesMany(items: ImportFilesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts the presentation clock: the speaker's elapsed time runs from now. */
  begin_presenting(): Promise<void>;
  /** Runs `begin_presenting` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  begin_presentingMany(items: BeginPresentingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the presentation clock. */
  end_presenting(): Promise<void>;
  /** Runs `end_presenting` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  end_presentingMany(items: EndPresentingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Holds the clock where it stands, until the speaker starts it again. */
  pause_clock(): Promise<void>;
  /** Runs `pause_clock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pause_clockMany(items: PauseClockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Runs the clock on from where it was held. */
  resume_clock(): Promise<void>;
  /** Runs `resume_clock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resume_clockMany(items: ResumeClockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps one slide a deck file brings. */
  adopt_slide(args: {
    f: SlideFile;
  }): Promise<void>;
  /** Runs `adopt_slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  adopt_slideMany(items: AdoptSlideArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Deck` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DECK_ACTIONS = ["add_slide", "add_authored_slide", "add_slides", "duplicate_slide", "duplicate_slideMany", "duplicate_current_slide", "duplicate_current_slideMany", "delete_slide", "delete_slideMany", "move_slide", "move_slideMany", "set_text", "set_textMany", "set_palette", "set_paletteMany", "set_look", "set_lookMany", "set_title", "set_titleMany", "read_deck", "open_presentation", "open_presentationMany", "export_odp", "export_pptx", "adopt", "adoptMany", "clear_notice", "clear_noticeMany", "import_file", "import_fileMany", "import_files", "import_filesMany", "begin_presenting", "begin_presentingMany", "end_presenting", "end_presentingMany", "pause_clock", "pause_clockMany", "resume_clock", "resume_clockMany", "adopt_slide", "adopt_slideMany"] as const;

/** The `tl.so.tilo.slides.Deck` kind at `path`. */
export function deck(ws: tilo.Workspace, path: string): Deck {
  return resourceProxy(ws, path, SURFACE, DECK_ACTIONS) as unknown as Deck;
}

/** Slide — `tl.so.tilo.slides.Slide`. */
export interface Slide extends tilo.Resource {
  w3sjwjk2: Layout;  // layout
  x3bud3c2: Background;  // background
  qowtxual: string;  // notes
  lbme2z4w: Array<tilo.Entry<Element>>;  // elements
  /** Set one paragraph of a rich-text field to what a Markdown text says. */
  editBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editBlockMany(items: EditBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Put a paragraph before the one the address names. */
  insertBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertBlockMany(items: InsertBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remove the paragraph the address names, and the paragraphs it holds. */
  deleteBlock(args: {
    block: string;
  }): Promise<void>;
  /** Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteBlockMany(items: DeleteBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies an element in place on its slide, a little down and across from it. */
  duplicate_element(args: {
    e: Element;
  }): Promise<void>;
  /** Runs `duplicate_element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  duplicate_elementMany(items: DuplicateElementArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds an element to a slide: what it is, where it stands, and any style it takes. */
  add_element(args: {
    kind: ElementKind;
    x?: number;
    y?: number;
    w?: number;
    h?: number;
    text?: string;
    src?: string;
    fill?: string;
    text_color?: string;
    stroke?: string;
    stroke_width?: number;
    font_size?: number;
    align?: Align;
    bold?: boolean;
    italic?: boolean;
    radius?: number;
  }): Promise<Element>;
  /** Inserts an element given whole, as the JSON another deck copied: a paste from anywhere. */
  insert_element(args: {
    element: string;
  }): Promise<unknown>;
  /** Deletes an element. */
  delete_element(args: {
    e: Element;
  }): Promise<void>;
  /** Runs `delete_element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_elementMany(items: DeleteElementArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Brings an element to the front. */
  bring_to_front(args: {
    e: Element;
  }): Promise<void>;
  /** Runs `bring_to_front` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  bring_to_frontMany(items: BringToFrontArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sends an element to the back. */
  send_to_back(args: {
    e: Element;
  }): Promise<void>;
  /** Runs `send_to_back` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  send_to_backMany(items: SendToBackArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves an element to a place on the slide, as a drag or the arrow keys place it. */
  move_element(args: {
    e: Element;
    x: number;
    y: number;
  }): Promise<void>;
  /** Runs `move_element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_elementMany(items: MoveElementArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves the element `target` names to `at`, as a drag on the stage reports it. */
  move_element_to(args: {
    target: string;
    at: number[];
  }): Promise<void>;
  /** Runs `move_element_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_element_toMany(items: MoveElementToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Nudges the element `target` names by `dx` and `dy` slide units. */
  nudge_element_to(args: {
    target: string;
    dx: number;
    dy: number;
  }): Promise<void>;
  /** Runs `nudge_element_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  nudge_element_toMany(items: NudgeElementToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sizes an element, as dragging its edge does. */
  resize_element(args: {
    e: Element;
    at: number[];
  }): Promise<void>;
  /** Runs `resize_element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resize_elementMany(items: ResizeElementArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sizes the element `target` names, as a drag on its edge reports it. */
  resize_element_to(args: {
    target: string;
    at: number[];
  }): Promise<void>;
  /** Runs `resize_element_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resize_element_toMany(items: ResizeElementToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets a slide's layout. */
  set_layout(args: {
    layout: Layout;
  }): Promise<void>;
  /** Runs `set_layout` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_layoutMany(items: SetLayoutArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets what a slide stands on. */
  set_background(args: {
    background: Background;
  }): Promise<void>;
  /** Runs `set_background` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_backgroundMany(items: SetBackgroundArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes what the speaker says over a slide. */
  set_notes(args: {
    notes: string;
  }): Promise<void>;
  /** Runs `set_notes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_notesMany(items: SetNotesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Cuts the chosen element: its JSON to the clipboard as another deck pastes it, and the element off the slide. */
  cut_element(args: {
    e: Element;
  }): Promise<void>;
  /** Pastes an element from the clipboard onto a slide, nudged clear of the one it came from. */
  paste_element(): Promise<void>;
  /** Runs `paste_element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  paste_elementMany(items: PasteElementArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a chosen picture on the slide. */
  add_picture(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `add_picture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_pictureMany(items: AddPictureArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Slide` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SLIDE_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "duplicate_element", "duplicate_elementMany", "add_element", "insert_element", "delete_element", "delete_elementMany", "bring_to_front", "bring_to_frontMany", "send_to_back", "send_to_backMany", "move_element", "move_elementMany", "move_element_to", "move_element_toMany", "nudge_element_to", "nudge_element_toMany", "resize_element", "resize_elementMany", "resize_element_to", "resize_element_toMany", "set_layout", "set_layoutMany", "set_background", "set_backgroundMany", "set_notes", "set_notesMany", "cut_element", "paste_element", "paste_elementMany", "add_picture", "add_pictureMany"] as const;

/** The `tl.so.tilo.slides.Slide` kind at `path`. */
export function slide(ws: tilo.Workspace, path: string): Slide {
  return resourceProxy(ws, path, SURFACE, SLIDE_ACTIONS) as unknown as Slide;
}

/** Element — `tl.so.tilo.slides.Element`. */
export interface Element extends tilo.Resource {
  "74of62le": ElementKind;  // kind
  b47bgsdf: number;  // x
  bh6ok4v5: number;  // y
  "53kv3kr2": number;  // w
  b65jrhk6: number;  // h
  "64oupu64": string;  // body
  qysmcg6z: string;  // src
  "3u7ptmma": string;  // fill
  dsypcewy: string;  // stroke
  lsbzlmm7: number;  // stroke_width
  i6lk64ca: number;  // font_size
  kk6sectq: Align;  // align
  "4g7ap5xd": boolean;  // bold
  haaxnqxs: boolean;  // italic
  pii7xmb6: number;  // radius
  /** Set one paragraph of a rich-text field to what a Markdown text says. */
  editBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editBlockMany(items: ElementEditBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Put a paragraph before the one the address names. */
  insertBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertBlockMany(items: ElementInsertBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remove the paragraph the address names, and the paragraphs it holds. */
  deleteBlock(args: {
    block: string;
  }): Promise<void>;
  /** Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteBlockMany(items: ElementDeleteBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets what an element is painted with, as the inspector's swatches do. */
  set_style(args?: {
    fill?: string;
    stroke?: string;
    stroke_width?: number;
    font_size?: number;
    align?: Align;
    bold?: boolean;
    italic?: boolean;
    radius?: number;
  }): Promise<void>;
  /** Runs `set_style` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_styleMany(items: SetStyleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies the chosen element to the clipboard as the JSON another deck pastes. */
  copy_element(): Promise<void>;
}

/** The actions `Element` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ELEMENT_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "set_style", "set_styleMany", "copy_element"] as const;

/** The `tl.so.tilo.slides.Element` kind at `path`. */
export function element(ws: tilo.Workspace, path: string): Element {
  return resourceProxy(ws, path, SURFACE, ELEMENT_ACTIONS) as unknown as Element;
}

/** DeckFile — `tl.so.tilo.slides.DeckFile`. */
export interface DeckFile extends tilo.Resource {
  mnmdemqn: string;  // title
  iayxmtjm: string;  // palette
  qzspviev: string;  // look
  ajyrzs4l: SlideFile[];  // slides
}

/** The actions `DeckFile` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DECKFILE_ACTIONS = [] as const;

/** The `tl.so.tilo.slides.DeckFile` kind at `path`. */
export function deckFile(ws: tilo.Workspace, path: string): DeckFile {
  return resourceProxy(ws, path, SURFACE, DECKFILE_ACTIONS) as unknown as DeckFile;
}

/** SlideFile — `tl.so.tilo.slides.SlideFile`. */
export interface SlideFile extends tilo.Resource {
  dqtkr5ot: string;  // id
  sqsminna: string;  // layout
  tmaage4k: string;  // background
  e73d2qv7: string;  // notes
  ggcmbhfn: ElementFile[];  // elements
}

/** The actions `SlideFile` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SLIDEFILE_ACTIONS = [] as const;

/** The `tl.so.tilo.slides.SlideFile` kind at `path`. */
export function slideFile(ws: tilo.Workspace, path: string): SlideFile {
  return resourceProxy(ws, path, SURFACE, SLIDEFILE_ACTIONS) as unknown as SlideFile;
}

/** ElementFile — `tl.so.tilo.slides.ElementFile`. */
export interface ElementFile extends tilo.Resource {
  kpm4jhpo: string;  // id
  n34lg7tm: string;  // kind
  hevssfo2: number;  // x
  u55ztccf: number;  // y
  qqtdorgx: number;  // w
  sdpmue2g: number;  // h
  yd6wa63n: string;  // text
  "3jfhhhfc": string;  // src
  a45as5qt: string;  // fill
  lqeh5xoc: string;  // stroke
  rrx5fbi7: number;  // stroke_width
  cbc3umvz: number;  // font_size
  vriuesyu: string;  // align
  xv57dbhp: boolean;  // bold
  zu3zrkv4: boolean;  // italic
  g4nvvgmj: number;  // radius
}

/** The actions `ElementFile` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ELEMENTFILE_ACTIONS = [] as const;

/** The `tl.so.tilo.slides.ElementFile` kind at `path`. */
export function elementFile(ws: tilo.Workspace, path: string): ElementFile {
  return resourceProxy(ws, path, SURFACE, ELEMENTFILE_ACTIONS) as unknown as ElementFile;
}

/** Outline — `tl.so.tilo.slides.Outline`. */
export interface Outline extends tilo.Resource {
  fmwlgt6m: string | null;  // title
  "3iepzseq": string | null;  // body
  tarratvg: Layout | null;  // layout
  uv2g5qve: string | null;  // notes
}

/** The actions `Outline` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OUTLINE_ACTIONS = [] as const;

/** The `tl.so.tilo.slides.Outline` kind at `path`. */
export function outline(ws: tilo.Workspace, path: string): Outline {
  return resourceProxy(ws, path, SURFACE, OUTLINE_ACTIONS) as unknown as Outline;
}

/** Colours — `tl.so.tilo.slides.Colours`. */
export interface Colours extends tilo.Resource {
  i6vzkphp: string;  // accent
  qp7qmrqr: string;  // accent_soft
  e2yv6zxc: string;  // accent_deep
  wvovch6g: string;  // surface
  "3pyvsswg": string;  // surface_dim
  mmxtpccn: string;  // deep
  ch6doguq: string;  // text
  fgu23gq2: string;  // text_dim
  ojgvaj3b: string;  // on_accent
  "23vs3mco": string;  // success
  "4ph7cxqz": string;  // warning
  g56ubagd: string;  // danger
  ne7cvyra: string;  // info
}

/** The actions `Colours` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COLOURS_ACTIONS = [] as const;

/** The `tl.so.tilo.slides.Colours` kind at `path`. */
export function colours(ws: tilo.Workspace, path: string): Colours {
  return resourceProxy(ws, path, SURFACE, COLOURS_ACTIONS) as unknown as Colours;
}

/** Slides — `tl.so.tilo.slides`. */
export interface Slides extends tilo.Resource {
  /** Reads the deck a presentation file holds. */
  read_presentation(args: {
    file: unknown;
  }): Promise<unknown>;
}

/** The actions `Slides` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SLIDES_ACTIONS = ["read_presentation"] as const;

/** The `tl.so.tilo.slides` kind at `path`. */
export function slides(ws: tilo.Workspace, path: string): Slides {
  return resourceProxy(ws, path, SURFACE, SLIDES_ACTIONS) as unknown as Slides;
}

