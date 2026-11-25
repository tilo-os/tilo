/**
 * Types for Browser (`so.tilo.browser`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`browser/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.browser@b138a3bc0cbeae2b";

/** Where a row the address bar offers comes from. One of "bookmark", "visited", "imported", "search". */
export type SuggestionKind = "bookmark" | "visited" | "imported" | "search";

/** The arguments of `remove_bookmark`. */
export interface RemoveBookmarkArgs {
}

/** The arguments of `name_bookmark`. */
export interface NameBookmarkArgs {
  title: string;
}

/** The arguments of `move_bookmark`. */
export interface MoveBookmarkArgs {
  folder: string;
}

/** The arguments of `name_visit`. */
export interface NameVisitArgs {
  title: string;
}

/** The arguments of `forget_visit`. */
export interface ForgetVisitArgs {
}

/** The arguments of `forget_form`. */
export interface ForgetFormArgs {
}

/** The arguments of `delete_address`. */
export interface DeleteAddressArgs {
}

/** The arguments of `open_from_page`. */
export interface OpenFromPageArgs {
  url: string;
  popup: boolean;
}

/** The arguments of `remember_history`. */
export interface RememberHistoryArgs {
  direction: string;
  entries: string;
}

/** The arguments of `remember_fullscreen`. */
export interface RememberFullscreenArgs {
  fullscreen: boolean;
}

/** The arguments of `note_extension_download`. */
export interface NoteExtensionDownloadArgs {
  id: number;
  state: string;
  name: string;
  url: string;
  path: string;
  received: number;
  total: number;
  error: string;
}

/** The arguments of `note_print`. */
export interface NotePrintArgs {
  ok: boolean;
  path: string;
  name: string;
  message: string;
}

/** The arguments of `float_page_video`. */
export interface FloatPageVideoArgs {
  floating?: boolean;
}

/** The arguments of `remember_extensions`. */
export interface RememberExtensionsArgs {
  installed: string;
  message: string;
}

/** The arguments of `note_extension`. */
export interface NoteExtensionArgs {
  ok: boolean;
  kind: string;
  message: string;
  id: string;
}

/** The arguments of `remember_panel`. */
export interface RememberPanelArgs {
  open: boolean;
  kind: string;
  id: string;
  url: string;
  interactive: boolean;
}

/** The arguments of `remember_omnibox`. */
export interface RememberOmniboxArgs {
  id: string;
  name: string;
  text: string;
  rows: string;
}

/** The arguments of `note_extension_updates`. */
export interface NoteExtensionUpdatesArgs {
  checked: number;
  updated: string;
  failed: string;
}

/** The arguments of `remember_credential`. */
export interface RememberCredentialArgs {
  did: string;
  success: boolean;
  message: string;
  forms: string;
  accounts: string;
  capture: string;
  username: string;
  late: boolean;
}

/** The arguments of `went_to`. */
export interface WentToArgs {
  url: string;
}

/** The arguments of `toggle_pause`. */
export interface TogglePauseArgs {
  paused?: boolean;
}

/** The arguments of `bookmark`. */
export interface BookmarkArgs {
  url: string;
  title?: string;
}

/** The arguments of `set_site_protection`. */
export interface SetSiteProtectionArgs {
  origin: string;
  enabled: boolean;
}

/** The arguments of `toggle_site_protection`. */
export interface ToggleSiteProtectionArgs {
  origin: string;
}

/** The arguments of `forget_site_forms`. */
export interface ForgetSiteFormsArgs {
  origin: string;
}

/** The arguments of `forget_all_forms`. */
export interface ForgetAllFormsArgs {
}

/** The arguments of `remember_form`. */
export interface RememberFormArgs {
  origin: string;
  signature: string;
  label: string;
  page: string;
  questions: string[];
  whole: boolean;
}

/** The arguments of `clear_history`. */
export interface ClearHistoryArgs {
}

/** The arguments of `visit`. */
export interface VisitArgs {
  url: string;
  title: string;
  at: string;
}

/** The arguments of `trim_history`. */
export interface TrimHistoryArgs {
  most: number;
}

/** The arguments of `arrived_at`. */
export interface ArrivedAtArgs {
  url: string;
}

/** The arguments of `import_browsing`. */
export interface ImportBrowsingArgs {
  file: unknown;
}

/** The arguments of `import_history`. */
export interface ImportHistoryArgs {
  bundle: string;
}

/** The arguments of `import_bookmarks`. */
export interface ImportBookmarksArgs {
  bundle: string;
}

