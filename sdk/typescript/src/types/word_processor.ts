/**
 * Types for Writer (`so.tilo.word-processor`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`word-processor/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.word-processor@b96da4f206ac010b";

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

/** The arguments of `dismiss_notice`. */
export interface DismissNoticeArgs {
}

/** The arguments of `append_paragraph`. */
export interface AppendParagraphArgs {
  text: string;
}

/** The arguments of `replace_body`. */
export interface ReplaceBodyArgs {
  markdown: string;
}

/** The arguments of `replace_text`. */
export interface ReplaceTextArgs {
  find: string;
  replace: string;
  all?: boolean;
}

/** The arguments of `insert_table`. */
export interface InsertTableArgs {
  rows?: number;
  columns?: number;
}

/** The arguments of `add_row`. */
export interface AddRowArgs {
  row?: number;
  table?: number;
}

/** The arguments of `add_column`. */
export interface AddColumnArgs {
  column?: number;
  table?: number;
}

/** The arguments of `delete_row`. */
export interface DeleteRowArgs {
  row: number;
  table?: number;
}

/** The arguments of `delete_column`. */
export interface DeleteColumnArgs {
  column: number;
  table?: number;
}

/** The arguments of `delete_table`. */
export interface DeleteTableArgs {
  table?: number;
}

/** The arguments of `write_cell`. */
export interface WriteCellArgs {
  row: number;
  column: number;
  text: string;
  table?: number;
}

/** The arguments of `set_style_look`. */
export interface SetStyleLookArgs {
  style: string;
  font?: string;
  size?: number;
  bold?: boolean;
  italic?: boolean;
  color?: string;
  align?: string;
  before?: number;
  after?: number;
  indent?: number;
}

/** The arguments of `set_style_ink`. */
export interface SetStyleInkArgs {
  style: string;
  swatch: number;
}

/** The arguments of `new_style`. */
export interface NewStyleArgs {
  name: string;
  from?: string;
}

/** The arguments of `duplicate_style`. */
export interface DuplicateStyleArgs {
  style: string;
}

/** The arguments of `delete_style`. */
export interface DeleteStyleArgs {
  style: string;
}

/** The arguments of `set_page_margins`. */
export interface SetPageMarginsArgs {
  top: number;
  right: number;
  bottom: number;
  left: number;
}

/** The arguments of `set_paper`. */
export interface SetPaperArgs {
  paper: string;
}

/** The arguments of `set_page_words`. */
export interface SetPageWordsArgs {
  where: string;
  words: string;
}

/** The arguments of `generate_toc`. */
export interface GenerateTocArgs {
}

/** The arguments of `set_hyperlink`. */
export interface SetHyperlinkArgs {
  target: string;
  text?: string;
}

/** The arguments of `insert_page_break`. */
export interface InsertPageBreakArgs {
}

/** The arguments of `paste`. */
export interface PasteArgs {
  text?: string;
  markdown?: string;
}

/** The arguments of `replace_selection`. */
export interface ReplaceSelectionArgs {
  text: string;
}

/** The arguments of `place_image`. */
export interface PlaceImageArgs {
  image: unknown;
}

/** The arguments of `delete_image`. */
export interface DeleteImageArgs {
  image: string;
}

