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
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.tasks@dc607ef0bfe199ca";

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
  }): Promise<tilo.Run | void>;
  createArea(args: {
    name: string;
    kind?: Kind;
  }): Promise<tilo.Run | void>;
  rename(args: {
    issue: string;
    title: string;
  }): Promise<tilo.Run | void>;
  renameArea(args: {
    area: string;
    name: string;
  }): Promise<tilo.Run | void>;
  setKind(args: {
    area: string;
    kind: Kind;
  }): Promise<tilo.Run | void>;
  involve(args: {
    area: string;
    name: string;
    role?: string;
  }): Promise<tilo.Run | void>;
  removePerson(args: {
    area: string;
    person: string;
  }): Promise<tilo.Run | void>;
  moveUnder(args: {
    issue: string;
    parent?: string;
  }): Promise<tilo.Run | void>;
  assign(args: {
    issue: string;
    assignee?: string;
  }): Promise<tilo.Run | void>;
  addLabel(args: {
    issue: string;
    label: string;
  }): Promise<tilo.Run | void>;
  place(args: {
    issue: string;
    status: Status;
    before?: string;
  }): Promise<tilo.Run | void>;
  comment(args: {
    issue: string;
    text: string;
  }): Promise<tilo.Run | void>;
  link(args: {
    issue: string;
    app: string;
    kind: string;
    resource: string;
    label: string;
    record?: string;
  }): Promise<tilo.Run | void>;
  linkNote(args: {
    issue: string;
    note: unknown;
  }): Promise<tilo.Run | void>;
  delete(args: {
    issue: string;
  }): Promise<tilo.Run | void>;
  plan(args: {
    issue: string;
    area?: Area;
    priority?: number;
    due_ms?: number;
  }): Promise<tilo.Run | void>;
  import(args: {
    tasks: Imported[];
  }): Promise<tilo.Run | void>;
  newProject(): Promise<void>;
}

/** The `tasks.tracker` kind at `path`. */
export function tracker(ws: tilo.Workspace, path: string): Tracker {
  return resourceProxy(ws, path, SURFACE) as unknown as Tracker;
}

