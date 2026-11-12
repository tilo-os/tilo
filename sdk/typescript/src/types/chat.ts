/**
 * Types for Chat (`so.tilo.chat`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Workspace` is a `Resource` of the app's `chat.workspace`, made by its factory with the
 * resource's path (`chat/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

export interface Category {
  name: string;
}

/** One of "text", "voice". */
export type ChannelKind = "text" | "voice";

export interface ChannelPlace {
  channel: string;
  category: string | null;
}

export interface ContextField {
  key: string;
  value: string;
}

export interface Conversation {
  category: string | null;
  name: string;
  topic: string;
  kind: ChannelKind;
  archived: boolean;
  participants: string[];
}

export interface CropRef {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Decision {
  id: string;
  person: string;
  reports: string[];
  outcome: Outcome2;
  until_ms: number | null;
  reasoning: string;
  model: string;
  reverted: Reverted[];
  by: string;
  at_ms: number;
  overturned_by: string | null;
  overturn_note: string | null;
  overturned_at_ms: number | null;
}

export interface Destination {
  room: string;
  provider: string;
  channel: string;
}

export interface FeedbackReport {
  submission: Submission | null;
  author: string;
  ts: number;
  triage: Triage[];
  reactions: unknown;
}

export interface FrameTimings {
  frames: number[];
  budget: number;
}

/** One of "name", "email", "phone", "address", "account", "credential", "personal". */
export type Hidden = "name" | "email" | "phone" | "address" | "account" | "credential" | "personal";

export interface Interaction {
  at: number;
  app: string;
  action: string;
  resource: string;
  label: string | null;
  outcome: Outcome;
}

export interface Moderation {
  reports: Report[];
  decisions: Decision[];
}

/** One of "done", "refused", "failed". */
export type Outcome = "done" | "refused" | "failed";

/** One of "no_action", "restrict", "ban". */
export type Outcome2 = "no_action" | "restrict" | "ban";

export interface Part {
  text: string;
  hidden: Hidden | null;
}

export interface Person {
  id: string;
  name: string;
  initials: string;
  color: number;
  status: Presence;
  agent: boolean;
}

/** One of "online", "idle", "dnd", "offline". */
export type Presence = "online" | "idle" | "dnd" | "offline";

export interface ReadMark {
  part: number;
  seen: number;
}

export interface RecordingRef {
  hash: string;
  app: string;
  app_name: string;
  kind: string;
  from_ms: number;
  to_ms: number;
  steps: number;
  bytes: number;
}

export interface Report {
  id: string;
  reporter: string;
  person: string;
  app: string | null;
  record: string | null;
  item: string | null;
  reason: string;
  at_ms: number;
  decision: string | null;
}

export interface ResourceRef {
  kind: string;
  id: string;
  label: string;
  span: Span | null;
}

export interface Reverted {
  app: string;
  record: string;
  run: string;
  changes: number;
}

export interface ReviewedFeedback {
  text: string;
  context: ContextField[];
  screenshot: ScreenshotRef | null;
  selections: ReviewedSelection[];
  interactions: Interaction[];
  timings: FrameTimings | null;
  recording: RecordingRef | null;
}

export interface ReviewedSelection {
  app: string;
  app_name: string;
  build: string | null;
  path: string;
  role: string;
  kind: string | null;
  record: string | null;
  label: ReviewedText | null;
  texts: ReviewedText[];
  note: ReviewedText | null;
  crop: CropRef | null;
}

export interface ReviewedText {
  kind: TextKind;
  parts: Part[];
}

export interface Ruling {
  outcome: Outcome2;
  until_ms: number | null;
  reasoning: string;
  model: string;
  reverted: Reverted[];
}

export interface ScreenshotRef {
  hash: string;
  width: number;
  height: number;
}

export interface Span {
  start: number;
  end: number;
  unit: SpanUnit;
}

/** One of "chars", "bytes". */
export type SpanUnit = "chars" | "bytes";

export interface Submission {
  id: string;
  destination: Destination;
  feedback: ReviewedFeedback;
}

/** One of "interface", "content". */
export type TextKind = "interface" | "content";

export interface Triage {
  author: string;
  ts: number;
  update: TriageUpdate | null;
}

/** One of investigating, resolved, reopen, duplicate, task: an object whose case the app tags it with. */
export type TriageUpdate = Record<string, unknown>;

export interface WorkspaceFiles {
  largest: number;
  member_hourly: number;
}

/** One of "complete", "truncated", "refused", "cancelled", "failed". */
export type AgentFinish = "complete" | "truncated" | "refused" | "cancelled" | "failed";

/** One of asked, applied, no_change, refused: an object whose case the app tags it with. */
export type AgentStepOutcome = Record<string, unknown>;

/** One of taken, waiting, doing, provider_finished, done, refused: an object whose case the app tags it with. */
export type AgentWorkState = Record<string, unknown>;

/** One of item, items, text: an object whose case the app tags it with. */
export type Anchor = Record<string, unknown>;

/** One of resource, text, image, video, file: an object whose case the app tags it with. */
export type Attached = Record<string, unknown>;

/** One of bool, int, str: an object whose case the app tags it with. */
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

export interface MarkSpan {
  start: number;
  end: number;
  name: string;
  value: MarkValue;
}

/** One of bool, int, str: an object whose case the app tags it with. */
export type MarkValue = Record<string, unknown>;

export interface Mention {
  user: string;
  start: number;
  len: number;
  is_agent: boolean;
}

export interface Message {
  channel: string;
  author: string;
  ts: number;
  text: string;
  body: ClipBlock[];
  edited: boolean;
  history: Revision[];
  parent: string | null;
  reactions: unknown;
  mentions: Mention[];
  attachments: Attached[];
  pinned: boolean;
  work: Work | null;
  appearance: ShownAs | null;
  share: WindowShare | null;
  attempts: number;
}

export interface ProviderStream {
  run: string;
  sequence: number;
  reasoning: string;
  progress: string;
  finish: AgentFinish | null;
  detail: string;
}

export interface ResourceAt {
  app: string;
  record: string | null;
  reference: ResourceRef;
}

export interface Revision {
  ts: number;
  text: string;
}

/** One of "view", "edit". */
export type ShareAccess = "view" | "edit";

export interface ShownAs {
  name: string;
  avatar: string | null;
  persona: ShownPersona | null;
}

export interface ShownPersona {
  service: string;
  account: string;
  linked: string | null;
}

export interface Step {
  id: string;
  sequence: number;
  resource: string;
  label: string;
  outcome: AgentStepOutcome | null;
}

export interface StepUpdate {
  id: string;
  sequence: number;
  resource: string;
  label: string;
  outcome: string;
  why: string | null;
}

export interface Transition {
  state: AgentWorkState;
  since: number;
}

export interface WindowShare {
  access: ShareAccess;
  open: boolean;
  ended: boolean;
}

export interface Work {
  stream: ProviderStream | null;
  state: Transition | null;
  trace: Step[];
}

export interface WorkUpdate {
  state: string;
  app: string | null;
  did: string | null;
  about: string | null;
  trace: StepUpdate[];
}

export interface Emoji {
  image: unknown;
  added_by: string;
  added_at: number;
}

export interface Agent {
  id: string;
  name: string;
  colour: number | null;
  keeper: string;
  model: string | null;
  bound: Bound[];
  spend_limit_micros: number | null;
  spent: Spent | null;
  state: AgentState;
  created_at: number;
}

/** One of "active", "retired". */
export type AgentState = "active" | "retired";

export interface Bound {
  node: string;
  kinds: string[];
  answer: string;
}

export interface Spent {
  day: string;
  cost_micros: number;
}

/** Chat — `chat.workspace`. */
export interface Workspace extends tilo.Resource {
  readonly categories: Category[];
  readonly channels: Conversation[];
  people: Person[];
  agent_names: Record<string, string>;
  read: Record<string, Record<string, ReadMark>>;
  feedback: FeedbackReport[];
  moderation: Moderation;
  readonly files: WorkspaceFiles;
  beginChat(args: {
    name: string;
    initials: string;
    color: number;
  }): Promise<tilo.Run | void>;
  introduce(args: {
    name: string;
    initials: string;
    color: number;
  }): Promise<tilo.Run | void>;
  nameAgent(args: {
    participant: string;
    name: string;
  }): Promise<tilo.Run | void>;
  createCategory(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  createChannel(args: {
    name: string;
    category?: string;
    kind?: ChannelKind;
    topic?: string;
  }): Promise<tilo.Run | void>;
  createChannelWithId(args: {
    id: string;
    name: string;
    category?: string;
    kind?: ChannelKind;
    topic?: string;
    participants?: string[];
  }): Promise<tilo.Run | void>;
  deleteCategory(args: {
    category: string;
  }): Promise<tilo.Run | void>;
  moveChannel(args: {
    channel: string;
    up: boolean;
  }): Promise<tilo.Run | void>;
  moveCategory(args: {
    category: string;
    up: boolean;
  }): Promise<tilo.Run | void>;
  arrangeChannels(args: {
    categories: string[];
    channels: ChannelPlace[];
  }): Promise<tilo.Run | void>;
  setChannelCategory(args: {
    channel: string;
    category?: string;
  }): Promise<tilo.Run | void>;
  renameChannel(args: {
    channel: string;
    name: string;
  }): Promise<tilo.Run | void>;
  setChannelTopic(args: {
    channel: string;
    topic: string;
  }): Promise<tilo.Run | void>;
  archiveChannel(args: {
    channel: string;
    archived: boolean;
  }): Promise<tilo.Run | void>;
  renameCategory(args: {
    category: string;
    name: string;
  }): Promise<tilo.Run | void>;
  deleteChannel(args: {
    channel: string;
  }): Promise<tilo.Run | void>;
  reportMessage(args: {
    message: string;
    author: string;
    reason: string;
  }): Promise<tilo.Run | void>;
  reportPerson(args: {
    person: string;
    reason: string;
  }): Promise<tilo.Run | void>;
  decideReports(args: {
    id: string;
    person: string;
    reports: string[];
    ruling: Ruling;
  }): Promise<tilo.Run | void>;
  overturnDecision(args: {
    decision: string;
    note: string;
  }): Promise<tilo.Run | void>;
  markRead(args: {
    channel: string;
    part: number;
    seen: number;
  }): Promise<tilo.Run | void>;
  submitFeedback(args: {
    channel: string;
    submission: unknown;
  }): Promise<tilo.Run | void>;
  triage(args: {
    report: string;
    update: unknown;
  }): Promise<tilo.Run | void>;
  exportContent(args: {
    channel: string;
    format?: string;
  }): Promise<void>;
  startCall(): Promise<void>;
  selectInput(args: {
    channel: string;
    input: string;
  }): Promise<void>;
}

/** The `chat.workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path) as unknown as Workspace;
}

/** Messages — `chat.messages`. */
export interface Messages extends tilo.Resource {
  readonly messages: Message[];
  readonly earlier: Record<string, Record<string, number>>;
  post(args: {
    channel: string;
    text: string;
    parent?: string;
    body?: unknown[];
    attachments?: unknown[];
    mentions?: unknown[];
    written?: number;
  }): Promise<tilo.Run | void>;
  postFile(args: {
    channel: string;
    content: string;
    name: string;
    text?: string;
    attachment?: Attached;
    mentions?: Mention[];
  }): Promise<tilo.Run | void>;
  shareResource(args: {
    channel: string;
    attached: ResourceAt;
    text?: string;
    mentions?: Mention[];
  }): Promise<tilo.Run | void>;
  shareWindow(args: {
    channel: string;
    attached: ResourceAt;
    access?: ShareAccess;
    text?: string;
    mentions?: Mention[];
  }): Promise<tilo.Run | void>;
  stopSharing(args: {
    message: string;
  }): Promise<tilo.Run | void>;
  setShareAccess(args: {
    message: string;
    access: ShareAccess;
  }): Promise<tilo.Run | void>;
  shareOpen(args: {
    message: string;
    open: boolean;
  }): Promise<tilo.Run | void>;
  deliverMessage(args: {
    channel: string;
    author: string;
    body: string;
    ts?: number;
  }): Promise<tilo.Run | void>;
  editMessage(args: {
    message: string;
    text: string;
    body?: ClipBlock[];
    mentions?: Mention[];
  }): Promise<tilo.Run | void>;
  deleteMessage(args: {
    message: string;
  }): Promise<tilo.Run | void>;
  removeMessage(args: {
    message: string;
  }): Promise<tilo.Run | void>;
  pin(args: {
    message: string;
    pinned: boolean;
  }): Promise<tilo.Run | void>;
  retry(args: {
    message: string;
    reply: string;
  }): Promise<tilo.Run | void>;
  recordWork(args: {
    message: string;
    work: WorkUpdate;
    say?: string;
  }): Promise<tilo.Run | void>;
  carryCounts(args: {
    earlier: Record<string, Record<string, number>>;
  }): Promise<tilo.Run | void>;
  streamOutput(args: {
    channel: string;
    reply: string;
    event: unknown;
    name?: string;
  }): Promise<tilo.Run | void>;
}

/** The `chat.messages` kind at `path`. */
export function messages(ws: tilo.Workspace, path: string): Messages {
  return resourceProxy(ws, path) as unknown as Messages;
}

/** Chat drafts — `chat.drafts`. */
export interface Drafts extends tilo.Resource {
  drafts: Record<string, string>;
  startDraft(args: {
    key: string;
  }): Promise<tilo.Run | void>;
}

/** The `chat.drafts` kind at `path`. */
export function drafts(ws: tilo.Workspace, path: string): Drafts {
  return resourceProxy(ws, path) as unknown as Drafts;
}

/** Workspace emoji — `tilo.workspace.emoji`. */
export interface WorkspaceEmoji extends tilo.Resource {
  emoji: Record<string, Emoji>;
  addEmoji(args: {
    name: string;
    image: string;
  }): Promise<tilo.Run | void>;
  removeEmoji(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  removeAnyEmoji(args: {
    name: string;
  }): Promise<tilo.Run | void>;
}

/** The `tilo.workspace.emoji` kind at `path`. */
export function workspaceEmoji(ws: tilo.Workspace, path: string): WorkspaceEmoji {
  return resourceProxy(ws, path) as unknown as WorkspaceEmoji;
}

/** Agents — `chat.agent`. */
export interface ChatAgent extends tilo.Resource {
  agents: Agent[];
  createAgent(args: {
    name: string;
    keeper?: string;
    id?: string;
    model?: string;
    colour?: number;
  }): Promise<tilo.Run | void>;
  setAgentSpendLimit(args: {
    agent: string;
    limit_micros?: number;
  }): Promise<tilo.Run | void>;
  recordAgentSpend(args: {
    agent: string;
    day: string;
    cost_micros: number;
  }): Promise<tilo.Run | void>;
  setAgentColour(args: {
    agent: string;
    colour: number;
  }): Promise<tilo.Run | void>;
  renameAgent(args: {
    agent: string;
    name: string;
  }): Promise<tilo.Run | void>;
  setAgentModel(args: {
    agent: string;
    model?: string;
  }): Promise<tilo.Run | void>;
  setAgentBound(args: {
    agent: string;
    bound: Bound[];
  }): Promise<tilo.Run | void>;
  retireAgent(args: {
    agent: string;
  }): Promise<tilo.Run | void>;
}

/** The `chat.agent` kind at `path`. */
export function chatAgent(ws: tilo.Workspace, path: string): ChatAgent {
  return resourceProxy(ws, path) as unknown as ChatAgent;
}