/** The arguments of `react`. */
export interface ReactArgs {
  emoji: string;
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

/** Document — `tl.so.tilo.word-processor.Document`. */
export interface Document extends tilo.Resource {
  "76ypz5mr": string;  // title
  myqdqp23: string;  // body
  "6dmyhevh": number;  // margin_top
  "5kuottk3": number;  // margin_right
  u7wfrpxp: number;  // margin_bottom
  tim24jvi: number;  // margin_left
  "5lzglfac": string;  // paper
  yojargfl: string;  // header
  "5ih7vhqz": string;  // footer
  wdffnjds: string;  // started
  "4hldj6md": Record<string, string[]>;  // reactions
  mgawklxl: string;  // notice
  mpu4ekps: string;  // chosen
  po3yh2xl: number;  // chosen_count
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
  /** Starts the document over: a blank page, or the Markdown one of the templates starts with. A document that holds words is replaced all the same — the change is one the person takes back, as every change is, and the templates are named in the File menu beside New. */
  new_document(args?: {
    template?: string;
  }): Promise<Document>;
  /** Takes away the reason the page shows over the writing: the person read it. */
  dismiss_notice(): Promise<void>;
  /** Runs `dismiss_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismiss_noticeMany(items: DismissNoticeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a paragraph of `text` at the end of the document, as Markdown says it. */
  append_paragraph(args: {
    text: string;
  }): Promise<void>;
  /** Runs `append_paragraph` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  append_paragraphMany(items: AppendParagraphArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Replaces the whole body from Markdown: headings, lists, pipe tables, emphasis and links. A picture is not written into the Markdown; it is placed with `place_image`. */
  replace_body(args: {
    markdown: string;
  }): Promise<void>;
  /** Runs `replace_body` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replace_bodyMany(items: ReplaceBodyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** What the document says as Markdown, ready for an agent or a file. */
  read_content(): Promise<string>;
  /** Finds `find` in the document and replaces the first of it, or every one, with `replace`. The words are replaced where they stand, as the editor replaces them: a paragraph's marks, its formatting and the comments anchored in it stay around the words that stay. */
  replace_text(args: {
    find: string;
    replace: string;
    all?: boolean;
  }): Promise<void>;
  /** Runs `replace_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replace_textMany(items: ReplaceTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Inserts an empty table of `rows` by `columns` where the writing ends, its head the first row. */
  insert_table(args?: {
    rows?: number;
    columns?: number;
  }): Promise<void>;
  /** Runs `insert_table` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insert_tableMany(items: InsertTableArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a blank row to a table of the document, under the row `row` counted from its head at 1, or under its last row. Only that table changes, as a row added in the editor's grid does. */
  add_row(args?: {
    row?: number;
    table?: number;
  }): Promise<void>;
  /** Runs `add_row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_rowMany(items: AddRowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a blank column to a table of the document, right of the column `column` counted from its left at 1, or right of its last. */
  add_column(args?: {
    column?: number;
    table?: number;
  }): Promise<void>;
  /** Runs `add_column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_columnMany(items: AddColumnArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a row of a table of the document, counted from its head at 1. */
  delete_row(args: {
    row: number;
    table?: number;
  }): Promise<void>;
  /** Runs `delete_row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_rowMany(items: DeleteRowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a column of a table of the document, counted from its left at 1. */
  delete_column(args: {
    column: number;
    table?: number;
  }): Promise<void>;
  /** Runs `delete_column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_columnMany(items: DeleteColumnArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a table of the document and everything it holds; the words around it stay. */
  delete_table(args?: {
    table?: number;
  }): Promise<void>;
  /** Runs `delete_table` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_tableMany(items: DeleteTableArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes `text` in one cell of a table of the document, at `row` counted from its head at 1 and `column` from its left at 1: that cell's words change and nothing else does, as typing in the editor's grid changes them. */
  write_cell(args: {
    row: number;
    column: number;
    text: string;
    table?: number;
  }): Promise<void>;
  /** Runs `write_cell` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  write_cellMany(items: WriteCellArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Changes how the named style `style` looks: its face (sans, serif or mono), its size in points, whether it is bold or italic, its colour (#rrggbb), how its lines align (left, center, right, justify), and the space before and after it and its indent, in points. What is left out stays as it was. The style is the document's own, so every paragraph set in it changes at once, on every device, and the change is one a person takes back. */
  set_style_look(args: {
    style: string;
    font?: string;
    size?: number;
    bold?: boolean;
    italic?: boolean;
    color?: string;
    align?: string;
    before?: number;
    after?: number;
    indent?: number;
  }): Promise<void>;
  /** Runs `set_style_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_style_lookMany(items: SetStyleLookArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the ink of the named style `style` to one of the inks the page offers, counted from 0 in the order its colour swatches show them: the page's black and greys, then blue, green, ochre, red, violet and pink. */
  set_style_ink(args: {
    style: string;
    swatch: number;
  }): Promise<void>;
  /** Runs `set_style_ink` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_style_inkMany(items: SetStyleInkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a new named style called `name`, looking as the style `from` does: a person sets paragraphs in it and changes its look without touching the styles every document has. */
  new_style(args: {
    name: string;
    from?: string;
  }): Promise<void>;
  /** Runs `new_style` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  new_styleMany(items: NewStyleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a copy of the named style `style`, called '`style` copy', to change apart from it. */
  duplicate_style(args: {
    style: string;
  }): Promise<void>;
  /** Runs `duplicate_style` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  duplicate_styleMany(items: DuplicateStyleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a style the document made: its paragraphs go back to the body's look. The styles every document has (Body, the headings, Quote and List item) stay. */
  delete_style(args: {
    style: string;
  }): Promise<void>;
  /** Runs `delete_style` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_styleMany(items: DeleteStyleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the page's margins, in points from the page's edges. */
  set_page_margins(args: {
    top: number;
    right: number;
    bottom: number;
    left: number;
  }): Promise<void>;
  /** Runs `set_page_margins` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_page_marginsMany(items: SetPageMarginsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the paper the page is written on: A4, Letter or Legal. */
  set_paper(args: {
    paper: string;
  }): Promise<void>;
  /** Runs `set_paper` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_paperMany(items: SetPaperArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the words every page's top or bottom margin carries: `where` is `header` or `footer`. Empty words for the header leave the document's title there. */
  set_page_words(args: {
    where: string;
    words: string;
  }): Promise<void>;
  /** Runs `set_page_words` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_page_wordsMany(items: SetPageWordsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes a table of contents at the head of the document. It is a field, not a copy: the page draws the document's headings into it as they stand, so an entry a person writes or takes away shows there at once. A document without headings is refused: there is nothing to list. */
  generate_toc(): Promise<void>;
  /** Runs `generate_toc` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  generate_tocMany(items: GenerateTocArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives the document's words a link at the end of it, reading `text` or the address itself. */
  set_hyperlink(args: {
    target: string;
    text?: string;
  }): Promise<void>;
  /** Runs `set_hyperlink` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_hyperlinkMany(items: SetHyperlinkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Insert a page break at the end of the writing. */
  insert_page_break(): Promise<void>;
  /** Runs `insert_page_break` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insert_page_breakMany(items: InsertPageBreakArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts `markdown` at the end of the writing, as a paste does. */
  paste(args?: {
    text?: string;
    markdown?: string;
  }): Promise<void>;
  /** Runs `paste` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pasteMany(items: PasteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Replaces the whole body with `text`, as Markdown. */
  replace_selection(args: {
    text: string;
  }): Promise<void>;
  /** Runs `replace_selection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replace_selectionMany(items: ReplaceSelectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes a picture in where the person is writing: the person picks the file, and the page hands the address of its content to the editor, which puts it at the caret. The document keeps the address rather than the picture. */
  place_image(args: {
    image: unknown;
  }): Promise<void>;
  /** Runs `place_image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  place_imageMany(items: PlaceImageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The first picture the document shows, as content another app can take. */
  share_image(): Promise<unknown>;
  /** Deletes the picture whose address is `image`. */
  delete_image(args: {
    image: string;
  }): Promise<void>;
  /** Runs `delete_image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_imageMany(items: DeleteImageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds your reaction with an emoji to the document, or takes it back when you had reacted with it: a press on one of the pills in the bar, or the pick the add button opened. */
  react(args: {
    emoji: string;
  }): Promise<void>;
  /** Runs `react` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reactMany(items: ReactArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Says something about the document, in the thread beside its page. When `at` names a paragraph and `start..end` cover words in it, the remark is anchored there: the editor marks the words and stands the comment's marker in the margin beside them, so it always points at what it was made about. The comment and its anchor are one change. */
  comment_on(args: {
    text: string;
    at?: string;
    start?: number;
    end?: number;
  }): Promise<unknown>;
  /** Opens a file into the document: a Word, OpenDocument, RTF, EPUB or HTML file read by the device's codecs as the Markdown the editor shows, Markdown as it stands, and plain text as one paragraph a line. A file no codec here reads is refused with what it is. Opening a file is one change, which the person takes back as any other. */
  open_document(args: {
    file: unknown;
  }): Promise<Document>;
  /** The document as a Markdown file, named by its title. */
  export_content(): Promise<unknown>;
  /** The document as a plain-text file: its words, a paragraph a line. */
  export_text(): Promise<unknown>;
  /** The document as a Word, OpenDocument, RTF, EPUB or PDF file, named by its title: what a person sends someone who does not write in Tilo. The bytes are the device's codecs'. */
  export_document(args: {
    format: string;
  }): Promise<unknown>;
  /** The document as one HTML page: its headings, lists and paragraphs, and its words. */
  export_html(): Promise<unknown>;
  /** Reads a file and answers what it says, without touching the document: what an agent reads a document through before it decides what to do with it. */
  read_file(args: {
    file: unknown;
  }): Promise<string>;
}

/** The actions `Document` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOCUMENT_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "new_document", "dismiss_notice", "dismiss_noticeMany", "append_paragraph", "append_paragraphMany", "replace_body", "replace_bodyMany", "read_content", "replace_text", "replace_textMany", "insert_table", "insert_tableMany", "add_row", "add_rowMany", "add_column", "add_columnMany", "delete_row", "delete_rowMany", "delete_column", "delete_columnMany", "delete_table", "delete_tableMany", "write_cell", "write_cellMany", "set_style_look", "set_style_lookMany", "set_style_ink", "set_style_inkMany", "new_style", "new_styleMany", "duplicate_style", "duplicate_styleMany", "delete_style", "delete_styleMany", "set_page_margins", "set_page_marginsMany", "set_paper", "set_paperMany", "set_page_words", "set_page_wordsMany", "generate_toc", "generate_tocMany", "set_hyperlink", "set_hyperlinkMany", "insert_page_break", "insert_page_breakMany", "paste", "pasteMany", "replace_selection", "replace_selectionMany", "place_image", "place_imageMany", "share_image", "delete_image", "delete_imageMany", "react", "reactMany", "comment_on", "open_document", "export_content", "export_text", "export_document", "export_html", "read_file"] as const;

/** The `tl.so.tilo.word-processor.Document` kind at `path`. */
export function wordProcessorDocument(ws: tilo.Workspace, path: string): Document {
  return resourceProxy(ws, path, SURFACE, DOCUMENT_ACTIONS) as unknown as Document;
}

/** Writer — `tl.so.tilo.word-processor`. */
export interface WordProcessor extends tilo.Resource {
}

/** The actions `WordProcessor` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORDPROCESSOR_ACTIONS = [] as const;

/** The `tl.so.tilo.word-processor` kind at `path`. */
export function wordProcessor(ws: tilo.Workspace, path: string): WordProcessor {
  return resourceProxy(ws, path, SURFACE, WORDPROCESSOR_ACTIONS) as unknown as WordProcessor;
}