/** The arguments of `reveal_download`. */
export interface RevealDownloadArgs {
  path: string;
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

/** Bookmark — `tl.so.tilo.browser.Bookmark`. */
export interface Bookmark extends tilo.Resource {
  "44buv5tt": string;  // url
  b3zhsydm: string;  // title
  ehmpzm3r: string;  // folder
  /** Takes a bookmark back. */
  remove_bookmark(): Promise<void>;
  /** Runs `remove_bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_bookmarkMany(items: RemoveBookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a saved page, when the person likes their own name for it better than its title. */
  name_bookmark(args: {
    title: string;
  }): Promise<void>;
  /** Runs `name_bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  name_bookmarkMany(items: NameBookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a bookmark in a folder of the person's library, such as `Work/Reading`, or at the top of the bookmarks when the folder is empty. */
  move_bookmark(args: {
    folder: string;
  }): Promise<void>;
  /** Runs `move_bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_bookmarkMany(items: MoveBookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Bookmark` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BOOKMARK_ACTIONS = ["remove_bookmark", "remove_bookmarkMany", "name_bookmark", "name_bookmarkMany", "move_bookmark", "move_bookmarkMany"] as const;

/** The `tl.so.tilo.browser.Bookmark` kind at `path`. */
export function bookmark(ws: tilo.Workspace, path: string): Bookmark {
  return resourceProxy(ws, path, SURFACE, BOOKMARK_ACTIONS) as unknown as Bookmark;
}

/** Visit — `tl.so.tilo.browser.Visit`. */
export interface Visit extends tilo.Resource {
  k73mxvmp: string;  // url
  qy6vtgjc: string;  // title
  "2yoxl6hx": string;  // last
  eruwucur: number;  // count
  /** Names a visit, when the person likes their own name for a page better than its title. */
  name_visit(args: {
    title: string;
  }): Promise<void>;
  /** Runs `name_visit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  name_visitMany(items: NameVisitArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Forgets one visit. A bookmark of the page stays. */
  forget_visit(): Promise<void>;
  /** Runs `forget_visit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forget_visitMany(items: ForgetVisitArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Visit` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VISIT_ACTIONS = ["name_visit", "name_visitMany", "forget_visit", "forget_visitMany"] as const;

/** The `tl.so.tilo.browser.Visit` kind at `path`. */
export function visit(ws: tilo.Workspace, path: string): Visit {
  return resourceProxy(ws, path, SURFACE, VISIT_ACTIONS) as unknown as Visit;
}

/** ImportedVisit — `tl.so.tilo.browser.ImportedVisit`. */
export interface ImportedVisit extends tilo.Resource {
  my4dhx2s: string;  // url
  "43vnrbh6": string;  // title
  avuwnnrf: string;  // last
  srpuiw6c: number;  // count
}

/** The actions `ImportedVisit` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IMPORTEDVISIT_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.ImportedVisit` kind at `path`. */
export function importedVisit(ws: tilo.Workspace, path: string): ImportedVisit {
  return resourceProxy(ws, path, SURFACE, IMPORTEDVISIT_ACTIONS) as unknown as ImportedVisit;
}

/** Suggestion — `tl.so.tilo.browser.Suggestion`. */
export interface Suggestion extends tilo.Resource {
  zl6ois7x: SuggestionKind;  // kind
  iwoez6ar: string;  // title
  np2bxhf4: string;  // url
  dermimxj: string;  // secondary
}

/** The actions `Suggestion` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SUGGESTION_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.Suggestion` kind at `path`. */
export function suggestion(ws: tilo.Workspace, path: string): Suggestion {
  return resourceProxy(ws, path, SURFACE, SUGGESTION_ACTIONS) as unknown as Suggestion;
}

/** RememberedForm — `tl.so.tilo.browser.RememberedForm`. */
export interface RememberedForm extends tilo.Resource {
  "3wdvc5zt": string;  // origin
  ksw3jnev: string;  // signature
  vdjubgjc: string;  // label
  "6atmgq6f": string[];  // pages
  znuzitns: number;  // seen
  kexnvpi2: string;  // read_at
  jalszv2p: Array<tilo.Entry<Question>>;  // questions
  /** Forgets one form a site asked. */
  forget_form(): Promise<void>;
  /** Runs `forget_form` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forget_formMany(items: ForgetFormArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `RememberedForm` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REMEMBEREDFORM_ACTIONS = ["forget_form", "forget_formMany"] as const;

/** The `tl.so.tilo.browser.RememberedForm` kind at `path`. */
export function rememberedForm(ws: tilo.Workspace, path: string): RememberedForm {
  return resourceProxy(ws, path, SURFACE, REMEMBEREDFORM_ACTIONS) as unknown as RememberedForm;
}

/** Question — `tl.so.tilo.browser.Question`. */
export interface Question extends tilo.Resource {
  "2wlmb5rp": string;  // name
  z4pmfk6p: string;  // answer
  mttccz7q: string;  // label
  yk2bhzup: string[];  // choices
  gmhi7gfy: boolean;  // required
}

/** The actions `Question` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const QUESTION_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.Question` kind at `path`. */
export function question(ws: tilo.Workspace, path: string): Question {
  return resourceProxy(ws, path, SURFACE, QUESTION_ACTIONS) as unknown as Question;
}

/** Address — `tl.so.tilo.browser.Address`. */
export interface Address extends tilo.Resource {
  fnyioh2y: string;  // label
  "3ewzn2z4": string;  // full_name
  n2u44pv5: string;  // given_name
  lgputt7y: string;  // family_name
  plkha23f: string;  // street
  pwnlxkv7: string;  // city
  g7xpip4v: string;  // region
  kj2h5fbf: string;  // postal_code
  iciyfupp: string;  // country
  /** Deletes an address, and stops filling forms with it. */
  delete_address(): Promise<void>;
  /** Runs `delete_address` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_addressMany(items: DeleteAddressArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Address` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ADDRESS_ACTIONS = ["delete_address", "delete_addressMany"] as const;

/** The `tl.so.tilo.browser.Address` kind at `path`. */
export function address(ws: tilo.Workspace, path: string): Address {
  return resourceProxy(ws, path, SURFACE, ADDRESS_ACTIONS) as unknown as Address;
}

/** Page — `tl.so.tilo.browser.Page`. */
export interface Page extends tilo.Resource {
  hf6q2oo4: string;  // url
  tyhunk32: string;  // title
  "4nlkcuks": boolean;  // paused
  s6ryunn5: boolean;  // private
  dedg3fev: Array<tilo.Entry<PageStep>>;  // steps
  mk755sf3: DialogAsk | null;  // dialog
  tveuvbse: MenuAsk | null;  // menu
  eg4w4fow: boolean;  // muted
  zh6wrcvk: PageMedia | null;  // media
  i6yst6vv: boolean;  // pip
  "3emd4v5c": boolean;  // fullscreen
  "252payib": SiteInfo | null;  // site
  nfmtkvs7: Array<tilo.Entry<LoginForm>>;  // login_forms
  jbnzv4tj: Array<tilo.Entry<LoginAccount>>;  // login_accounts
  jz6xwcus: string;  // capture
  wqz44fsx: string;  // login_notice
  ylu3tat4: string;  // print_notice
  a45urcsf: Array<tilo.Entry<PageButton>>;  // buttons
  "3wa3bkzu": Array<tilo.Entry<ExtensionNotice>>;  // notices
  mbgqkffs: Panel | null;  // panel
  osrvi5lh: Omnibox | null;  // omnibox
  ctm6ma5g: Array<tilo.Entry<HistoryEntry>>;  // back
  znzxwmxa: Array<tilo.Entry<HistoryEntry>>;  // forward
  eiqyiw53: boolean;  // loading
  xu2jvlwq: number;  // progress
  "5gspmng7": number;  // zoom_level
  j5emshom: number;  // find_at
  n6y73q7c: number;  // find_count
  cjwvjr5r: string;  // find_text
  /** What the page of `p` says and does, carried to the window that holds it. Each action answers the page's own report as an [`Answer`], and what was done is written into the page's record. Read the page: what it says, line by line, as an agent reads it. A long page is read in parts: a read that stops short names the line to read on from. */
  read_page(args?: {
    from?: number;
    frame?: string;
  }): Promise<Answer>;
  /** What changed on the page since the last read: the lines added and removed, or the whole outline when the document is new. */
  read_page_changes(args?: {
    frame?: string;
  }): Promise<Answer>;
  /** What a click at a point would hit, through overlays and banners, and the element holding it. */
  read_page_element_at(args: {
    x: number;
    y: number;
    frame?: string;
  }): Promise<Answer>;
  /** Wait for the page to go quiet, or until the words it must say are there and the ones it must not say are gone. */
  wait_page_settled(args?: {
    quiet_ms?: number;
    timeout_ms?: number;
    text?: string;
    gone?: string;
    frame?: string;
  }): Promise<Answer>;
  /** The page's console messages and uncaught errors after a sequence number. */
  read_page_console(args?: {
    since?: number;
  }): Promise<Answer>;
  /** The page's network requests after a sequence number: method, address, type, status and failure, never headers, bodies or cookies. */
  read_page_network(args?: {
    since?: number;
  }): Promise<Answer>;
  /** The record of who did what in the page, after the place `since` names. */
  read_page_record(args?: {
    since?: number;
  }): Promise<Answer>;
  /** Every field of the document with what it holds and who set it. A secret is listed by length. */
  read_page_fields(args?: {
    frame?: string;
  }): Promise<Answer>;
  /** The document's forms as what they ask: their questions, the answers they offer, which are required, and the buttons that send them. */
  read_page_forms(args?: {
    frame?: string;
  }): Promise<Answer>;
  /** The page's tables as rows, a long one read in parts. */
  read_page_table(args?: {
    element?: string;
    from?: number;
    frame?: string;
  }): Promise<Answer>;
  /** The outline lines that hold `text`, each with its line number. */
  find_page_text(args: {
    text: string;
    frame?: string;
    frames?: boolean;
  }): Promise<Answer>;
  /** The element the role and name name, and the ref that acts there. */
  find_page_element(args?: {
    role?: string;
    name?: string;
    nth?: number;
    frame?: string;
    frames?: boolean;
  }): Promise<Answer>;
  /** Press the element, as a hand does. */
  click_page(args: {
    element: string;
    frame?: string;
  }): Promise<Answer>;
  /** Move the pointer over an element, or to a point of the page. */
  hover_page(args?: {
    element?: string;
    x?: number;
    y?: number;
  }): Promise<Answer>;
  /** Drag from one element or point to another, in steps. */
  drag_page(args?: {
    from?: string;
    from_x?: number;
    from_y?: number;
    to?: string;
    to_x?: number;
    to_y?: number;
    steps?: number;
  }): Promise<Answer>;
  /** Write `text` into an editable element. */
  fill_page(args: {
    element: string;
    text: string;
    frame?: string;
  }): Promise<Answer>;
  /** Write a secret into a password or card field: it is never kept in the page's record. */
  fill_page_secret(args: {
    element: string;
    text: string;
    frame?: string;
  }): Promise<Answer>;
  /** Choose options of a select. */
  select_page(args: {
    element: string;
    values: string[];
    frame?: string;
  }): Promise<Answer>;
  /** Choose the files of a file input, by absolute path. */
  fill_page_files(args: {
    element: string;
    paths: string[];
    frame?: string;
  }): Promise<Answer>;
  /** Press a key in the page, as `Control+a` or `Enter`. */
  press_page(args: {
    key: string;
    element?: string;
    frame?: string;
  }): Promise<Answer>;
  /** Send the element's form through the page's own validation. */
  submit_page(args: {
    element: string;
    cross_origin?: boolean;
    frame?: string;
  }): Promise<Answer>;
  /** Keeps a page the page opened in a window of its own as a page of this browser, as its window hands it over: a plain new tab, or a popup a window could not show beside the page. */
  open_from_page(args: {
    url: string;
    popup: boolean;
  }): Promise<void>;
  /** Runs `open_from_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_from_pageMany(items: OpenFromPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes the pages the page can go to one way into its record, as its window hands them over: what the menu then offers beside Back and Forward. */
  remember_history(args: {
    direction: string;
    entries: string;
  }): Promise<void>;
  /** Runs `remember_history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remember_historyMany(items: RememberHistoryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Answers a whole form of the page in one ask: each question's answer by its name, through the page's own validation, and the form's first button when `submit` is on. A form is named by the shape `read_page_forms` publishes for it. */
  fill_page_form(args: {
    form: string;
    answers: Record<string, string>;
    submit?: boolean;
    cross_origin?: boolean;
    frame?: string;
  }): Promise<Answer>;
  /** A trusted pointer gesture: press at the first point, move through the rest and release at the last, for drawing on canvases. */
  pointer_page(args: {
    steps: string[];
    button?: string;
  }): Promise<Answer>;
  /** Turn the page's wheel to the top or the bottom, into view, or by a distance. */
  scroll_page(args?: {
    element?: string;
    to?: string;
    by?: number;
    into_view?: string;
    frame?: string;
  }): Promise<Answer>;
  /** A picture of the page, or of one element's box, as a PNG. */
  capture_page(args?: {
    element?: string;
    max_width?: number;
    frame?: string;
  }): Promise<Answer>;
  /** Open an address in the page's own window: a site named without its scheme opens as one (`example.org`), and words that name no address open the search engine's answer to them. */
  navigate(args: {
    url: string;
  }): Promise<Answer>;
  /** What Enter in the address does: what follows an extension's keyword goes to the extension, which hears of it and goes where it sends the page; anything else is an address to go to. */
  submit_address(args: {
    typed: string;
  }): Promise<Answer>;
  /** The person chose one of the rows under the address: the extension hears what it stands for. */
  choose_omnibox(args: {
    content: string;
  }): Promise<Answer>;
  /** Go back to the page before this one. */
  back(): Promise<Answer>;
  /** Go forward to the page this one followed. */
  forward(): Promise<Answer>;
  /** Load the page again. */
  reload(): Promise<Answer>;
  /** Stop loading the page. */
  stop_loading(): Promise<Answer>;
  /** Leave the page's fullscreen: the engine stops presenting the page, and its window shows the page with its own controls again at once, since the person pressed the way out. The engine's own report of it follows into the record. */
  leave_fullscreen(): Promise<Answer>;
  /** Writes whether the page is in fullscreen into its record, as its window hands it over: a video or a slide deck that filled the screen, or left it. */
  remember_fullscreen(args: {
    fullscreen: boolean;
  }): Promise<void>;
  /** Runs `remember_fullscreen` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remember_fullscreenMany(items: RememberFullscreenArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Show the page's article without its clutter. */
  reader(args: {
    enabled: boolean;
  }): Promise<Answer>;
  /** Show the page at `level` zoom, where 1 is actual size. */
  zoom(args: {
    level: number;
  }): Promise<Answer>;
  /** Find words in the page: the window's search is the page's find, and the words are kept so the way on to the next match and back to the previous ask for the same ones. No words takes the find away: the marks go, and the page is left as it was before they were typed. */
  find_in_page(args: {
    text: string;
    forward?: boolean;
    next?: boolean;
  }): Promise<Answer>;
  /** Close the find bar's search. */
  stop_finding(): Promise<Answer>;
  /** Answer a page's own question: an alert, a confirmation or a prompt. */
  answer_page_dialog(args: {
    id: number;
    accept: boolean;
    text?: string;
  }): Promise<Answer>;
  /** Reads what the page waits on into its window's record: the question it asks, the menu it showed, and how far it has loaded. The window calls it when it opens the page and after each answer; nothing here asks the page anything of its own. */
  refresh_page(): Promise<Answer>;
  /** Lets a site use what it asked for (its camera, microphone, location, notifications). The window raises it as the Browser's own call when the site asks: the person allows it on the system's card, which names the Browser and the site, and an always-allow lets this site have it from then on. Declined, or the page gone, the site is told no. */
  allow_permission(args: {
    id: number;
    origin: string;
    wants: string;
  }): Promise<Answer>;
  /** Saves the file a page offers. The window raises it as the Browser's own call when the page offers one, and the person allows it on the system's card. */
  save_download(args: {
    id: number;
    name: string;
    url: string;
  }): Promise<Answer>;
  /** Turns down the file a page offers: nothing is saved, and the page is told so. */
  discard_download(args: {
    id: number;
    name?: string;
  }): Promise<Answer>;
  /** Downloads what an address serves into the person's Downloads, as a job of the Browser's own that goes on when the window closes. A page's window raises it for a download the person saved there, naming the handle its request waits under, and again for one the person retried, naming the job it ended as (`again`); a person or agent names an address. */
  download(args: {
    url: string;
    name?: string;
    request?: string;
    again?: string;
  }): Promise<Answer>;
  /** Keeps where a download an extension started stands, as the page's engine, which saves it, tells: the Browser's one list shows it with the rest. The window raises it as the download moves; a person, agent or script never does. */
  note_extension_download(args: {
    id: number;
    state: string;
    name: string;
    url: string;
    path: string;
    received: number;
    total: number;
    error: string;
  }): Promise<void>;
  /** Runs `note_extension_download` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  note_extension_downloadMany(items: NoteExtensionDownloadArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a download that failed or was cancelled again, from the beginning, as signed in as the page was: the page's window hands over what a request for the address carries from it now, and the download keeps the new job. */
  retry_download(args: {
    d: Download;
  }): Promise<Answer>;
  /** Opens the engine's developer tools for this page: its console, its elements and its network. */
  developer_tools(): Promise<Answer>;
  /** Writes the page as a PDF and hands it to Print Centre, where the person chooses the printer and how it prints. The engine answers with the file once it has written it; a page with nothing open has nothing to print. */
  print_page(): Promise<Answer>;
  /** Hands the PDF the engine wrote to Print Centre, through its own action, so the person chooses the printer and how it prints there; the copy the engine wrote goes. The window raises it once the engine is done; a person, agent or script never does. */
  note_print(args: {
    ok: boolean;
    path: string;
    name: string;
    message: string;
  }): Promise<void>;
  /** Runs `note_print` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  note_printMany(items: NotePrintArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Silences the page's sound, or lets it play again. */
  mute(args?: {
    muted?: boolean;
  }): Promise<Answer>;
  /** Floats the page's video above its window as picture-in-picture, so it plays while the person works elsewhere, or puts it back where the page draws it. */
  float_page_video(args?: {
    floating?: boolean;
  }): Promise<void>;
  /** Runs `float_page_video` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  float_page_videoMany(items: FloatPageVideoArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the page where it can go back to. The engine answers with the pages before this one, which the page's record holds once its window reads them again. */
  read_back_history(): Promise<Answer>;
  /** Asks the page where it can go forward to, the answer kept like [`read_back_history`]'s. */
  read_forward_history(): Promise<Answer>;
  /** Goes to one page of the page's own history, by the entry its reading named. */
  go_history_entry(args: {
    entry: number;
  }): Promise<Answer>;
  /** Reads the site's connection, its certificate, the requests tracker protection blocked and what the site may ask for, into the page's record. */
  read_site(): Promise<Answer>;
  /** Clears this site's storage, cookies and remembered forms, including cookies it shares with its subdomains, and reloads it. */
  clear_site_data(): Promise<Answer>;
  /** Blocks what the site may ask for (its camera, microphone, location and notifications). */
  block_site_permissions(): Promise<Answer>;
  /** Lets the site ask again for what it was refused. */
  ask_site_permissions_again(): Promise<Answer>;
  /** Reads the login forms this page asks for. */
  discover_logins(): Promise<Answer>;
  /** Lists the logins the workspace's vault keeps for this site, by name. */
  list_saved_logins(): Promise<Answer>;
  /** Takes what the person typed into a login form, so it can be saved for this site. The password stays in the engine until saving hands it to the vault, and never crosses to the app. */
  capture_login(args: {
    f: LoginForm;
  }): Promise<Answer>;
  /** Saves the capture the engine took in the workspace's vault, as a login of this site the Browser keeps. */
  save_login(): Promise<Answer>;
  /** Fills a form of this page with a login the vault keeps for this site. A login the Browser saved itself fills at once; any other is filled once its person allowed the Browser that one login. */
  fill_saved_login(args: {
    f: LoginForm;
    a: LoginAccount;
  }): Promise<Answer>;
  /** Replaces a login the vault keeps with the capture the engine took. */
  update_saved_login(args: {
    f: LoginForm;
    a: LoginAccount;
  }): Promise<Answer>;
  /** Forgets a login of this site from the vault: one the Browser saved, or one it may fill. */
  delete_saved_login(args: {
    a: LoginAccount;
  }): Promise<Answer>;
  /** Turns down the login the engine offers to save. */
  cancel_login(): Promise<Answer>;
  /** Reads the addresses the engine fills forms with. */
  discover_addresses(): Promise<Answer>;
  /** Fills a form of this page with a saved address, through the engine. */
  fill_saved_address(args: {
    f: LoginForm;
    a: Address;
  }): Promise<Answer>;
  /** Reads the extensions installed on this device into the Browser's record. */
  list_extensions(): Promise<Answer>;
  /** Installs the extension package the person chose, a .crx signed by its publisher or a plain .zip: the engine verifies a signed package, unpacks it under its own identifier, and lists it. The person picks the package with the system's own chooser, as any app's file. */
  install_extension(args: {
    file: unknown;
  }): Promise<Answer>;
  /** Installs the Chrome Web Store's item `said` (a store page's address, or the extension's id): its package comes from the store's update service, is verified, and is turned on as the store installs one. */
  install_store_extension(args: {
    said: string;
  }): Promise<Answer>;
  /** Asks the Chrome Web Store for newer versions of the store extensions installed here and installs them: they are there the next time a page opens, and are loaded now. */
  update_extensions(): Promise<Answer>;
  /** Turns an installed extension on, or off, in the running window: Chromium loads and unloads it where the page stands, so nothing waits for a window restart. */
  enable_extension(args: {
    e: Extension;
    on?: boolean;
  }): Promise<Answer>;
  /** Forgets an extension installed on this device: its folder and everything it may do. */
  remove_extension(args: {
    e: Extension;
  }): Promise<Answer>;
  /** Presses one of the page's extension buttons: the engine runs the extension's action, or opens the page it offers. */
  click_extension(args: {
    b: PageButton;
  }): Promise<Answer>;
  /** Presses one of the buttons a notification an extension showed offers. */
  choose_extension_notice(args: {
    n: ExtensionNotice;
    button: string;
  }): Promise<Answer>;
  /** Takes a notification an extension showed away without pressing anything. */
  dismiss_extension_notice(args: {
    n: ExtensionNotice;
  }): Promise<Answer>;
  /** Writes the extensions a page's window listed into the Browser's record. The window raises it; a person, agent or script never does. */
  remember_extensions(args: {
    installed: string;
    message: string;
  }): Promise<void>;
  /** Runs `remember_extensions` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remember_extensionsMany(items: RememberExtensionsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes what installing, turning on or removing an extension came to, in the person's words. The window raises it; a person, agent or script never does. */
  note_extension(args: {
    ok: boolean;
    kind: string;
    message: string;
    id: string;
  }): Promise<void>;
  /** Runs `note_extension` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  note_extensionMany(items: NoteExtensionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the extension for its side panel beside the page: its own page says what the panel shows, and the page's window shows it there. */
  open_side_panel(args: {
    e: Extension;
  }): Promise<Answer>;
  /** Closes the page the page's window showed beside or over this one: an extension's side panel, the page it asked the person to sign in on, or a popup a script opened. */
  close_panel(): Promise<Answer>;
  /** Writes the page the page's window shows beside this one into its record, or takes it away. The window raises it; a person, agent or script never does. */
  remember_panel(args: {
    open: boolean;
    kind: string;
    id: string;
    url: string;
    interactive: boolean;
  }): Promise<void>;
  /** Runs `remember_panel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remember_panelMany(items: RememberPanelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes the extension the address addresses, and the rows it offers, into the page's record; an empty identifier takes them away, since the address no longer addresses one. The window raises it; a person, agent or script never does. */
  remember_omnibox(args: {
    id: string;
    name: string;
    text: string;
    rows: string;
  }): Promise<void>;
  /** Runs `remember_omnibox` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remember_omniboxMany(items: RememberOmniboxArgs[]): Promise<tilo.ActionAnswer[]>;
  /** What the person typed in the page's address: when it starts with an extension's keyword and a space, what follows is the extension's, which hears of each change. */
  typed_address(args: {
    typed: string;
  }): Promise<Answer>;
  /** The person left the address without Enter: the extension hears of it. */
  address_left(): Promise<Answer>;
  /** The person pressed Enter, or chose one of the rows: the extension hears what it was given. */
  enter_omnibox(args: {
    text: string;
  }): Promise<Answer>;
  /** Writes what asking the Chrome Web Store for newer versions came to, in the person's words. The window raises it; a person, agent or script never does. */
  note_extension_updates(args: {
    checked: number;
    updated: string;
    failed: string;
  }): Promise<void>;
  /** Runs `note_extension_updates` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  note_extension_updatesMany(items: NoteExtensionUpdatesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes what a login action came to into the page's record: the forms the engine found, the vault's logins for the site, the capture the engine took, and their own words. The window raises it; a person, agent or script never does. */
  remember_credential(args: {
    did: string;
    success: boolean;
    message: string;
    forms: string;
    accounts: string;
    capture: string;
    username: string;
    late: boolean;
  }): Promise<void>;
  /** Runs `remember_credential` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remember_credentialMany(items: RememberCredentialArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Tells the engine which sites tracker protection is off for, as the Browser's own set says: the engine keeps the same list beside its filters. */
  sync_site_exceptions(): Promise<Answer>;
  /** Gives an extension the more access it asked for. The window raises it as the Browser's own call, and the person allows it on the system's card, which says what the extension could then do. */
  allow_extension_access(args: {
    id: number;
    extension: string;
    could: string;
  }): Promise<Answer>;
  /** Signs in to a site that asks for a user name and a password. The window raises it as the Browser's own call; the person types both on the system's card, on this device, and the password goes to the site alone: never into this action's arguments, its record or history. */
  sign_in(args: {
    id: number;
    asker: string;
    site: string;
    realm: string;
  }): Promise<Answer>;
  /** Turns down a site's sign-in: nothing is sent, and the server shows its own page instead. */
  cancel_sign_in(args: {
    id: number;
    asker: string;
  }): Promise<Answer>;
  /** Lets what the page sends leave it: a form, a navigation or a request kept until the person says so. The window raises it as the Browser's own call, and the person allows it on the system's card, which says what leaves and where. */
  let_leave(args: {
    id: number;
    origin: string;
    method: string;
    leaving: string;
  }): Promise<Answer>;
  /** Takes a row of the page's own menu, by the command the engine gave it. */
  context_menu_action(args: {
    id: number;
    command: number;
  }): Promise<Answer>;
  /** Fills a form of the page with a saved address, question by question, without sending it. The form is named by the shape its reading publishes (`RememberedForm.signature`, or the signature `read_page_forms` answers), and the page's own validation still decides whether it could be sent. */
  fill_address(args: {
    form: string;
    a: Address;
  }): Promise<Answer>;
  /** The page went somewhere: where it is now is written on the page, and the visit joins the person's history unless the page's window paused it. */
  went_to(args: {
    url: string;
  }): Promise<void>;
  /** Runs `went_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  went_toMany(items: WentToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Pauses or resumes the person's history in a page's window. */
  toggle_pause(args?: {
    paused?: boolean;
  }): Promise<void>;
  /** Runs `toggle_pause` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_pauseMany(items: TogglePauseArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The page `p` does what `request` (JSON, as the window's page reads it) says, and answers the page's own report. What was done is written into the page's record; a call that could not be made answers with its words. Every named page action below is this one, written out. */
  ask_page(args: {
    request: string;
  }): Promise<Answer>;
}

/** The actions `Page` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGE_ACTIONS = ["read_page", "read_page_changes", "read_page_element_at", "wait_page_settled", "read_page_console", "read_page_network", "read_page_record", "read_page_fields", "read_page_forms", "read_page_table", "find_page_text", "find_page_element", "click_page", "hover_page", "drag_page", "fill_page", "fill_page_secret", "select_page", "fill_page_files", "press_page", "submit_page", "open_from_page", "open_from_pageMany", "remember_history", "remember_historyMany", "fill_page_form", "pointer_page", "scroll_page", "capture_page", "navigate", "submit_address", "choose_omnibox", "back", "forward", "reload", "stop_loading", "leave_fullscreen", "remember_fullscreen", "remember_fullscreenMany", "reader", "zoom", "find_in_page", "stop_finding", "answer_page_dialog", "refresh_page", "allow_permission", "save_download", "discard_download", "download", "note_extension_download", "note_extension_downloadMany", "retry_download", "developer_tools", "print_page", "note_print", "note_printMany", "mute", "float_page_video", "float_page_videoMany", "read_back_history", "read_forward_history", "go_history_entry", "read_site", "clear_site_data", "block_site_permissions", "ask_site_permissions_again", "discover_logins", "list_saved_logins", "capture_login", "save_login", "fill_saved_login", "update_saved_login", "delete_saved_login", "cancel_login", "discover_addresses", "fill_saved_address", "list_extensions", "install_extension", "install_store_extension", "update_extensions", "enable_extension", "remove_extension", "click_extension", "choose_extension_notice", "dismiss_extension_notice", "remember_extensions", "remember_extensionsMany", "note_extension", "note_extensionMany", "open_side_panel", "close_panel", "remember_panel", "remember_panelMany", "remember_omnibox", "remember_omniboxMany", "typed_address", "address_left", "enter_omnibox", "note_extension_updates", "note_extension_updatesMany", "remember_credential", "remember_credentialMany", "sync_site_exceptions", "allow_extension_access", "sign_in", "cancel_sign_in", "let_leave", "context_menu_action", "fill_address", "went_to", "went_toMany", "toggle_pause", "toggle_pauseMany", "ask_page"] as const;

/** The `tl.so.tilo.browser.Page` kind at `path`. */
export function page(ws: tilo.Workspace, path: string): Page {
  return resourceProxy(ws, path, SURFACE, PAGE_ACTIONS) as unknown as Page;
}

/** PageStep — `tl.so.tilo.browser.PageStep`. */
export interface PageStep extends tilo.Resource {
  cpjp36fw: number;  // at
  "4mcq24cu": string | null;  // actor
  g4lc2ulh: string;  // device
  ouuwq4an: string;  // device_name
  "7qikcw5o": string;  // operation
  olgrtknj: boolean;  // ok
  "2mfaldtw": string;  // code
  grbghvzm: string;  // message
  vlqrueqe: string;  // url
  x3xrjcbr: string;  // to_url
  ta3jd45o: string;  // role
  kmz5bb2s: string;  // element
  g67lnb6k: string | null;  // text
  ws5snl25: string[] | null;  // values
}

/** The actions `PageStep` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGESTEP_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.PageStep` kind at `path`. */
export function pageStep(ws: tilo.Workspace, path: string): PageStep {
  return resourceProxy(ws, path, SURFACE, PAGESTEP_ACTIONS) as unknown as PageStep;
}

/** DialogAsk — `tl.so.tilo.browser.DialogAsk`. */
export interface DialogAsk extends tilo.Resource {
  trchguwk: number;  // request
  plkldhkl: string;  // kind
  ruephqmr: string;  // origin
  asepg6kx: string;  // message
  "4v2f3ua3": string;  // default_text
}

/** The actions `DialogAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DIALOGASK_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.DialogAsk` kind at `path`. */
export function dialogAsk(ws: tilo.Workspace, path: string): DialogAsk {
  return resourceProxy(ws, path, SURFACE, DIALOGASK_ACTIONS) as unknown as DialogAsk;
}

/** MenuAsk — `tl.so.tilo.browser.MenuAsk`. */
export interface MenuAsk extends tilo.Resource {
  hjqv4o64: number;  // request
  f2gbwl6v: string;  // link
  "6b3vpys4": MenuRow[];  // items
}

/** The actions `MenuAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MENUASK_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.MenuAsk` kind at `path`. */
export function menuAsk(ws: tilo.Workspace, path: string): MenuAsk {
  return resourceProxy(ws, path, SURFACE, MENUASK_ACTIONS) as unknown as MenuAsk;
}

/** MenuRow — `tl.so.tilo.browser.MenuRow`. */
export interface MenuRow extends tilo.Resource {
  kybyncey: number;  // command
  cgfpo6b7: string;  // label
  hn5vxavq: boolean;  // disabled
  qmekhn2b: boolean;  // checked
  "67scpsna": boolean;  // separator
}

/** The actions `MenuRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MENUROW_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.MenuRow` kind at `path`. */
export function menuRow(ws: tilo.Workspace, path: string): MenuRow {
  return resourceProxy(ws, path, SURFACE, MENUROW_ACTIONS) as unknown as MenuRow;
}

/** PageMedia — `tl.so.tilo.browser.PageMedia`. */
export interface PageMedia extends tilo.Resource {
  "3rpnjayb": string;  // element
  f3bbhirh: string;  // label
}

/** The actions `PageMedia` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGEMEDIA_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.PageMedia` kind at `path`. */
export function pageMedia(ws: tilo.Workspace, path: string): PageMedia {
  return resourceProxy(ws, path, SURFACE, PAGEMEDIA_ACTIONS) as unknown as PageMedia;
}

/** Download — `tl.so.tilo.browser.Download`. */
export interface Download extends tilo.Resource {
  z4gzfvzw: string;  // url
  wz6wi2p3: string;  // name
  mhf455xb: string;  // job
  "5jny2jyp": DownloadJob | null;  // seen
  inchqje5: number;  // engine
  /** Stops a download that is still going: what it saved so far is removed. */
  cancel_download(): Promise<Answer>;
}

/** The actions `Download` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOWNLOAD_ACTIONS = ["cancel_download"] as const;

/** The `tl.so.tilo.browser.Download` kind at `path`. */
export function download(ws: tilo.Workspace, path: string): Download {
  return resourceProxy(ws, path, SURFACE, DOWNLOAD_ACTIONS) as unknown as Download;
}

/** DownloadSaved — `tl.so.tilo.browser.DownloadSaved`. */
export interface DownloadSaved extends tilo.Resource {
  xig3oj7a: string;  // path
  as5zgbcl: string;  // name
}

/** The actions `DownloadSaved` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOWNLOADSAVED_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.DownloadSaved` kind at `path`. */
export function downloadSaved(ws: tilo.Workspace, path: string): DownloadSaved {
  return resourceProxy(ws, path, SURFACE, DOWNLOADSAVED_ACTIONS) as unknown as DownloadSaved;
}

/** DownloadJob — `tl.so.tilo.browser.DownloadJob`. */
export interface DownloadJob extends tilo.Resource {
  q5gkqwpu: string;  // state
  sr3wciux: number;  // done
  r5unqai3: number;  // total
  fmdfhyux: string;  // path
  ba6qps55: string;  // name
  zmyv6bxj: string;  // error
}

/** The actions `DownloadJob` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOWNLOADJOB_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.DownloadJob` kind at `path`. */
export function downloadJob(ws: tilo.Workspace, path: string): DownloadJob {
  return resourceProxy(ws, path, SURFACE, DOWNLOADJOB_ACTIONS) as unknown as DownloadJob;
}

/** DownloadAsk — `tl.so.tilo.browser.DownloadAsk`. */
export interface DownloadAsk extends tilo.Resource {
  "3dy7bucf": string;  // url
  ova5zveb: string;  // name
  e7uolvpg: string;  // request
}

/** The actions `DownloadAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOWNLOADASK_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.DownloadAsk` kind at `path`. */
export function downloadAsk(ws: tilo.Workspace, path: string): DownloadAsk {
  return resourceProxy(ws, path, SURFACE, DOWNLOADASK_ACTIONS) as unknown as DownloadAsk;
}

/** SiteInfo — `tl.so.tilo.browser.SiteInfo`. */
export interface SiteInfo extends tilo.Resource {
  hrkhpkbd: string;  // origin
  scje2sv3: number;  // generation
  lyopffwi: number;  // blocked
  az2v2pr3: boolean;  // protected
  ageqt2tb: string;  // security
  i3vtfymr: number;  // certificate
  zarq7l6o: string;  // issuer
  "5nsqo5b5": string;  // subject
  gsqr2kkr: string[];  // permissions
  eygtkatk: boolean;  // can_reset
}

/** The actions `SiteInfo` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SITEINFO_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.SiteInfo` kind at `path`. */
export function siteInfo(ws: tilo.Workspace, path: string): SiteInfo {
  return resourceProxy(ws, path, SURFACE, SITEINFO_ACTIONS) as unknown as SiteInfo;
}

/** HistoryEntry — `tl.so.tilo.browser.HistoryEntry`. */
export interface HistoryEntry extends tilo.Resource {
  eyk6uw4k: number;  // entry
  "2chmpqlt": string;  // url
  uugq5btr: string;  // title
}

/** The actions `HistoryEntry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HISTORYENTRY_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.HistoryEntry` kind at `path`. */
export function historyEntry(ws: tilo.Workspace, path: string): HistoryEntry {
  return resourceProxy(ws, path, SURFACE, HISTORYENTRY_ACTIONS) as unknown as HistoryEntry;
}

/** PageButton — `tl.so.tilo.browser.PageButton`. */
export interface PageButton extends tilo.Resource {
  t25qin6v: string;  // ext
  pigtj7mb: string;  // label
  dizhfbps: string;  // badge
  ugbnd25j: boolean;  // enabled
  "2wovoiro": boolean;  // popup
}

/** The actions `PageButton` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGEBUTTON_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.PageButton` kind at `path`. */
export function pageButton(ws: tilo.Workspace, path: string): PageButton {
  return resourceProxy(ws, path, SURFACE, PAGEBUTTON_ACTIONS) as unknown as PageButton;
}

/** ExtensionNotice — `tl.so.tilo.browser.ExtensionNotice`. */
export interface ExtensionNotice extends tilo.Resource {
  bmmxddfi: string;  // ext
  "7wjg3mlt": string;  // notification
  sqkziu2x: string;  // title
  h27qgqgy: string;  // message
  c3idm5kp: string[];  // buttons
}

/** The actions `ExtensionNotice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EXTENSIONNOTICE_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.ExtensionNotice` kind at `path`. */
export function extensionNotice(ws: tilo.Workspace, path: string): ExtensionNotice {
  return resourceProxy(ws, path, SURFACE, EXTENSIONNOTICE_ACTIONS) as unknown as ExtensionNotice;
}

/** Panel — `tl.so.tilo.browser.Panel`. */
export interface Panel extends tilo.Resource {
  "52ms3sap": string;  // kind
  sgn42dz6: string;  // ext
  uzhylkwp: string;  // url
  "6vgotm2u": boolean;  // interactive
}

/** The actions `Panel` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PANEL_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.Panel` kind at `path`. */
export function panel(ws: tilo.Workspace, path: string): Panel {
  return resourceProxy(ws, path, SURFACE, PANEL_ACTIONS) as unknown as Panel;
}

/** Omnibox — `tl.so.tilo.browser.Omnibox`. */
export interface Omnibox extends tilo.Resource {
  yf5tqm24: string;  // ext
  h6pi6u6y: string;  // name
  tzxibbua: string;  // text
  oaxzyzwj: OmniboxRow[];  // rows
}

/** The actions `Omnibox` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OMNIBOX_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.Omnibox` kind at `path`. */
export function omnibox(ws: tilo.Workspace, path: string): Omnibox {
  return resourceProxy(ws, path, SURFACE, OMNIBOX_ACTIONS) as unknown as Omnibox;
}

/** OmniboxRow — `tl.so.tilo.browser.OmniboxRow`. */
export interface OmniboxRow extends tilo.Resource {
  wdpe6rh5: string;  // title
  d662kmwa: string;  // content
}

/** The actions `OmniboxRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OMNIBOXROW_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.OmniboxRow` kind at `path`. */
export function omniboxRow(ws: tilo.Workspace, path: string): OmniboxRow {
  return resourceProxy(ws, path, SURFACE, OMNIBOXROW_ACTIONS) as unknown as OmniboxRow;
}

/** Addressed — `tl.so.tilo.browser.Addressed`. */
export interface Addressed extends tilo.Resource {
  mkrajvka: string;  // ext
  "7sy22uee": string;  // name
  mcydjsg5: string;  // text
}

/** The actions `Addressed` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ADDRESSED_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.Addressed` kind at `path`. */
export function addressed(ws: tilo.Workspace, path: string): Addressed {
  return resourceProxy(ws, path, SURFACE, ADDRESSED_ACTIONS) as unknown as Addressed;
}

/** Extension — `tl.so.tilo.browser.Extension`. */
export interface Extension extends tilo.Resource {
  iymjskx5: string;  // key
  vhst373v: string;  // name
  "7b7zbela": string;  // version
  v7jkztuy: number;  // manifest_version
  iaycfmmk: string;  // description
  kkeg7wgf: string;  // permissions
  "372chbvw": boolean;  // enabled
  pqaac6ny: string;  // keyword
  amvgds6o: boolean;  // side_panel
}

/** The actions `Extension` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EXTENSION_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.Extension` kind at `path`. */
export function extension(ws: tilo.Workspace, path: string): Extension {
  return resourceProxy(ws, path, SURFACE, EXTENSION_ACTIONS) as unknown as Extension;
}

/** LoginForm — `tl.so.tilo.browser.LoginForm`. */
export interface LoginForm extends tilo.Resource {
  hdxbxfcl: string;  // form
  enz3azev: string;  // username
  kqqay3gr: string;  // kind
}

/** The actions `LoginForm` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LOGINFORM_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.LoginForm` kind at `path`. */
export function loginForm(ws: tilo.Workspace, path: string): LoginForm {
  return resourceProxy(ws, path, SURFACE, LOGINFORM_ACTIONS) as unknown as LoginForm;
}

/** LoginAccount — `tl.so.tilo.browser.LoginAccount`. */
export interface LoginAccount extends tilo.Resource {
  "53raewop": string;  // account
  "5oqresy2": string;  // username
  qpgkf2v5: string;  // title
  ultqehya: boolean;  // own
}

/** The actions `LoginAccount` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LOGINACCOUNT_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.LoginAccount` kind at `path`. */
export function loginAccount(ws: tilo.Workspace, path: string): LoginAccount {
  return resourceProxy(ws, path, SURFACE, LOGINACCOUNT_ACTIONS) as unknown as LoginAccount;
}

/** Ask — `tl.so.tilo.browser.Ask`. */
export interface Ask extends tilo.Resource {
  fqvhxdyr: string;  // op
  hfpnr6ua: string | null;  // element
  tpz54oe5: string | null;  // text
  "5vdpb6hg": number | null;  // from
  wf32vfhg: number | null;  // by
  q76mm3tr: string | null;  // drag_from
  byeflqne: number | null;  // drag_steps
  hxjdnlwy: string | null;  // frame
  "4udfayix": boolean;  // frames
  txp5lrru: number | null;  // x
  bqydqtva: number | null;  // y
  wbhz4hh6: number | null;  // from_x
  iuschp5g: number | null;  // from_y
  "6i5bwl43": number | null;  // to_x
  hrquaspl: number | null;  // to_y
  "5ka7kek5": string | null;  // role
  davmrr4h: string | null;  // name
  dmxbh4fm: number | null;  // nth
  ao4elp2a: string | null;  // key
  jmhhkxjz: string | null;  // url
  i75qjot6: string | null;  // job
  oerj5bdj: string | null;  // file
  xic2qsit: string | null;  // item
  "5px2ripq": string | null;  // extension
  d7deek3g: string | null;  // rest
  xtjgu4pr: string | null;  // notification
  lqvkggvx: number | null;  // level
  zroiv7hs: string[] | null;  // values
  ntpmcicu: Record<string, string> | null;  // answers
  zoqmh2dz: boolean;  // muted
  l3gmhwrx: string | null;  // direction
  pucrebly: number | null;  // entry
  "4vqdzqut": string | null;  // doing
  fbtgqfhx: string[] | null;  // origins
  ymm5jh2h: string | null;  // origin
  wdlbrp6r: string | null;  // credential
  x6klrsfn: string | null;  // capture
  o345kv2f: string | null;  // account
  cirolhk7: Address | null;  // address
  "7gcvts2u": number | null;  // generation
  ky26jv4a: string[] | null;  // paths
  iabkjt2r: string[] | null;  // steps
  zh65uqqk: number | null;  // since
  ersdn54t: number | null;  // quiet_ms
  bgflyuoe: number | null;  // timeout_ms
  hjfilxdg: string | null;  // gone
  btx3fn7s: boolean;  // cross_origin
  zf6y6r3k: boolean;  // secret
  arhpr4t7: boolean;  // submit
  h6fxerpb: string | null;  // form
  cnvdpt2t: string | null;  // submit_button
  asha7lz3: string | null;  // to
  jcrv3nzs: string | null;  // into_view
  qit2hijp: string | null;  // button
  "2zpkv6jg": boolean;  // enabled
  fggdqfhy: number | null;  // command
  sizwtymn: number | null;  // max_width
  htce2xyp: number | null;  // id
  wbwndzhw: string | null;  // asker
  nqkenrjw: string | null;  // username
  ns2mfzwr: string | null;  // password
  qvvu7twk: boolean;  // typed
  vwi6rjsr: boolean;  // accept
  szlyr265: boolean;  // allow
  acxwp3qh: boolean;  // forward
  m6sqrzav: boolean;  // next
}

/** The actions `Ask` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ASK_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.Ask` kind at `path`. */
export function ask(ws: tilo.Workspace, path: string): Ask {
  return resourceProxy(ws, path, SURFACE, ASK_ACTIONS) as unknown as Ask;
}

/** Answer — `tl.so.tilo.browser.Answer`. */
export interface Answer extends tilo.Resource {
  "2ubdeo5l": boolean;  // ok
  pv4ylrjw: string;  // code
  d7izzogo: string;  // message
  ta6htvhh: string;  // url
  xzex263o: string;  // title
  htwzro7c: string;  // navigation
  vptvuat4: boolean;  // loading
  vvykamug: string;  // outline
  i2okiki6: string;  // element
  mmtsrl7r: number;  // line
  p6uplgxr: string;  // role
  "7zlc443q": string;  // name
  "2ibb6fxi": string;  // frame
  cxsstht7: string;  // image
}

/** The actions `Answer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.Answer` kind at `path`. */
export function answer(ws: tilo.Workspace, path: string): Answer {
  return resourceProxy(ws, path, SURFACE, ANSWER_ACTIONS) as unknown as Answer;
}

/** Browser — `tl.so.tilo.browser.Browser`. */
export interface Browser extends tilo.Resource {
  v73p6lyt: boolean;  // adblock
  "4mcclqi3": boolean;  // clear_on_exit
  zhpmqsvs: string;  // search_engine
  ezeliuhm: string;  // homepage
  "54pfa4xp": string;  // trouble
  b7avacvg: string[];  // sites_without_protection
  "5kcrev3j": Array<tilo.Entry<Bookmark>>;  // bookmarks
  qfwsjkyv: Array<tilo.Entry<Visit>>;  // visits
  dlo6rb3l: Array<tilo.Entry<ImportedVisit>>;  // imported_visits
  m7eeditk: Array<tilo.Entry<RememberedForm>>;  // forms
  "2zm7y6e5": Array<tilo.Entry<Address>>;  // addresses
  zx4a3bhv: Array<tilo.Entry<Page>>;  // pages
  z3z5vks2: boolean;  // addresses_enabled
  nskkfe4o: Array<tilo.Entry<Extension>>;  // extensions
  "3gaxygjd": Array<tilo.Entry<Download>>;  // downloads
  uq4ctcke: string;  // extension_notice
  /** Opens a page in a window of its own. Every window of the app is one page; what it shows, its address and its own back and forward are the system's. */
  new_page(): Promise<Page>;
  /** Opens a window that browses privately: nothing it reads, signs in to or downloads joins what this device keeps of the person. */
  new_private_page(): Promise<Page>;
  /** Opens an address in a page of its own, or asks the search engine for what was typed when it is not one. */
  go(args: {
    typed: string;
  }): Promise<Page>;
  /** Opens a link of a scheme this browser claims: the address in a page of its own, or the search a `web+search:` link asks for. A shell hands a link here whether or not a window of the app is open, and what this returns opens in the window. */
  open_link(args: {
    link: string;
  }): Promise<Page>;
  /** Saves a page for later. A page is bookmarked by its address alone: bookmarking it twice keeps one bookmark, and no title given falls back to the address. */
  bookmark(args: {
    url: string;
    title?: string;
  }): Promise<void>;
  /** Runs `bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  bookmarkMany(items: BookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns tracker protection off for one site, or back on. A site is named by its origin, so turning it off for `https://example.org` covers every page of it. */
  set_site_protection(args: {
    origin: string;
    enabled: boolean;
  }): Promise<void>;
  /** Runs `set_site_protection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_site_protectionMany(items: SetSiteProtectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns tracker protection off for this site, or back on, as the page's shield does. */
  toggle_site_protection(args: {
    origin: string;
  }): Promise<void>;
  /** Runs `toggle_site_protection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_site_protectionMany(items: ToggleSiteProtectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Forgets every form a site asked. */
  forget_site_forms(args: {
    origin: string;
  }): Promise<void>;
  /** Runs `forget_site_forms` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forget_site_formsMany(items: ForgetSiteFormsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Forgets every form every site asked. */
  forget_all_forms(): Promise<void>;
  /** Runs `forget_all_forms` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forget_all_formsMany(items: ForgetAllFormsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remembers what a page's forms ask, as its window reads them: the shape of each form and of its questions, never what a field held. A form read here already is read again: what it asks now replaces what it asked before. `questions` is one question a line, its fields a tab apart: name, the kind of answer, the label, `true` when it must be answered, and the answers it offers, a `|` between them. */
  remember_form(args: {
    origin: string;
    signature: string;
    label: string;
    page: string;
    questions: string[];
    whole: boolean;
  }): Promise<void>;
  /** Runs `remember_form` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remember_formMany(items: RememberFormArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds an address for the forms the person fills. */
  add_address(): Promise<Address>;
  /** Clears the browsing history. Bookmarks and remembered forms stay. */
  clear_history(): Promise<void>;
  /** Runs `clear_history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_historyMany(items: ClearHistoryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a page a window opened to the person's history, counting it when it is there already. A page that opens itself is a visit like any other. */
  visit(args: {
    url: string;
    title: string;
    at: string;
  }): Promise<void>;
  /** Runs `visit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  visitMany(items: VisitArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps the person's own visits to their newest `most`, forgetting the oldest: the history stays within what a person could read, as every other list the system keeps does. */
  trim_history(args: {
    most: number;
  }): Promise<void>;
  /** Runs `trim_history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  trim_historyMany(items: TrimHistoryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes in a page a window arrived at: it joins the person's history. */
  arrived_at(args: {
    url: string;
  }): Promise<void>;
  /** Runs `arrived_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  arrived_atMany(items: ArrivedAtArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes in the browsing data another browser holds, from the bundle this app's own import and `tilo migrate` hand over: its history, its bookmarks, or both. */
  import_browsing(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `import_browsing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_browsingMany(items: ImportBrowsingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Imports the pages another browser's history holds, from the bundle this app's own import and `tilo migrate` hand over: a JSON text of `{'browser': '...', 'visits': [{'url': '...', 'title': '...', 'last': 1760000000, 'count': 1}]}`. Importing the same bundle twice keeps one set of pages. */
  import_history(args: {
    bundle: string;
  }): Promise<void>;
  /** Runs `import_history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_historyMany(items: ImportHistoryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Imports the bookmarks another browser holds, from the bundle this app's own import hands over: the `ImportBundle` JSON with bookmark chunks (`{'bookmarks': [{'url': '...', 'title': '...', 'folder': 'Work'}]}`). Importing the same file twice keeps one bookmark of each page. */
  import_bookmarks(args: {
    bundle: string;
  }): Promise<void>;
  /** Runs `import_bookmarks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_bookmarksMany(items: ImportBookmarksArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Browser` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BROWSER_ACTIONS = ["new_page", "new_private_page", "go", "open_link", "bookmark", "bookmarkMany", "set_site_protection", "set_site_protectionMany", "toggle_site_protection", "toggle_site_protectionMany", "forget_site_forms", "forget_site_formsMany", "forget_all_forms", "forget_all_formsMany", "remember_form", "remember_formMany", "add_address", "clear_history", "clear_historyMany", "visit", "visitMany", "trim_history", "trim_historyMany", "arrived_at", "arrived_atMany", "import_browsing", "import_browsingMany", "import_history", "import_historyMany", "import_bookmarks", "import_bookmarksMany"] as const;

/** The `tl.so.tilo.browser.Browser` kind at `path`. */
export function browser(ws: tilo.Workspace, path: string): Browser {
  return resourceProxy(ws, path, SURFACE, BROWSER_ACTIONS) as unknown as Browser;
}

/** Browser — `tl.so.tilo.browser`. */
export interface TiloBrowser extends tilo.Resource {
  /** Stops every download still going, from every window. */
  cancel_all_downloads(): Promise<Answer>;
  /** Opens a saved download in the app that opens its kind, through the system's one open. */
  open_download(args: {
    name: string;
    path: string;
  }): Promise<void>;
  /** Shows where a saved download stands in Files, with the file marked: the system's one place for what a window saved. */
  reveal_download(args: {
    path: string;
  }): Promise<void>;
  /** Runs `reveal_download` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reveal_downloadMany(items: RevealDownloadArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `TiloBrowser` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOBROWSER_ACTIONS = ["cancel_all_downloads", "open_download", "reveal_download", "reveal_downloadMany"] as const;

/** The `tl.so.tilo.browser` kind at `path`. */
export function tiloBrowser(ws: tilo.Workspace, path: string): TiloBrowser {
  return resourceProxy(ws, path, SURFACE, TILOBROWSER_ACTIONS) as unknown as TiloBrowser;
}

