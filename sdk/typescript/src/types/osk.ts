/**
 * Types for On-Screen Keyboard (`so.tilo.osk`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`osk/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.osk@4286fa454a61820c";

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

/** Typing — `tl.so.tilo.osk.Typing`. */
export interface Typing extends tilo.Resource {
  m6tp67xz: number;  // field
  xgezzsnq: string;  // word
  kurpxlhy: string;  // previous
  tduplb36: boolean;  // swiped
  nrfgg7zc: string[];  // chips
  oli6hbi7: string;  // typed
  kydmz4at: string;  // tail
  yir2po5h: number;  // shift
  "64zexfdz": string;  // page
  lfpmstnh: string;  // query
  "2tfifihw": number;  // ctrl
  x6mgoh2l: number;  // alt
  qbj7o6ze: boolean;  // sentence
  "25ultnxs": boolean;  // parked
  "3mckgl7e": string[];  // forms
  jf436se6: string;  // forms_for
}

/** The actions `Typing` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TYPING_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.Typing` kind at `path`. */
export function typing(ws: tilo.Workspace, path: string): Typing {
  return resourceProxy(ws, path, SURFACE, TYPING_ACTIONS) as unknown as Typing;
}

/** Taught — `tl.so.tilo.osk.Taught`. */
export interface Taught extends tilo.Resource {
  ecaq5x2q: string;  // context
  yybwwgav: string;  // word
  htei6x6l: number;  // count
}

/** The actions `Taught` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TAUGHT_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.Taught` kind at `path`. */
export function taught(ws: tilo.Workspace, path: string): Taught {
  return resourceProxy(ws, path, SURFACE, TAUGHT_ACTIONS) as unknown as Taught;
}

/** SuggestAsk — `tl.so.tilo.osk.SuggestAsk`. */
export interface SuggestAsk extends tilo.Resource {
  u7ua3uwj: string;  // word
  x64bsvli: string;  // previous
  i2l5ongd: string;  // language
  xyp2g7uo: string[];  // personal
  btxnh6dk: boolean;  // personalization
  lbar2w74: Taught[];  // learned
  "3jxblazz": string[];  // kept
  oqjnl4cg: number;  // limit
}

/** The actions `SuggestAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SUGGESTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.SuggestAsk` kind at `path`. */
export function suggestAsk(ws: tilo.Workspace, path: string): SuggestAsk {
  return resourceProxy(ws, path, SURFACE, SUGGESTASK_ACTIONS) as unknown as SuggestAsk;
}

/** Offer — `tl.so.tilo.osk.Offer`. */
export interface Offer extends tilo.Resource {
  sspaaf2o: string;  // word
  ym3zbzik: number;  // score
  o6ytofpt: boolean;  // personal
  zjsvehiu: boolean;  // emoji
}

/** The actions `Offer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OFFER_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.Offer` kind at `path`. */
export function offer(ws: tilo.Workspace, path: string): Offer {
  return resourceProxy(ws, path, SURFACE, OFFER_ACTIONS) as unknown as Offer;
}

/** Suggested — `tl.so.tilo.osk.Suggested`. */
export interface Suggested extends tilo.Resource {
  "7qgx3bze": Offer[];  // completions
  txmmjsyz: Offer[];  // next
}

/** The actions `Suggested` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SUGGESTED_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.Suggested` kind at `path`. */
export function suggested(ws: tilo.Workspace, path: string): Suggested {
  return resourceProxy(ws, path, SURFACE, SUGGESTED_ACTIONS) as unknown as Suggested;
}

/** CorrectAsk — `tl.so.tilo.osk.CorrectAsk`. */
export interface CorrectAsk extends tilo.Resource {
  avrdwkwu: string;  // word
  xh7nuttw: string;  // language
  rdht4mzn: string;  // layout
  ip6lajuh: string[];  // rows
  nyu5nrfl: string[];  // kept
}

/** The actions `CorrectAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CORRECTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.CorrectAsk` kind at `path`. */
export function correctAsk(ws: tilo.Workspace, path: string): CorrectAsk {
  return resourceProxy(ws, path, SURFACE, CORRECTASK_ACTIONS) as unknown as CorrectAsk;
}

/** Corrected — `tl.so.tilo.osk.Corrected`. */
export interface Corrected extends tilo.Resource {
  "37fhak2q": string | null;  // correction
}

/** The actions `Corrected` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CORRECTED_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.Corrected` kind at `path`. */
export function corrected(ws: tilo.Workspace, path: string): Corrected {
  return resourceProxy(ws, path, SURFACE, CORRECTED_ACTIONS) as unknown as Corrected;
}

