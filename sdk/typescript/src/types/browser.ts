/**
 * Types for Browser (`so.tilo.browser`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Bookmark` is a `Resource` of the app's `tl.so.tilo.browser.Bookmark`, made by its factory with the
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
export const SURFACE = "so.tilo.browser@fa16aa89e2c41823";

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

/** The arguments of `update_download`. */
export interface UpdateDownloadArgs {
  id: number;
  state: string;
  name: string;
  url: string;
  path: string;
  retryable: boolean;
  received: number;
  total: number;
  error: string;
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
  dedg3fev: Array<tilo.Entry<PageStep>>;  // steps
  mk755sf3: DialogAsk | null;  // dialog
  tveuvbse: MenuAsk | null;  // menu
  o2occ235: Array<tilo.Entry<Download>>;  // downloads
  eg4w4fow: boolean;  // muted
  "252payib": SiteInfo | null;  // site
  nfmtkvs7: Array<tilo.Entry<LoginForm>>;  // login_forms
  jbnzv4tj: Array<tilo.Entry<LoginAccount>>;  // login_accounts
  jz6xwcus: string;  // capture
  wqz44fsx: string;  // login_notice
  ctm6ma5g: Array<tilo.Entry<HistoryEntry>>;  // back
  znzxwmxa: Array<tilo.Entry<HistoryEntry>>;  // forward
  eiqyiw53: boolean;  // loading
  xu2jvlwq: number;  // progress
  "5gspmng7": number;  // zoom_level
  j5emshom: number;  // find_at
  n6y73q7c: number;  // find_count
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
  /** Writes what the page last said of one of its downloads into the page's record, as its window hands it over while the transfer moves. The window raises it, never a person, agent or script. */
  update_download(args: {
    id: number;
    state: string;
    name: string;
    url: string;
    path: string;
    retryable: boolean;
    received: number;
    total: number;
    error: string;
  }): Promise<void>;
  /** Runs `update_download` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  update_downloadMany(items: UpdateDownloadArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps a page the page opened in a window of its own as a page of this browser, as its window hands it over: a popup a site sized, or a plain new tab. */
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
  /** Open an address in the page's own window. */
  navigate(args: {
    url: string;
  }): Promise<Answer>;
  /** Go back to the page before this one. */
  back(): Promise<Answer>;
  /** Go forward to the page this one followed. */
  forward(): Promise<Answer>;
  /** Load the page again. */
  reload(): Promise<Answer>;
  /** Stop loading the page. */
  stop_loading(): Promise<Answer>;
  /** Show the page's article without its clutter. */
  reader(args: {
    enabled: boolean;
  }): Promise<Answer>;
  /** Show the page at `level` zoom, where 1 is actual size. */
  zoom(args: {
    level: number;
  }): Promise<Answer>;
  /** Find words in the page, as the find bar does. */
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
  /** Saves what an address serves into the browser's downloads. */
  download(args: {
    url: string;
  }): Promise<Answer>;
  /** Stops a download that is still going. */
  cancel_download(args: {
    d: Download;
  }): Promise<Answer>;
  /** Starts a failed download again from the beginning. */
  retry_download(args: {
    d: Download;
  }): Promise<Answer>;
  /** Stops every download this page has going. */
  cancel_all_downloads(): Promise<Answer>;
  /** Opens the engine's developer tools for this page: its console, its elements and its network. */
  developer_tools(): Promise<Answer>;
  /** Silences the page's sound, or lets it play again. */
  mute(args?: {
    muted?: boolean;
  }): Promise<Answer>;
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
  /** Reads the login forms this page asks for, through the engine's own credential store. */
  discover_logins(): Promise<Answer>;
  /** Lists the logins the engine keeps for this site. */
  list_saved_logins(): Promise<Answer>;
  /** Takes what the person typed into a login form, so it can be saved for this site. The password stays in the engine's credential store and never crosses to the app. */
  capture_login(args: {
    f: LoginForm;
  }): Promise<Answer>;
  /** Saves the capture the engine took for this site. */
  save_login(): Promise<Answer>;
  /** Fills a form of this page with a login the engine keeps. */
  fill_saved_login(args: {
    f: LoginForm;
    a: LoginAccount;
  }): Promise<Answer>;
  /** Replaces a saved login with the capture the engine took. */
  update_saved_login(args: {
    f: LoginForm;
    a: LoginAccount;
  }): Promise<Answer>;
  /** Forgets a login the engine keeps for this site. */
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
  /** Writes what the engine's credential store answered into the page's record: the forms and accounts it listed, the capture it took, and its own words. The window raises it; a person, agent or script never does. */
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
export const PAGE_ACTIONS = ["read_page", "read_page_changes", "read_page_element_at", "wait_page_settled", "read_page_console", "read_page_network", "read_page_record", "read_page_fields", "read_page_forms", "read_page_table", "find_page_text", "find_page_element", "click_page", "hover_page", "drag_page", "fill_page", "fill_page_secret", "select_page", "fill_page_files", "press_page", "submit_page", "update_download", "update_downloadMany", "open_from_page", "open_from_pageMany", "remember_history", "remember_historyMany", "fill_page_form", "pointer_page", "scroll_page", "capture_page", "navigate", "back", "forward", "reload", "stop_loading", "reader", "zoom", "find_in_page", "stop_finding", "answer_page_dialog", "refresh_page", "allow_permission", "save_download", "discard_download", "download", "cancel_download", "retry_download", "cancel_all_downloads", "developer_tools", "mute", "read_back_history", "read_forward_history", "go_history_entry", "read_site", "clear_site_data", "block_site_permissions", "ask_site_permissions_again", "discover_logins", "list_saved_logins", "capture_login", "save_login", "fill_saved_login", "update_saved_login", "delete_saved_login", "cancel_login", "discover_addresses", "fill_saved_address", "remember_credential", "remember_credentialMany", "sync_site_exceptions", "allow_extension_access", "sign_in", "cancel_sign_in", "let_leave", "context_menu_action", "fill_address", "went_to", "went_toMany", "toggle_pause", "toggle_pauseMany", "ask_page"] as const;

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

/** Download — `tl.so.tilo.browser.Download`. */
export interface Download extends tilo.Resource {
  anfisgsq: number;  // transfer
  in525zqp: string;  // state
  wz6wi2p3: string;  // name
  z4gzfvzw: string;  // url
  feka5n4y: string;  // path
  kubpqqew: boolean;  // retryable
  cokrfftg: number;  // received
  pbavpdyk: number;  // total
  wfaksaa6: string;  // error
}

/** The actions `Download` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOWNLOAD_ACTIONS = [] as const;

/** The `tl.so.tilo.browser.Download` kind at `path`. */
export function download(ws: tilo.Workspace, path: string): Download {
  return resourceProxy(ws, path, SURFACE, DOWNLOAD_ACTIONS) as unknown as Download;
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
  /** Opens a page in a window of its own. Every window of the app is one page; what it shows, its address and its own back and forward are the system's. */
  new_page(): Promise<Page>;
  /** Opens an address in a page of its own, or asks the search engine for what was typed when it is not one. */
  go(args: {
    typed: string;
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
export const BROWSER_ACTIONS = ["new_page", "go", "bookmark", "bookmarkMany", "set_site_protection", "set_site_protectionMany", "toggle_site_protection", "toggle_site_protectionMany", "forget_site_forms", "forget_site_formsMany", "forget_all_forms", "forget_all_formsMany", "remember_form", "remember_formMany", "add_address", "clear_history", "clear_historyMany", "visit", "visitMany", "trim_history", "trim_historyMany", "arrived_at", "arrived_atMany", "import_browsing", "import_browsingMany", "import_history", "import_historyMany", "import_bookmarks", "import_bookmarksMany"] as const;

/** The `tl.so.tilo.browser.Browser` kind at `path`. */
export function browser(ws: tilo.Workspace, path: string): Browser {
  return resourceProxy(ws, path, SURFACE, BROWSER_ACTIONS) as unknown as Browser;
}

/** Browser — `tl.so.tilo.browser`. */
export interface TiloBrowser extends tilo.Resource {
  /** Opens a saved download in the app that opens its kind, through the system's one open. */
  open_download(args: {
    name: string;
    path: string;
  }): Promise<void>;
}

/** The actions `TiloBrowser` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOBROWSER_ACTIONS = ["open_download"] as const;

/** The `tl.so.tilo.browser` kind at `path`. */
export function tiloBrowser(ws: tilo.Workspace, path: string): TiloBrowser {
  return resourceProxy(ws, path, SURFACE, TILOBROWSER_ACTIONS) as unknown as TiloBrowser;
}

