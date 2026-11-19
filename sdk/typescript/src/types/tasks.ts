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
export const SURFACE = "so.tilo.tasks@541eb358c0795bd2";

/** One of "project", "area". */
export type Kind = "project" | "area";

export interface Person {
  name: string;
  role: string;
}

export interface TrackerArea {
  id: string;
  name: string;
  kind: Kind;
  people: Array<tilo.Entry<Person>>;
}

export interface Area {
  resource: string;
  app: string;
  kind: string;
  label: string;
}

export interface Attachment {
  name: string;
  media_type: string | null;
  bytes: unknown;
  author: string | null;
  author_name: string;
  at_ms: number;
}

/** One of "person", "agent". */
export type BudgetBy = "person" | "agent";

export interface Issue {
  number: number;
  rank: string;
  title: string;
  body: string;
  status: Status;
  assignee: string | null;
  assigned_by: string | null;
  assigned_ms: number;
  labels: string[];
  parent: string | null;
  links: Array<tilo.Entry<Link>>;
  area: Area | null;
  due_ms: number | null;
  priority: number | null;
  estimate: number | null;
  attachments: Array<tilo.Entry<Attachment>>;
  comments: Array<tilo.Entry<Message>>;
  author: string | null;
  author_name: string;
  created_ms: number;
  origin: Origin | null;
  url: string | null;
  work: Work | null;
}

export interface Link {
  label: string;
  app: string;
  kind: string;
  resource: string;
  record: string | null;
}

export interface ListedIssue {
  key: string | null;
  under: string | null;
  title: string;
  parent: string | null;
  body: string | null;
  status: Status | null;
  assignee: string | null;
  url: string | null;
  project: string | null;
  priority: Priority | null;
  estimate: number | null;
  due_ms: number | null;
  labels: string[] | null;
  origin: Origin | null;
  rank: string | null;
  goal: Area | null;
}

export interface Message {
  author: string | null;
  author_name: string;
  by_agent: boolean;
  at_ms: number;
  origin: Origin | null;
  text: string;
  kind: MessageKind;
  run: number;
  lookup: string;
  step: string;
  incorporated: boolean;
}

/** One of "Comment", "ToRun", "FromRun", "Lookup", "Step", "Answer". */
export type MessageKind = "Comment" | "ToRun" | "FromRun" | "Lookup" | "Step" | "Answer";

export interface NewIssue {
  title: string;
  parent: string | null;
  body: string | null;
  status: Status | null;
  assignee: string | null;
  url: string | null;
  project: string | null;
  priority: Priority | null;
  estimate: number | null;
  due_ms: number | null;
  labels: string[] | null;
  origin: Origin | null;
  rank: string | null;
  goal: Area | null;
}

export interface Origin {
  author: string;
  handle: string | null;
  source: string;
  url: string | null;
  reference: string | null;
  created_ms: number | null;
  updated_ms: number | null;
}

/** One of "none", "urgent", "high", "medium", "low". */
export type Priority = "none" | "urgent" | "high" | "medium" | "low";

export interface Resumed {
  cause: string;
  at_ms: number;
}

/** One of "backlog", "todo", "in-progress", "in-review", "done", "cancelled". */
export type Status = "backlog" | "todo" | "in-progress" | "in-review" | "done" | "cancelled";

export interface Stopped {
  why: StoppedWhy;
  words: string;
  at_ms: number;
}

/** One of "failed", "budget", "time", "lost-worker", "person". */
export type StoppedWhy = "failed" | "budget" | "time" | "lost-worker" | "person";

export interface WaitFor {
  kind: WaitKind;
  what: string;
  on: string | null;
  event: string | null;
}

/** One of "time", "reply", "change", "answer", "people", "pieces". */
export type WaitKind = "time" | "reply" | "change" | "answer" | "people" | "pieces";

export interface Work {
  folder: string;
  note: string;
  waits: WaitFor[];
  until_ms: number | null;
  since_ms: number | null;
  budget_micros: number | null;
  budget_ms: number | null;
  budget_by: BudgetBy;
  spent_micros: number;
  worked_ms: number;
  sessions: number;
  session_since_ms: number | null;
  lost: number;
  stopped: Stopped | null;
  resumed: Resumed | null;
  pieces_at_once: number | null;
}