/** SwipeAsk — `tl.so.tilo.osk.SwipeAsk`. */
export interface SwipeAsk extends tilo.Resource {
  vkdpd5r7: number[];  // path
  qhrbdy4q: number[];  // area
  p6t77uy7: string[];  // rows
  udzknqo5: string;  // layout
  yhplpjdc: string;  // language
  pxdf2zfl: string;  // shift
  mocqi47p: number;  // limit
  a2o5rdfx: boolean;  // judged
  mvvgppgc: number;  // aspect
}

/** The actions `SwipeAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SWIPEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.SwipeAsk` kind at `path`. */
export function swipeAsk(ws: tilo.Workspace, path: string): SwipeAsk {
  return resourceProxy(ws, path, SURFACE, SWIPEASK_ACTIONS) as unknown as SwipeAsk;
}

/** Candidate — `tl.so.tilo.osk.Candidate`. */
export interface Candidate extends tilo.Resource {
  zsr74dix: string;  // word
  mybbluwr: number;  // score
}

/** The actions `Candidate` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CANDIDATE_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.Candidate` kind at `path`. */
export function candidate(ws: tilo.Workspace, path: string): Candidate {
  return resourceProxy(ws, path, SURFACE, CANDIDATE_ACTIONS) as unknown as Candidate;
}

/** Swiped — `tl.so.tilo.osk.Swiped`. */
export interface Swiped extends tilo.Resource {
  veop4uz2: boolean;  // swipe
  "56adnkcz": Candidate[];  // candidates
}

/** The actions `Swiped` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SWIPED_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.Swiped` kind at `path`. */
export function swiped(ws: tilo.Workspace, path: string): Swiped {
  return resourceProxy(ws, path, SURFACE, SWIPED_ACTIONS) as unknown as Swiped;
}

/** TouchAsk — `tl.so.tilo.osk.TouchAsk`. */
export interface TouchAsk extends tilo.Resource {
  ilse75gf: number[];  // point
  ayis4zfh: number[];  // area
  "25lqxg2k": string;  // layout
  yjrtaqh2: string[];  // rows
  j7nbu66f: string;  // previous
  lb64yv45: string;  // word
  "6ofwzyyn": string;  // language
  tfzfpcem: unknown[];  // offsets
  xl5fo2at: boolean;  // personalization
}

/** The actions `TouchAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TOUCHASK_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.TouchAsk` kind at `path`. */
export function touchAsk(ws: tilo.Workspace, path: string): TouchAsk {
  return resourceProxy(ws, path, SURFACE, TOUCHASK_ACTIONS) as unknown as TouchAsk;
}

/** Landed — `tl.so.tilo.osk.Landed`. */
export interface Landed extends tilo.Resource {
  gmprye6b: string;  // ch
  "7e2ewqea": number;  // dx
  xxbw2p3b: number;  // dy
  hidcbudr: number;  // samples
}

/** The actions `Landed` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LANDED_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.Landed` kind at `path`. */
export function landed(ws: tilo.Workspace, path: string): Landed {
  return resourceProxy(ws, path, SURFACE, LANDED_ACTIONS) as unknown as Landed;
}

/** Touched — `tl.so.tilo.osk.Touched`. */
export interface Touched extends tilo.Resource {
  uk2nkbvx: number[] | null;  // key
  gf3rqhji: Landed | null;  // taught
}

/** The actions `Touched` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TOUCHED_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.Touched` kind at `path`. */
export function touched(ws: tilo.Workspace, path: string): Touched {
  return resourceProxy(ws, path, SURFACE, TOUCHED_ACTIONS) as unknown as Touched;
}

/** AccentsAsk — `tl.so.tilo.osk.AccentsAsk`. */
export interface AccentsAsk extends tilo.Resource {
  erau6win: string[];  // keys
  wbtiabii: string;  // language
  "6we4suoh": string;  // shift
}

/** The actions `AccentsAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCENTSASK_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.AccentsAsk` kind at `path`. */
export function accentsAsk(ws: tilo.Workspace, path: string): AccentsAsk {
  return resourceProxy(ws, path, SURFACE, ACCENTSASK_ACTIONS) as unknown as AccentsAsk;
}

/** Accents — `tl.so.tilo.osk.Accents`. */
export interface Accents extends tilo.Resource {
  cha7zqyr: Record<string, string[]>;  // forms
}

/** The actions `Accents` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCENTS_ACTIONS = [] as const;

/** The `tl.so.tilo.osk.Accents` kind at `path`. */
export function accents(ws: tilo.Workspace, path: string): Accents {
  return resourceProxy(ws, path, SURFACE, ACCENTS_ACTIONS) as unknown as Accents;
}

