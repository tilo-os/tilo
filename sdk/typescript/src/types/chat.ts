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
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.chat@936a822ab04182d5";

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
  members: string[];
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
  workspace: string;
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

export interface Summary {
  text: string;
  through: string;
  through_ts: number;
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
  quoted: Quote | null;
  reactions: unknown;
  mentions: Mention[];
  attachments: Attached[];
  pinned: boolean;
  work: Work | null;
  appearance: ShownAs | null;
  acting: unknown | null;
  author_name: string | null;
  origin: Origin | null;
  share: WindowShare | null;
  attempts: number;
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

export interface ProviderStream {
  run: string;
  sequence: number;
  reasoning: string;
  progress: string;
  finish: AgentFinish | null;
  detail: string;
}

export interface Quote {
  text: string;
  start: number;
  end: number;
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
  failure_detail: string | null;
}

export interface WorkUpdate {
  state: string;
  app: string | null;
  did: string | null;
  about: string | null;
  trace: StepUpdate[];
  detail: string | null;
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
  mcp_client: string | null;
  bound: Bound[];
  spend_limit_micros: number | null;
  spent: Spent | null;
  state: AgentState;
  created_at: number;
  instruction: string | null;
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

/** The arguments of `begin-chat`. */
export interface BeginChatArgs {
  name: string;
  initials: string;
  color: number;
}

/** The arguments of `introduce`. */
export interface IntroduceArgs {
  name: string;
  initials: string;
  color: number;
}

/** The arguments of `create-category`. */
export interface CreateCategoryArgs {
  name: string;
}

/** The arguments of `create-channel`. */
export interface CreateChannelArgs {
  name: string;
  category?: string;
  kind?: ChannelKind;
  topic?: string;
}

/** The arguments of `create-channel-with-id`. */
export interface CreateChannelWithIdArgs {
  id: string;
  name: string;
  category?: string;
  kind?: ChannelKind;
  topic?: string;
  members?: unknown[];
}

/** The arguments of `delete-category`. */
export interface DeleteCategoryArgs {
  category: string;
}

/** The arguments of `move-channel`. */
export interface MoveChannelArgs {
  channel: string;
  up: boolean;
}

/** The arguments of `move-category`. */
export interface MoveCategoryArgs {
  category: string;
  up: boolean;
}

/** The arguments of `arrange-channels`. */
export interface ArrangeChannelsArgs {
  categories: string[];
  channels: ChannelPlace[];
}

/** The arguments of `set-channel-category`. */
export interface SetChannelCategoryArgs {
  channel: string;
  category?: string;
}

/** The arguments of `rename-channel`. */
export interface RenameChannelArgs {
  channel: string;
  name: string;
}

/** The arguments of `set-channel-topic`. */
export interface SetChannelTopicArgs {
  channel: string;
  topic: string;
}

/** The arguments of `archive-channel`. */
export interface ArchiveChannelArgs {
  channel: string;
  archived: boolean;
}

/** The arguments of `rename-category`. */
export interface RenameCategoryArgs {
  category: string;
  name: string;
}

/** The arguments of `delete-channel`. */
export interface DeleteChannelArgs {
  channel: string;
}

/** The arguments of `report-message`. */
export interface ReportMessageArgs {
  message: string;
  author: unknown;
  reason: string;
}

/** The arguments of `report-person`. */
export interface ReportPersonArgs {
  person: string;
  reason: string;
}

/** The arguments of `decide-reports`. */
export interface DecideReportsArgs {
  id: string;
  person: string;
  reports: string[];
  ruling: Ruling;
}

/** The arguments of `overturn-decision`. */
export interface OverturnDecisionArgs {
  decision: string;
  note: string;
}

/** The arguments of `mark-read`. */
export interface MarkReadArgs {
  channel: string;
  part: number;
  seen: number;
}

/** The arguments of `keep-summary`. */
export interface KeepSummaryArgs {
  channel: string;
  through: string;
  through_ts: number;
  text: string;
}

/** The arguments of `submit-feedback`. */
export interface SubmitFeedbackArgs {
  channel: string;
  submission: unknown;
}

/** The arguments of `triage`. */
export interface TriageArgs {
  report: string;
  update: unknown;
}

/** The arguments of `post`. */
export interface PostArgs {
  channel: string;
  text: string;
  parent?: string;
  quoted?: unknown;
  body?: unknown[];
  attachments?: unknown[];
  mentions?: unknown[];
  written?: number;
  origin?: unknown;
}

/** The arguments of `post-file`. */
export interface PostFileArgs {
  channel: string;
  content: string;
  name: string;
  text?: string;
  attachment?: Attached;
  mentions?: Mention[];
}

/** The arguments of `share-resource`. */
export interface ShareResourceArgs {
  channel: string;
  attached: ResourceAt;
  text?: string;
  mentions?: Mention[];
}

/** The arguments of `share-window`. */
export interface ShareWindowArgs {
  channel: string;
  attached: ResourceAt;
  access?: ShareAccess;
  text?: string;
  mentions?: Mention[];
}

/** The arguments of `stop-sharing`. */
export interface StopSharingArgs {
  message: string;
}

/** The arguments of `set-share-access`. */
export interface SetShareAccessArgs {
  message: string;
  access: ShareAccess;
}

/** The arguments of `share-open`. */
export interface ShareOpenArgs {
  message: string;
  open: boolean;
}

/** The arguments of `deliver-message`. */
export interface DeliverMessageArgs {
  channel: string;
  author: string;
  body: string;
  ts?: number;
}

/** The arguments of `edit-message`. */
export interface EditMessageArgs {
  message: string;
  text: string;
  body?: ClipBlock[];
  mentions?: Mention[];
}

/** The arguments of `delete-message`. */
export interface DeleteMessageArgs {
  message: string;
}

/** The arguments of `remove-message`. */
export interface RemoveMessageArgs {
  message: string;
}

/** The arguments of `pin`. */
export interface PinArgs {
  message: string;
  pinned: boolean;
}

/** The arguments of `retry`. */
export interface RetryArgs {
  message: string;
  reply: string;
}

/** The arguments of `record-work`. */
export interface RecordWorkArgs {
  message: string;
  work: WorkUpdate;
  say?: string;
}

/** The arguments of `carry-counts`. */
export interface CarryCountsArgs {
  earlier: Record<string, Record<string, number>>;
}

/** The arguments of `stream-output`. */
export interface StreamOutputArgs {
  channel: string;
  reply: string;
  event: unknown;
  name?: string;
}

/** The arguments of `start-draft`. */
export interface StartDraftArgs {
  key: string;
}

/** The arguments of `add-emoji`. */
export interface AddEmojiArgs {
  name: string;
  image: string;
}

/** The arguments of `remove-emoji`. */
export interface RemoveEmojiArgs {
  name: string;
}

/** The arguments of `remove-any-emoji`. */
export interface RemoveAnyEmojiArgs {
  name: string;
}

/** The arguments of `create-agent`. */
export interface CreateAgentArgs {
  name: string;
  keeper?: string;
  id?: string;
  model?: string;
  colour?: number;
  mcp_client?: string;
}

/** The arguments of `set-agent-spend-limit`. */
export interface SetAgentSpendLimitArgs {
  agent: string;
  limit_micros?: number;
}

/** The arguments of `record-agent-spend`. */
export interface RecordAgentSpendArgs {
  agent: string;
  day: string;
  cost_micros: number;
}

/** The arguments of `set-agent-colour`. */
export interface SetAgentColourArgs {
  agent: string;
  colour: number;
}

/** The arguments of `rename-agent`. */
export interface RenameAgentArgs {
  agent: string;
  name: string;
}

/** The arguments of `set-agent-model`. */
export interface SetAgentModelArgs {
  agent: string;
  model?: string;
}

/** The arguments of `set-agent-instruction`. */
export interface SetAgentInstructionArgs {
  agent: string;
  instruction?: string;
}

/** The arguments of `set-agent-bound`. */
export interface SetAgentBoundArgs {
  agent: string;
  bound: Bound[];
}

/** The arguments of `retire-agent`. */
export interface RetireAgentArgs {
  agent: string;
}

/** Chat — `chat.workspace`. */
export interface Workspace extends tilo.Resource {
  readonly categories: Category[];
  readonly channels: Conversation[];
  people: Person[];
  read: Record<string, Record<string, ReadMark>>;
  feedback: FeedbackReport[];
  moderation: Moderation;
  readonly files: WorkspaceFiles;
  readonly summaries: Record<string, Record<string, Summary>>;
  beginChat(args: {
    name: string;
    initials: string;
    color: number;
  }): Promise<tilo.Run | void>;
  /** Runs `begin-chat` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  beginChatMany(items: BeginChatArgs[]): Promise<tilo.ActionAnswer[]>;
  introduce(args: {
    name: string;
    initials: string;
    color: number;
  }): Promise<tilo.Run | void>;
  /** Runs `introduce` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  introduceMany(items: IntroduceArgs[]): Promise<tilo.ActionAnswer[]>;
  createCategory(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `create-category` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  createCategoryMany(items: CreateCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  createChannel(args: {
    name: string;
    category?: string;
    kind?: ChannelKind;
    topic?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `create-channel` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  createChannelMany(items: CreateChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  createChannelWithId(args: {
    id: string;
    name: string;
    category?: string;
    kind?: ChannelKind;
    topic?: string;
    members?: unknown[];
  }): Promise<tilo.Run | void>;
  /** Runs `create-channel-with-id` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  createChannelWithIdMany(items: CreateChannelWithIdArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteCategory(args: {
    category: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-category` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteCategoryMany(items: DeleteCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  moveChannel(args: {
    channel: string;
    up: boolean;
  }): Promise<tilo.Run | void>;
  /** Runs `move-channel` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  moveChannelMany(items: MoveChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  moveCategory(args: {
    category: string;
    up: boolean;
  }): Promise<tilo.Run | void>;
  /** Runs `move-category` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  moveCategoryMany(items: MoveCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  arrangeChannels(args: {
    categories: string[];
    channels: ChannelPlace[];
  }): Promise<tilo.Run | void>;
  /** Runs `arrange-channels` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  arrangeChannelsMany(items: ArrangeChannelsArgs[]): Promise<tilo.ActionAnswer[]>;
  setChannelCategory(args: {
    channel: string;
    category?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `set-channel-category` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setChannelCategoryMany(items: SetChannelCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  renameChannel(args: {
    channel: string;
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `rename-channel` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  renameChannelMany(items: RenameChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  setChannelTopic(args: {
    channel: string;
    topic: string;
  }): Promise<tilo.Run | void>;
  /** Runs `set-channel-topic` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setChannelTopicMany(items: SetChannelTopicArgs[]): Promise<tilo.ActionAnswer[]>;
  archiveChannel(args: {
    channel: string;
    archived: boolean;
  }): Promise<tilo.Run | void>;
  /** Runs `archive-channel` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  archiveChannelMany(items: ArchiveChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  renameCategory(args: {
    category: string;
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `rename-category` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  renameCategoryMany(items: RenameCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteChannel(args: {
    channel: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-channel` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteChannelMany(items: DeleteChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  reportMessage(args: {
    message: string;
    author: unknown;
    reason: string;
  }): Promise<tilo.Run | void>;
  /** Runs `report-message` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  reportMessageMany(items: ReportMessageArgs[]): Promise<tilo.ActionAnswer[]>;
  reportPerson(args: {
    person: string;
    reason: string;
  }): Promise<tilo.Run | void>;
  /** Runs `report-person` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  reportPersonMany(items: ReportPersonArgs[]): Promise<tilo.ActionAnswer[]>;
  decideReports(args: {
    id: string;
    person: string;
    reports: string[];
    ruling: Ruling;
  }): Promise<tilo.Run | void>;
  /** Runs `decide-reports` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  decideReportsMany(items: DecideReportsArgs[]): Promise<tilo.ActionAnswer[]>;
  overturnDecision(args: {
    decision: string;
    note: string;
  }): Promise<tilo.Run | void>;
  /** Runs `overturn-decision` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  overturnDecisionMany(items: OverturnDecisionArgs[]): Promise<tilo.ActionAnswer[]>;
  markRead(args: {
    channel: string;
    part: number;
    seen: number;
  }): Promise<tilo.Run | void>;
  /** Runs `mark-read` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  markReadMany(items: MarkReadArgs[]): Promise<tilo.ActionAnswer[]>;
  keepSummary(args: {
    channel: string;
    through: string;
    through_ts: number;
    text: string;
  }): Promise<tilo.Run | void>;
  /** Runs `keep-summary` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  keepSummaryMany(items: KeepSummaryArgs[]): Promise<tilo.ActionAnswer[]>;
  submitFeedback(args: {
    channel: string;
    submission: unknown;
  }): Promise<tilo.Run | void>;
  /** Runs `submit-feedback` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  submitFeedbackMany(items: SubmitFeedbackArgs[]): Promise<tilo.ActionAnswer[]>;
  triage(args: {
    report: string;
    update: unknown;
  }): Promise<tilo.Run | void>;
  /** Runs `triage` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  triageMany(items: TriageArgs[]): Promise<tilo.ActionAnswer[]>;
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
  return resourceProxy(ws, path, SURFACE) as unknown as Workspace;
}

/** Messages — `chat.messages`. */
export interface Messages extends tilo.Resource {
  readonly messages: Message[];
  readonly earlier: Record<string, Record<string, number>>;
  post(args: {
    channel: string;
    text: string;
    parent?: string;
    quoted?: unknown;
    body?: unknown[];
    attachments?: unknown[];
    mentions?: unknown[];
    written?: number;
    origin?: unknown;
  }): Promise<tilo.Run | void>;
  /** Runs `post` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  postMany(items: PostArgs[]): Promise<tilo.ActionAnswer[]>;
  postFile(args: {
    channel: string;
    content: string;
    name: string;
    text?: string;
    attachment?: Attached;
    mentions?: Mention[];
  }): Promise<tilo.Run | void>;
  /** Runs `post-file` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  postFileMany(items: PostFileArgs[]): Promise<tilo.ActionAnswer[]>;
  shareResource(args: {
    channel: string;
    attached: ResourceAt;
    text?: string;
    mentions?: Mention[];
  }): Promise<tilo.Run | void>;
  /** Runs `share-resource` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  shareResourceMany(items: ShareResourceArgs[]): Promise<tilo.ActionAnswer[]>;
  shareWindow(args: {
    channel: string;
    attached: ResourceAt;
    access?: ShareAccess;
    text?: string;
    mentions?: Mention[];
  }): Promise<tilo.Run | void>;
  /** Runs `share-window` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  shareWindowMany(items: ShareWindowArgs[]): Promise<tilo.ActionAnswer[]>;
  stopSharing(args: {
    message: string;
  }): Promise<tilo.Run | void>;
  /** Runs `stop-sharing` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  stopSharingMany(items: StopSharingArgs[]): Promise<tilo.ActionAnswer[]>;
  setShareAccess(args: {
    message: string;
    access: ShareAccess;
  }): Promise<tilo.Run | void>;
  /** Runs `set-share-access` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setShareAccessMany(items: SetShareAccessArgs[]): Promise<tilo.ActionAnswer[]>;
  shareOpen(args: {
    message: string;
    open: boolean;
  }): Promise<tilo.Run | void>;
  /** Runs `share-open` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  shareOpenMany(items: ShareOpenArgs[]): Promise<tilo.ActionAnswer[]>;
  deliverMessage(args: {
    channel: string;
    author: string;
    body: string;
    ts?: number;
  }): Promise<tilo.Run | void>;
  /** Runs `deliver-message` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deliverMessageMany(items: DeliverMessageArgs[]): Promise<tilo.ActionAnswer[]>;
  editMessage(args: {
    message: string;
    text: string;
    body?: ClipBlock[];
    mentions?: Mention[];
  }): Promise<tilo.Run | void>;
  /** Runs `edit-message` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  editMessageMany(items: EditMessageArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteMessage(args: {
    message: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-message` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteMessageMany(items: DeleteMessageArgs[]): Promise<tilo.ActionAnswer[]>;
  removeMessage(args: {
    message: string;
  }): Promise<tilo.Run | void>;
  /** Runs `remove-message` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeMessageMany(items: RemoveMessageArgs[]): Promise<tilo.ActionAnswer[]>;
  pin(args: {
    message: string;
    pinned: boolean;
  }): Promise<tilo.Run | void>;
  /** Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  pinMany(items: PinArgs[]): Promise<tilo.ActionAnswer[]>;
  retry(args: {
    message: string;
    reply: string;
  }): Promise<tilo.Run | void>;
  /** Runs `retry` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  retryMany(items: RetryArgs[]): Promise<tilo.ActionAnswer[]>;
  recordWork(args: {
    message: string;
    work: WorkUpdate;
    say?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `record-work` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordWorkMany(items: RecordWorkArgs[]): Promise<tilo.ActionAnswer[]>;
  carryCounts(args: {
    earlier: Record<string, Record<string, number>>;
  }): Promise<tilo.Run | void>;
  /** Runs `carry-counts` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  carryCountsMany(items: CarryCountsArgs[]): Promise<tilo.ActionAnswer[]>;
  streamOutput(args: {
    channel: string;
    reply: string;
    event: unknown;
    name?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `stream-output` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  streamOutputMany(items: StreamOutputArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `chat.messages` kind at `path`. */
export function messages(ws: tilo.Workspace, path: string): Messages {
  return resourceProxy(ws, path, SURFACE) as unknown as Messages;
}

/** Chat drafts — `chat.drafts`. */
export interface Drafts extends tilo.Resource {
  drafts: Record<string, string>;
  startDraft(args: {
    key: string;
  }): Promise<tilo.Run | void>;
  /** Runs `start-draft` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  startDraftMany(items: StartDraftArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `chat.drafts` kind at `path`. */
export function drafts(ws: tilo.Workspace, path: string): Drafts {
  return resourceProxy(ws, path, SURFACE) as unknown as Drafts;
}

/** Workspace emoji — `tilo.workspace.emoji`. */
export interface WorkspaceEmoji extends tilo.Resource {
  emoji: Record<string, Emoji>;
  addEmoji(args: {
    name: string;
    image: string;
  }): Promise<tilo.Run | void>;
  /** Runs `add-emoji` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addEmojiMany(items: AddEmojiArgs[]): Promise<tilo.ActionAnswer[]>;
  removeEmoji(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `remove-emoji` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeEmojiMany(items: RemoveEmojiArgs[]): Promise<tilo.ActionAnswer[]>;
  removeAnyEmoji(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `remove-any-emoji` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeAnyEmojiMany(items: RemoveAnyEmojiArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `tilo.workspace.emoji` kind at `path`. */
export function workspaceEmoji(ws: tilo.Workspace, path: string): WorkspaceEmoji {
  return resourceProxy(ws, path, SURFACE) as unknown as WorkspaceEmoji;
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
    mcp_client?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `create-agent` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  createAgentMany(items: CreateAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentSpendLimit(args: {
    agent: string;
    limit_micros?: number;
  }): Promise<tilo.Run | void>;
  /** Runs `set-agent-spend-limit` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setAgentSpendLimitMany(items: SetAgentSpendLimitArgs[]): Promise<tilo.ActionAnswer[]>;
  recordAgentSpend(args: {
    agent: string;
    day: string;
    cost_micros: number;
  }): Promise<tilo.Run | void>;
  /** Runs `record-agent-spend` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordAgentSpendMany(items: RecordAgentSpendArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentColour(args: {
    agent: string;
    colour: number;
  }): Promise<tilo.Run | void>;
  /** Runs `set-agent-colour` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setAgentColourMany(items: SetAgentColourArgs[]): Promise<tilo.ActionAnswer[]>;
  renameAgent(args: {
    agent: string;
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `rename-agent` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  renameAgentMany(items: RenameAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentModel(args: {
    agent: string;
    model?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `set-agent-model` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setAgentModelMany(items: SetAgentModelArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentInstruction(args: {
    agent: string;
    instruction?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `set-agent-instruction` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setAgentInstructionMany(items: SetAgentInstructionArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentBound(args: {
    agent: string;
    bound: Bound[];
  }): Promise<tilo.Run | void>;
  /** Runs `set-agent-bound` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setAgentBoundMany(items: SetAgentBoundArgs[]): Promise<tilo.ActionAnswer[]>;
  retireAgent(args: {
    agent: string;
  }): Promise<tilo.Run | void>;
  /** Runs `retire-agent` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  retireAgentMany(items: RetireAgentArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `chat.agent` kind at `path`. */
export function chatAgent(ws: tilo.Workspace, path: string): ChatAgent {
  return resourceProxy(ws, path, SURFACE) as unknown as ChatAgent;
}