/** The arguments of `create-area`. */
export interface CreateAreaArgs {
  name: string;
  kind?: Kind;
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

/** The arguments of `delete-area`. */
export interface DeleteAreaArgs {
  area: string;
}

/** The arguments of `create`. */
export interface CreateArgs {
  title: string;
  parent?: string;
  body?: string;
  status?: Status;
  assignee?: string;
  url?: string;
  project?: string;
  priority?: Priority;
  estimate?: number;
  due_ms?: number;
  labels?: string[];
  origin?: Origin;
  rank?: string;
  goal?: Area;
}

/** The arguments of `start`. */
export interface StartArgs {
  before: number;
}

/** The arguments of `create-many`. */
export interface CreateManyArgs {
  issues: ListedIssue[];
}

/** The arguments of `rename`. */
export interface RenameArgs {
  issue: string;
  title: string;
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

/** The arguments of `set-project`. */
export interface SetProjectArgs {
  issue: string;
  project?: string;
  goal?: Area;
}

/** The arguments of `attach`. */
export interface AttachArgs {
  issue: string;
  name: string;
  bytes: unknown;
  media_type?: string;
}

/** The arguments of `detach`. */
export interface DetachArgs {
  issue: string;
  attachment: string;
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
  rank?: string;
}

/** The arguments of `comment`. */
export interface CommentArgs {
  issue: string;
  text: string;
  origin?: Origin;
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
  priority?: Priority;
  estimate?: number;
  due_ms?: number;
}

/** The arguments of `set-place`. */
export interface SetPlaceArgs {
  issue: string;
  folder: string;
  note?: string;
}

/** The arguments of `wait`. */
export interface WaitArgs {
  issue: string;
  waits: WaitFor[];
  until_ms: number;
}

/** The arguments of `resume`. */
export interface ResumeArgs {
  issue: string;
  cause: string;
}

/** The arguments of `set-budget`. */
export interface SetBudgetArgs {
  issue: string;
  money_micros?: number;
  time_ms?: number;
}

/** The arguments of `set-pieces-at-once`. */
export interface SetPiecesAtOnceArgs {
  issue: string;
  at_once?: number;
}

/** The arguments of `session-began`. */
export interface SessionBeganArgs {
  issue: string;
}

/** The arguments of `session-ended`. */
export interface SessionEndedArgs {
  issue: string;
  cost_micros: number;
  worked_ms: number;
}

/** The arguments of `stop`. */
export interface StopArgs {
  issue: string;
  why: StoppedWhy;
  words: string;
}

/** Tasks — `tasks.tracker`. */
export interface Tracker extends tilo.Resource {
  areas: Array<tilo.Entry<TrackerArea>>;
  createArea(args: {
    name: string;
    kind?: Kind;
  }): Promise<void>;
  /** Runs `create-area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  createAreaMany(items: CreateAreaArgs[]): Promise<tilo.ActionAnswer[]>;
  renameArea(args: {
    area: string;
    name: string;
  }): Promise<void>;
  /** Runs `rename-area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameAreaMany(items: RenameAreaArgs[]): Promise<tilo.ActionAnswer[]>;
  setKind(args: {
    area: string;
    kind: Kind;
  }): Promise<void>;
  /** Runs `set-kind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setKindMany(items: SetKindArgs[]): Promise<tilo.ActionAnswer[]>;
  involve(args: {
    area: string;
    name: string;
    role?: string;
  }): Promise<void>;
  /** Runs `involve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  involveMany(items: InvolveArgs[]): Promise<tilo.ActionAnswer[]>;
  removePerson(args: {
    area: string;
    person: string;
  }): Promise<void>;
  /** Runs `remove-person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removePersonMany(items: RemovePersonArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteArea(args: {
    area: string;
  }): Promise<void>;
  /** Runs `delete-area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteAreaMany(items: DeleteAreaArgs[]): Promise<tilo.ActionAnswer[]>;
  newProject(): Promise<void>;
}

/** The actions `Tracker` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TRACKER_ACTIONS = ["createArea", "createAreaMany", "renameArea", "renameAreaMany", "setKind", "setKindMany", "involve", "involveMany", "removePerson", "removePersonMany", "deleteArea", "deleteAreaMany", "newProject"] as const;

/** The `tasks.tracker` kind at `path`. */
export function tracker(ws: tilo.Workspace, path: string): Tracker {
  return resourceProxy(ws, path, SURFACE, TRACKER_ACTIONS) as unknown as Tracker;
}

/** Issues — `tasks.issues`. */
export interface Issues extends tilo.Resource {
  issues: Array<tilo.Entry<Issue>>;
  readonly before: number;
  readonly numbers: number;
  create(args: {
    title: string;
    parent?: string;
    body?: string;
    status?: Status;
    assignee?: string;
    url?: string;
    project?: string;
    priority?: Priority;
    estimate?: number;
    due_ms?: number;
    labels?: string[];
    origin?: Origin;
    rank?: string;
    goal?: Area;
  }): Promise<void>;
  /** Runs `create` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  createMany(items: CreateArgs[]): Promise<tilo.ActionAnswer[]>;
  start(args: {
    before: number;
  }): Promise<void>;
  /** Runs `start` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  startMany(items: StartArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `create-many` action is named as the SDK's bulk calls (`<action>Many`):
  // run it with `action("create-many")`.
  rename(args: {
    issue: string;
    title: string;
  }): Promise<void>;
  /** Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameMany(items: RenameArgs[]): Promise<tilo.ActionAnswer[]>;
  moveUnder(args: {
    issue: string;
    parent?: string;
  }): Promise<void>;
  /** Runs `move-under` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  moveUnderMany(items: MoveUnderArgs[]): Promise<tilo.ActionAnswer[]>;
  assign(args: {
    issue: string;
    assignee?: string;
  }): Promise<void>;
  /** Runs `assign` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  assignMany(items: AssignArgs[]): Promise<tilo.ActionAnswer[]>;
  setProject(args: {
    issue: string;
    project?: string;
    goal?: Area;
  }): Promise<void>;
  /** Runs `set-project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setProjectMany(items: SetProjectArgs[]): Promise<tilo.ActionAnswer[]>;
  attach(args: {
    issue: string;
    name: string;
    bytes: unknown;
    media_type?: string;
  }): Promise<void>;
  /** Runs `attach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  attachMany(items: AttachArgs[]): Promise<tilo.ActionAnswer[]>;
  detach(args: {
    issue: string;
    attachment: string;
  }): Promise<void>;
  /** Runs `detach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  detachMany(items: DetachArgs[]): Promise<tilo.ActionAnswer[]>;
  addLabel(args: {
    issue: string;
    label: string;
  }): Promise<void>;
  /** Runs `add-label` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addLabelMany(items: AddLabelArgs[]): Promise<tilo.ActionAnswer[]>;
  place(args: {
    issue: string;
    status: Status;
    before?: string;
    rank?: string;
  }): Promise<void>;
  /** Runs `place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  placeMany(items: PlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  comment(args: {
    issue: string;
    text: string;
    origin?: Origin;
  }): Promise<void>;
  /** Runs `comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  commentMany(items: CommentArgs[]): Promise<tilo.ActionAnswer[]>;
  link(args: {
    issue: string;
    app: string;
    kind: string;
    resource: string;
    label: string;
    record?: string;
  }): Promise<void>;
  /** Runs `link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  linkMany(items: LinkArgs[]): Promise<tilo.ActionAnswer[]>;
  linkNote(args: {
    issue: string;
    note: unknown;
  }): Promise<void>;
  /** Runs `link-note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  linkNoteMany(items: LinkNoteArgs[]): Promise<tilo.ActionAnswer[]>;
  delete(args: {
    issue: string;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  plan(args: {
    issue: string;
    area?: Area;
    priority?: Priority;
    estimate?: number;
    due_ms?: number;
  }): Promise<void>;
  /** Runs `plan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  planMany(items: PlanArgs[]): Promise<tilo.ActionAnswer[]>;
  setPlace(args: {
    issue: string;
    folder: string;
    note?: string;
  }): Promise<void>;
  /** Runs `set-place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setPlaceMany(items: SetPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  wait(args: {
    issue: string;
    waits: WaitFor[];
    until_ms: number;
  }): Promise<void>;
  /** Runs `wait` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  waitMany(items: WaitArgs[]): Promise<tilo.ActionAnswer[]>;
  resume(args: {
    issue: string;
    cause: string;
  }): Promise<void>;
  /** Runs `resume` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resumeMany(items: ResumeArgs[]): Promise<tilo.ActionAnswer[]>;
  setBudget(args: {
    issue: string;
    money_micros?: number;
    time_ms?: number;
  }): Promise<void>;
  /** Runs `set-budget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setBudgetMany(items: SetBudgetArgs[]): Promise<tilo.ActionAnswer[]>;
  setPiecesAtOnce(args: {
    issue: string;
    at_once?: number;
  }): Promise<void>;
  /** Runs `set-pieces-at-once` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setPiecesAtOnceMany(items: SetPiecesAtOnceArgs[]): Promise<tilo.ActionAnswer[]>;
  sessionBegan(args: {
    issue: string;
  }): Promise<void>;
  /** Runs `session-began` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sessionBeganMany(items: SessionBeganArgs[]): Promise<tilo.ActionAnswer[]>;
  sessionEnded(args: {
    issue: string;
    cost_micros: number;
    worked_ms: number;
  }): Promise<void>;
  /** Runs `session-ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sessionEndedMany(items: SessionEndedArgs[]): Promise<tilo.ActionAnswer[]>;
  stop(args: {
    issue: string;
    why: StoppedWhy;
    words: string;
  }): Promise<void>;
  /** Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopMany(items: StopArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Issues` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ISSUES_ACTIONS = ["create", "createMany", "start", "startMany", "rename", "renameMany", "moveUnder", "moveUnderMany", "assign", "assignMany", "setProject", "setProjectMany", "attach", "attachMany", "detach", "detachMany", "addLabel", "addLabelMany", "place", "placeMany", "comment", "commentMany", "link", "linkMany", "linkNote", "linkNoteMany", "delete", "deleteMany", "plan", "planMany", "setPlace", "setPlaceMany", "wait", "waitMany", "resume", "resumeMany", "setBudget", "setBudgetMany", "setPiecesAtOnce", "setPiecesAtOnceMany", "sessionBegan", "sessionBeganMany", "sessionEnded", "sessionEndedMany", "stop", "stopMany"] as const;

/** The `tasks.issues` kind at `path`. */
export function issues(ws: tilo.Workspace, path: string): Issues {
  return resourceProxy(ws, path, SURFACE, ISSUES_ACTIONS) as unknown as Issues;
}