/** On-Screen Keyboard — `tl.so.tilo.osk`. */
export interface Osk extends tilo.Resource {
  /** A key of the pad was pressed: `index` counts the keys shown, `at` is where the finger was, as fractions of the pad's box. */
  pressed(args: {
    k: unknown;
    typing: Typing;
    index: number;
    at: number[];
  }): Promise<Typing>;
  /** A long press chose one of a key's forms. */
  held(args: {
    k: unknown;
    typing: Typing;
    choice: string[];
  }): Promise<Typing>;
  /** The letter a tap meant. Where this person's taps tend to land and which letter may come next move the edge between two letters, never the keys drawn; what the tap taught of where they land is kept. Only in a field of words, and never a secret's. */
  meant(args: {
    k: unknown;
    t: Typing;
    key: string;
    at: number[];
  }): Promise<string>;
  /** Some word of the language goes on past `stem'`: `don'`, `l'`, `aujourd'`. */
  continues(args: {
    k: unknown;
    t: Typing;
    stem: string;
  }): Promise<boolean>;
  /** A letter typed: composed into the word where the field takes words, else typed as it is. With Ctrl or Alt held, the key is a chord: pressed, not typed. */
  letter(args: {
    k: unknown;
    typing: Typing;
    key: string;
  }): Promise<Typing>;
  /** A key that is no letter: one of the keys' own, an editing key, or a mark. */
  press(args: {
    k: unknown;
    typing: Typing;
    key: string;
  }): Promise<Typing>;
  /** The composing word goes into the field with `after` behind it: corrected first when `correct` asks and the field takes corrections, and learned from. With no word composing, `after` is typed alone. */
  commit(args: {
    k: unknown;
    typing: Typing;
    after: string;
    correct: boolean;
  }): Promise<Typing>;
  /** What the device would correct `word` to, or the word itself. */
  corrected(args: {
    k: unknown;
    t: Typing;
    word: string;
  }): Promise<string>;
  /** Backspace: right after a correction it types the word back as the person typed it, and keeps it so; in a word it takes its last letter (a swiped word goes whole); elsewhere it reaches the field. */
  backspace(args: {
    k: unknown;
    typing: Typing;
  }): Promise<Typing>;
  /** The correction just made is taken back: the word goes in again as typed, and is never corrected again. */
  undo_correction(args: {
    k: unknown;
    typing: Typing;
  }): Promise<Typing>;
  /** A key pressed with Ctrl or Alt held. A modifier latched for one key lets go after it. */
  chord(args: {
    k: unknown;
    typing: Typing;
    name: string;
  }): Promise<Typing>;
  /** The space bar was dragged: the caret moves by `steps` letters, after the word goes in. */
  caret(args: {
    k: unknown;
    typing: Typing;
    steps: number;
  }): Promise<Typing>;
  /** A finger crossed the letters: the word it spelled composes, cased for Shift, and the other words it may have meant stand in the bar. A swiped word is never corrected. */
  swiped(args: {
    k: unknown;
    typing: Typing;
    path: number[];
    aspect: number;
  }): Promise<Typing>;
  /** What the bar offers now: the words the one composing may become, or the ones that may come next, the person's own first, then up to two emoji. */
  offer(args: {
    k: unknown;
    typing: Typing;
  }): Promise<Typing>;
  /** One of the bar's offers was taken. */
  took(args: {
    k: unknown;
    typing: Typing;
    chip: string;
  }): Promise<Typing>;
  /** An emoji from the picker goes in after the word. */
  picked(args: {
    k: unknown;
    typing: Typing;
    emoji: string;
  }): Promise<Typing>;
  /** One of the strip's commands. */
  tool(args: {
    k: unknown;
    typing: Typing;
    name: string;
  }): Promise<Typing>;
  /** The forms a long press offers on every letter, asked of the device in the language typed. */
  with_forms(args: {
    k: unknown;
    typing: Typing;
  }): Promise<Typing>;
}

/** The actions `Osk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OSK_ACTIONS = ["pressed", "held", "meant", "continues", "letter", "press", "commit", "corrected", "backspace", "undo_correction", "chord", "caret", "swiped", "offer", "took", "picked", "tool", "with_forms"] as const;

/** The `tl.so.tilo.osk` kind at `path`. */
export function osk(ws: tilo.Workspace, path: string): Osk {
  return resourceProxy(ws, path, SURFACE, OSK_ACTIONS) as unknown as Osk;
}

