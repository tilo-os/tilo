/**
 * Types for Print Centre (`so.tilo.print-centre`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`print-centre/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.print-centre@e9a8937d08949cf7";

/** The arguments of `turn_page`. */
export interface TurnPageArgs {
}

/** The arguments of `recognize`. */
export interface RecognizeArgs {
}

/** The arguments of `scan_ended`. */
export interface ScanEndedArgs {
  j: unknown;
}

/** The arguments of `print_ended`. */
export interface PrintEndedArgs {
  j: unknown;
}

/** The arguments of `ask_options`. */
export interface AskOptionsArgs {
}

/** The arguments of `absorb_document`. */
export interface AbsorbDocumentArgs {
  file: unknown;
}

/** The arguments of `print_file`. */
export interface PrintFileArgs {
  file: unknown;
}

/** The arguments of `leave_handed`. */
export interface LeaveHandedArgs {
}

/** The arguments of `send_to_printer`. */
export interface SendToPrinterArgs {
}

/** The arguments of `scan_page`. */
export interface ScanPageArgs {
}

/** The arguments of `crop_page`. */
export interface CropPageArgs {
  page: ScannedPage;
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** The arguments of `remove_page`. */
export interface RemovePageArgs {
  page: ScannedPage;
}

/** The arguments of `new_scanned_document`. */
export interface NewScannedDocumentArgs {
}

/** The arguments of `move_page`. */
export interface MovePageArgs {
  page: ScannedPage;
  order: number;
  swapped?: number;
}

/** The arguments of `choose_printer`. */
export interface ChoosePrinterArgs {
  queue: string;
}

/** The arguments of `set_two_sided`. */
export interface SetTwoSidedArgs {
  two_sided: boolean;
}

/** The arguments of `set_print_option`. */
export interface SetPrintOptionArgs {
  option: string;
  value: string;
}

/** The arguments of `set_resolution`. */
export interface SetResolutionArgs {
  dpi: number;
}

/** The arguments of `set_scan_colour`. */
export interface SetScanColourArgs {
  colour: string;
}

/** The arguments of `set_scan_source`. */
export interface SetScanSourceArgs {
  source: string;
}

/** The arguments of `set_ocr_language`. */
export interface SetOcrLanguageArgs {
  language: string;
}

/** The arguments of `set_scanner`. */
export interface SetScannerArgs {
  device_name: string;
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

/** Defaults — `tl.so.tilo.print-centre.Defaults`. */
export interface Defaults extends tilo.Resource {
  coq3zrug: boolean;  // two_sided
  "26zbe323": number;  // scan_resolution
  wsba2l5p: string;  // scan_colour
  vzgtuxuj: string;  // scan_source
  warqkuxs: string;  // ocr_language
}

/** The actions `Defaults` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DEFAULTS_ACTIONS = [] as const;

/** The `tl.so.tilo.print-centre.Defaults` kind at `path`. */
export function defaults(ws: tilo.Workspace, path: string): Defaults {
  return resourceProxy(ws, path, SURFACE, DEFAULTS_ACTIONS) as unknown as Defaults;
}

/** ScannedPage — `tl.so.tilo.print-centre.ScannedPage`. */
export interface ScannedPage extends tilo.Resource {
  zphe26i4: unknown;  // file
  "67hjm6gx": string;  // text
  t2im62jd: number;  // order
  /** Turns a scanned page a quarter clockwise. */
  turn_page(): Promise<void>;
  /** Runs `turn_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  turn_pageMany(items: TurnPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the text off a scanned page, in the person's language. */
  recognize(): Promise<void>;
  /** Runs `recognize` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recognizeMany(items: RecognizeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies a scanned page's text. */
  copy_text(): Promise<void>;
  /** The scanned page as a file to save. */
  page_to_save(): Promise<unknown>;
}

/** The actions `ScannedPage` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANNEDPAGE_ACTIONS = ["turn_page", "turn_pageMany", "recognize", "recognizeMany", "copy_text", "page_to_save"] as const;

/** The `tl.so.tilo.print-centre.ScannedPage` kind at `path`. */
export function scannedPage(ws: tilo.Workspace, path: string): ScannedPage {
  return resourceProxy(ws, path, SURFACE, SCANNEDPAGE_ACTIONS) as unknown as ScannedPage;
}

/** ScanSession — `tl.so.tilo.print-centre.ScanSession`. */
export interface ScanSession extends tilo.Resource {
  "3kybq73d": string;  // scanner
  pva6zctd: Array<tilo.Entry<ScannedPage>>;  // pages
  wv7xzi7n: string;  // job
  t6j77lzd: string;  // failed
  /** The scan ended; a page that arrived is added to the document. */
  scan_ended(args: {
    j: unknown;
  }): Promise<void>;
  /** Runs `scan_ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  scan_endedMany(items: ScanEndedArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `ScanSession` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANSESSION_ACTIONS = ["scan_ended", "scan_endedMany"] as const;

/** The `tl.so.tilo.print-centre.ScanSession` kind at `path`. */
export function scanSession(ws: tilo.Workspace, path: string): ScanSession {
  return resourceProxy(ws, path, SURFACE, SCANSESSION_ACTIONS) as unknown as ScanSession;
}

/** PrintDocument — `tl.so.tilo.print-centre.PrintDocument`. */
export interface PrintDocument extends tilo.Resource {
  lurulxdl: unknown | null;  // file
  nfdtbdvt: unknown | null;  // preview
  hkdu56wa: string;  // name
  "7ec7kpow": string;  // queue
  "2ts7f4kz": number;  // copies
  uiqz5mt6: string;  // pages
  "27dapuxo": boolean;  // two_sided
  baptgoau: string;  // page_size
  ngceod5n: string;  // colour
  bgty3reb: string;  // duplex
  zqg7u6bz: string;  // orientation
  dzhipxmg: string;  // job
  so7pxc5y: string;  // failed
  "4ngifmut": string;  // trouble
  "6i7qoal4": boolean;  // handed
  /** The print ended, printed or failed; it says which. */
  print_ended(args: {
    j: unknown;
  }): Promise<void>;
  /** Runs `print_ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  print_endedMany(items: PrintEndedArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `PrintDocument` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PRINTDOCUMENT_ACTIONS = ["print_ended", "print_endedMany"] as const;

/** The `tl.so.tilo.print-centre.PrintDocument` kind at `path`. */
export function printDocument(ws: tilo.Workspace, path: string): PrintDocument {
  return resourceProxy(ws, path, SURFACE, PRINTDOCUMENT_ACTIONS) as unknown as PrintDocument;
}

/** Choice — `tl.so.tilo.print-centre.Choice`. */
export interface Choice extends tilo.Resource {
  ybptuzdv: string;  // key
  n2rjd6ws: string;  // label
  q5qiqhve: boolean;  // default
}

/** The actions `Choice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHOICE_ACTIONS = [] as const;

/** The `tl.so.tilo.print-centre.Choice` kind at `path`. */
export function choice(ws: tilo.Workspace, path: string): Choice {
  return resourceProxy(ws, path, SURFACE, CHOICE_ACTIONS) as unknown as Choice;
}

/** PrinterOptions — `tl.so.tilo.print-centre.PrinterOptions`. */
export interface PrinterOptions extends tilo.Resource {
  nov3upcs: Choice[];  // page_sizes
  pvufqhii: Choice[];  // colour_modes
  boxhaco3: Choice[];  // duplex_modes
  f6npg6br: Choice[];  // orientations
}

/** The actions `PrinterOptions` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PRINTEROPTIONS_ACTIONS = [] as const;

/** The `tl.so.tilo.print-centre.PrinterOptions` kind at `path`. */
export function printerOptions(ws: tilo.Workspace, path: string): PrinterOptions {
  return resourceProxy(ws, path, SURFACE, PRINTEROPTIONS_ACTIONS) as unknown as PrinterOptions;
}

/** PrintCentre — `tl.so.tilo.print-centre.PrintCentre`. */
export interface PrintCentre extends tilo.Resource {
  kqm47afv: Defaults;  // defaults
  lz7ue3si: Array<tilo.Entry<PrintDocument>>;  // documents
  iysceybl: Array<tilo.Entry<ScanSession>>;  // scans
  qxaoly4s: PrinterOptions;  // options
  /** The person's document being printed, made the first time it is wanted. */
  document_of(): Promise<PrintDocument>;
  /** The person's scanned document, made the first time it is wanted. */
  scan_of(): Promise<ScanSession>;
  /** Asks the engine what the chosen printer reports, so the form offers only what it can do. */
  ask_options(): Promise<void>;
  /** Runs `ask_options` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ask_optionsMany(items: AskOptionsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the file the person chose as what will be printed. */
  absorb_document(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `absorb_document` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  absorb_documentMany(items: AbsorbDocumentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a document another app hands over as what this window prints, and shows the print form so the person chooses the printer and how it prints: the one action an app calls to print a file it made. The file's bytes are the workspace's content, so the app that hands it over never needs this window's own files. */
  print_file(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `print_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  print_fileMany(items: PrintFileArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Leaves the print form a handed-over document opened, so the person's own choice of place stands. */
  leave_handed(): Promise<void>;
  /** Runs `leave_handed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  leave_handedMany(items: LeaveHandedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Prints the chosen document on the chosen printer, with the form's settings. */
  send_to_printer(): Promise<void>;
  /** Runs `send_to_printer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  send_to_printerMany(items: SendToPrinterArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Scans one page with the chosen scanner and the defaults. */
  scan_page(): Promise<void>;
  /** Runs `scan_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  scan_pageMany(items: ScanPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Crops a scanned page to the part the person kept, in fractions of the picture a viewer shows. The text read off the whole page no longer describes it, so it is cleared. */
  crop_page(args: {
    page: ScannedPage;
    left: number;
    top: number;
    right: number;
    bottom: number;
  }): Promise<void>;
  /** Runs `crop_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  crop_pageMany(items: CropPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Throws a scanned page away. */
  remove_page(args: {
    page: ScannedPage;
  }): Promise<void>;
  /** Runs `remove_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_pageMany(items: RemovePageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a new scanned document: the pages it held go. The change is the person's own and is taken back from history; it is refused while a page is still being scanned, so a scan in flight is never thrown away. */
  new_scanned_document(): Promise<void>;
  /** Runs `new_scanned_document` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  new_scanned_documentMany(items: NewScannedDocumentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a scanned page to `order`, and the page that stood there to where it was. */
  move_page(args: {
    page: ScannedPage;
    order: number;
    swapped?: number;
  }): Promise<void>;
  /** Runs `move_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_pageMany(items: MovePageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The scanned document as one searchable PDF, to save. */
  document_to_save(): Promise<unknown>;
  /** Chooses a printer, and offers what it reports. */
  choose_printer(args: {
    queue: string;
  }): Promise<void>;
  /** Runs `choose_printer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  choose_printerMany(items: ChoosePrinterArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Prints on both sides of the paper, or one. */
  set_two_sided(args: {
    two_sided: boolean;
  }): Promise<void>;
  /** Runs `set_two_sided` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_two_sidedMany(items: SetTwoSidedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** One of the printer's own options. */
  set_print_option(args: {
    option: string;
    value: string;
  }): Promise<void>;
  /** Runs `set_print_option` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_print_optionMany(items: SetPrintOptionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** How finely to scan. */
  set_resolution(args: {
    dpi: number;
  }): Promise<void>;
  /** Runs `set_resolution` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_resolutionMany(items: SetResolutionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** How to scan: in colour, grayscale or line art. */
  set_scan_colour(args: {
    colour: string;
  }): Promise<void>;
  /** Runs `set_scan_colour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_scan_colourMany(items: SetScanColourArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Where to scan from. */
  set_scan_source(args: {
    source: string;
  }): Promise<void>;
  /** Runs `set_scan_source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_scan_sourceMany(items: SetScanSourceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The language text is recognized in. */
  set_ocr_language(args: {
    language: string;
  }): Promise<void>;
  /** Runs `set_ocr_language` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_ocr_languageMany(items: SetOcrLanguageArgs[]): Promise<tilo.ActionAnswer[]>;
  set_scanner(args: {
    device_name: string;
  }): Promise<void>;
  /** Runs `set_scanner` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_scannerMany(items: SetScannerArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `PrintCentre` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PRINTCENTRE_ACTIONS = ["document_of", "scan_of", "ask_options", "ask_optionsMany", "absorb_document", "absorb_documentMany", "print_file", "print_fileMany", "leave_handed", "leave_handedMany", "send_to_printer", "send_to_printerMany", "scan_page", "scan_pageMany", "crop_page", "crop_pageMany", "remove_page", "remove_pageMany", "new_scanned_document", "new_scanned_documentMany", "move_page", "move_pageMany", "document_to_save", "choose_printer", "choose_printerMany", "set_two_sided", "set_two_sidedMany", "set_print_option", "set_print_optionMany", "set_resolution", "set_resolutionMany", "set_scan_colour", "set_scan_colourMany", "set_scan_source", "set_scan_sourceMany", "set_ocr_language", "set_ocr_languageMany", "set_scanner", "set_scannerMany"] as const;

/** The `tl.so.tilo.print-centre.PrintCentre` kind at `path`. */
export function printCentre(ws: tilo.Workspace, path: string): PrintCentre {
  return resourceProxy(ws, path, SURFACE, PRINTCENTRE_ACTIONS) as unknown as PrintCentre;
}

/** OptionsAsk — `tl.so.tilo.print-centre.OptionsAsk`. */
export interface OptionsAsk extends tilo.Resource {
  aun3edzr: string;  // queue
}

/** The actions `OptionsAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OPTIONSASK_ACTIONS = [] as const;

/** The `tl.so.tilo.print-centre.OptionsAsk` kind at `path`. */
export function optionsAsk(ws: tilo.Workspace, path: string): OptionsAsk {
  return resourceProxy(ws, path, SURFACE, OPTIONSASK_ACTIONS) as unknown as OptionsAsk;
}

/** ScanDetail — `tl.so.tilo.print-centre.ScanDetail`. */
export interface ScanDetail extends tilo.Resource {
  jlmf7wws: string;  // url
}

/** The actions `ScanDetail` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANDETAIL_ACTIONS = [] as const;

/** The `tl.so.tilo.print-centre.ScanDetail` kind at `path`. */
export function scanDetail(ws: tilo.Workspace, path: string): ScanDetail {
  return resourceProxy(ws, path, SURFACE, SCANDETAIL_ACTIONS) as unknown as ScanDetail;
}

/** OcrAsk — `tl.so.tilo.print-centre.OcrAsk`. */
export interface OcrAsk extends tilo.Resource {
  vuwh5pv6: string;  // url
  "56cimtrv": string;  // language
}

/** The actions `OcrAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OCRASK_ACTIONS = [] as const;

/** The `tl.so.tilo.print-centre.OcrAsk` kind at `path`. */
export function ocrAsk(ws: tilo.Workspace, path: string): OcrAsk {
  return resourceProxy(ws, path, SURFACE, OCRASK_ACTIONS) as unknown as OcrAsk;
}

/** OcrAnswer — `tl.so.tilo.print-centre.OcrAnswer`. */
export interface OcrAnswer extends tilo.Resource {
  qegnvsxr: string;  // text
}

/** The actions `OcrAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OCRANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.print-centre.OcrAnswer` kind at `path`. */
export function ocrAnswer(ws: tilo.Workspace, path: string): OcrAnswer {
  return resourceProxy(ws, path, SURFACE, OCRANSWER_ACTIONS) as unknown as OcrAnswer;
}

/** PdfPage — `tl.so.tilo.print-centre.PdfPage`. */
export interface PdfPage extends tilo.Resource {
  qxzi756t: string;  // url
  mytppaxs: string;  // text
}

/** The actions `PdfPage` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PDFPAGE_ACTIONS = [] as const;

/** The `tl.so.tilo.print-centre.PdfPage` kind at `path`. */
export function pdfPage(ws: tilo.Workspace, path: string): PdfPage {
  return resourceProxy(ws, path, SURFACE, PDFPAGE_ACTIONS) as unknown as PdfPage;
}

/** PagesAsk — `tl.so.tilo.print-centre.PagesAsk`. */
export interface PagesAsk extends tilo.Resource {
  omod5rbh: PdfPage[];  // pages
  u566k7ni: string;  // name
}

/** The actions `PagesAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGESASK_ACTIONS = [] as const;

/** The `tl.so.tilo.print-centre.PagesAsk` kind at `path`. */
export function pagesAsk(ws: tilo.Workspace, path: string): PagesAsk {
  return resourceProxy(ws, path, SURFACE, PAGESASK_ACTIONS) as unknown as PagesAsk;
}

/** UrlAnswer — `tl.so.tilo.print-centre.UrlAnswer`. */
export interface UrlAnswer extends tilo.Resource {
  y2ejci4l: string;  // url
}

/** The actions `UrlAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const URLANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.print-centre.UrlAnswer` kind at `path`. */
export function urlAnswer(ws: tilo.Workspace, path: string): UrlAnswer {
  return resourceProxy(ws, path, SURFACE, URLANSWER_ACTIONS) as unknown as UrlAnswer;
}

/** PrintAsk — `tl.so.tilo.print-centre.PrintAsk`. */
export interface PrintAsk extends tilo.Resource {
  fk3jkxxv: string;  // queue
  tddc4c3j: string;  // url
  dkvqbxny: string;  // name
  "4ryo577f": number;  // copies
  g6s27sdx: string;  // pages
  "7tzzarzm": boolean;  // two_sided
  akngsdrz: string;  // page_size
  jq74dcfy: string;  // colour
  oef7pq2y: string;  // duplex
  frurca4k: string;  // orientation
}

/** The actions `PrintAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PRINTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.print-centre.PrintAsk` kind at `path`. */
export function printAsk(ws: tilo.Workspace, path: string): PrintAsk {
  return resourceProxy(ws, path, SURFACE, PRINTASK_ACTIONS) as unknown as PrintAsk;
}

/** ScanAsk — `tl.so.tilo.print-centre.ScanAsk`. */
export interface ScanAsk extends tilo.Resource {
  vilpbf2g: string;  // scanner
  onyv6rml: number;  // resolution
  dvoh6l75: string;  // colour
  "5mr6kzrm": string;  // source
  "72c3cb6z": string;  // name
}

/** The actions `ScanAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANASK_ACTIONS = [] as const;

/** The `tl.so.tilo.print-centre.ScanAsk` kind at `path`. */
export function scanAsk(ws: tilo.Workspace, path: string): ScanAsk {
  return resourceProxy(ws, path, SURFACE, SCANASK_ACTIONS) as unknown as ScanAsk;
}

/** Print Centre — `tl.so.tilo.print-centre`. */
export interface TiloPrintCentre extends tilo.Resource {
  /** The first page's picture, for the print form: a PDF's first page, or the picture itself. */
  preview_of_file(args: {
    file: unknown;
  }): Promise<unknown | null>;
}

/** The actions `TiloPrintCentre` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOPRINTCENTRE_ACTIONS = ["preview_of_file"] as const;

/** The `tl.so.tilo.print-centre` kind at `path`. */
export function tiloPrintCentre(ws: tilo.Workspace, path: string): TiloPrintCentre {
  return resourceProxy(ws, path, SURFACE, TILOPRINTCENTRE_ACTIONS) as unknown as TiloPrintCentre;
}

