/**
 * Types for Tasks (`so.tilo.tasks`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Tracker` is a `Resource` of the app's `tasks.tracker`, made by its factory with the
 * resource's path (`tasks/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.tasks@08d0fa20785e982a";

export interface Area {
  resource: string;
  app: string;
  kind: string;
  label: string;
}

export interface Imported {
  task: string;
  title: string;
  instruction: string;
  status: Status;
  assignee: string | null;
  author: string;
  created_ms: number;
  messages: ImportedMessage[];
}

export interface ImportedMessage {
  author: string;
  by_agent: boolean;
  at_ms: number;
  text: string;
}

export interface Issue {
  number: number;
  title: string;
  body: string;
  status: Status;
  assignee: string | null;
  labels: string[];
  parent: string | null;
  links: Link[];
  area: Area | null;
  due_ms: number | null;
  priority: number | null;
  comments: Message[];
  author: string;
  created_ms: number;
  url: string | null;
}

/** One of "project", "area". */
export type Kind = "project" | "area";

export interface Link {
  label: string;
  app: string;
  kind: string;
  resource: string;
  record: string | null;
}

export interface Message {
  author: string;
  by_agent: boolean;
  at_ms: number;
  text: string;
  kind: MessageKind;
  run: number;
  lookup: string;
  step: string;
  incorporated: boolean;
}

/** One of "comment", "to_run", "from_run", "lookup", "step", "answer". */
export type MessageKind = "comment" | "to_run" | "from_run" | "lookup" | "step" | "answer";

export interface Person {
  name: string;
  role: string;
}

/** One of "backlog", "todo", "in_progress", "in_review", "done", "cancelled". */
export type Status = "backlog" | "todo" | "in_progress" | "in_review" | "done" | "cancelled";

export interface TrackerArea {
  id: string;
  name: string;
  kind: Kind;
  people: Person[];
}

/** The arguments of `create`. */
export interface CreateArgs {
  title: string;
  parent?: string;
  body?: string;
  status?: Status;
  assignee?: string;
  url?: string;
}

/** The arguments of `create-area`. */
export interface CreateAreaArgs {
  name: string;
  kind?: Kind;
}

/** The arguments of `rename`. */
export interface RenameArgs {
  issue: string;
  title: string;
}

/** The arguments of `rename-area`. */
export interface RenameAreaArgs {
  area: string;
  name: string;
}

/** The arguments of `set-kind`. */
export interface SetKindArgs {
  area: string;
  kind: Kind;
}

/** The arguments of `involve`. */
export interface InvolveArgs {
  area: string;
  name: string;
  role?: string;
}

/** The arguments of `remove-person`. */
export interface RemovePersonArgs {
  area: string;
  person: string;
}

/** The arguments of `move-under`. */
export interface MoveUnderArgs {
  issue: string;
  parent?: string;
}

/** The arguments of `assign`. */
export interface AssignArgs {
  issue: string;
  assignee?: string;
}

/** The arguments of `add-label`. */
export interface AddLabelArgs {
  issue: string;
  label: string;
}

/** The arguments of `place`. */
export interface PlaceArgs {
  issue: string;
  status: Status;
  before?: string;
}

/** The arguments of `comment`. */
export interface CommentArgs {
  issue: string;
  text: string;
}

/** The arguments of `link`. */
export interface LinkArgs {
  issue: string;
  app: string;
  kind: string;
  resource: string;
  label: string;
  record?: string;
}

/** The arguments of `link-note`. */
export interface LinkNoteArgs {
  issue: string;
  note: unknown;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  issue: string;
}

/** The arguments of `plan`. */
export interface PlanArgs {
  issue: string;
  area?: Area;
  priority?: number;
  due_ms?: number;
}

/** The arguments of `import`. */
export interface ImportArgs {
  tasks: Imported[];
}

/** Tasks — `tasks.tracker`. */
export interface Tracker extends tilo.Resource {
  issues: Issue[];
  areas: TrackerArea[];
  readonly numbers: number;
  readonly imported: string[];
  create(args: {
    title: string;
    parent?: string;
    body?: string;
    status?: Status;
    assignee?: string;
    url?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `create` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  createMany(items: CreateArgs[]): Promise<tilo.ActionAnswer[]>;
  createArea(args: {
    name: string;
    kind?: Kind;
  }): Promise<tilo.Run | void>;
  /** Runs `create-area` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  createAreaMany(items: CreateAreaArgs[]): Promise<tilo.ActionAnswer[]>;
  rename(args: {
    issue: string;
    title: string;
  }): Promise<tilo.Run | void>;
  /** Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  renameMany(items: RenameArgs[]): Promise<tilo.ActionAnswer[]>;
  renameArea(args: {
    area: string;
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `rename-area` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  renameAreaMany(items: RenameAreaArgs[]): Promise<tilo.ActionAnswer[]>;
  setKind(args: {
    area: string;
    kind: Kind;
  }): Promise<tilo.Run | void>;
  /** Runs `set-kind` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setKindMany(items: SetKindArgs[]): Promise<tilo.ActionAnswer[]>;
  involve(args: {
    area: string;
    name: string;
    role?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `involve` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  involveMany(items: InvolveArgs[]): Promise<tilo.ActionAnswer[]>;
  removePerson(args: {
    area: string;
    person: string;
  }): Promise<tilo.Run | void>;
  /** Runs `remove-person` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removePersonMany(items: RemovePersonArgs[]): Promise<tilo.ActionAnswer[]>;
  moveUnder(args: {
    issue: string;
    parent?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `move-under` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  moveUnderMany(items: MoveUnderArgs[]): Promise<tilo.ActionAnswer[]>;
  assign(args: {
    issue: string;
    assignee?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `assign` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  assignMany(items: AssignArgs[]): Promise<tilo.ActionAnswer[]>;
  addLabel(args: {
    issue: string;
    label: string;
  }): Promise<tilo.Run | void>;
  /** Runs `add-label` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addLabelMany(items: AddLabelArgs[]): Promise<tilo.ActionAnswer[]>;
  place(args: {
    issue: string;
    status: Status;
    before?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `place` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  placeMany(items: PlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  comment(args: {
    issue: string;
    text: string;
  }): Promise<tilo.Run | void>;
  /** Runs `comment` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  commentMany(items: CommentArgs[]): Promise<tilo.ActionAnswer[]>;
  link(args: {
    issue: string;
    app: string;
    kind: string;
    resource: string;
    label: string;
    record?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `link` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  linkMany(items: LinkArgs[]): Promise<tilo.ActionAnswer[]>;
  linkNote(args: {
    issue: string;
    note: unknown;
  }): Promise<tilo.Run | void>;
  /** Runs `link-note` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  linkNoteMany(items: LinkNoteArgs[]): Promise<tilo.ActionAnswer[]>;
  delete(args: {
    issue: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  plan(args: {
    issue: string;
    area?: Area;
    priority?: number;
    due_ms?: number;
  }): Promise<tilo.Run | void>;
  /** Runs `plan` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  planMany(items: PlanArgs[]): Promise<tilo.ActionAnswer[]>;
  import(args: {
    tasks: Imported[];
  }): Promise<tilo.Run | void>;
  /** Runs `import` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  importMany(items: ImportArgs[]): Promise<tilo.ActionAnswer[]>;
  newProject(): Promise<void>;
}

/** The `tasks.tracker` kind at `path`. */
export function tracker(ws: tilo.Workspace, path: string): Tracker {
  return resourceProxy(ws, path, SURFACE) as unknown as Tracker;
}

