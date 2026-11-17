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
export const SURFACE = "so.tilo.chat@38c6b58b5e923b5b";

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
  triage: Array<tilo.Entry<Triage>>;
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
  reports: Array<tilo.Entry<Report>>;
  decisions: Array<tilo.Entry<Decision>>;
}

/** One of "done", "refused", "failed". */
export type Outcome = "done" | "refused" | "failed";

/** One of "no-action", "restrict", "ban". */
export type Outcome2 = "no-action" | "restrict" | "ban";

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

/** One of "Chars", "Bytes". */
export type SpanUnit = "Chars" | "Bytes";

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

/** One of Item, Items, Text: an object whose case the app tags it with. */
export type Anchor = Record<string, unknown>;

export interface Answered {
  answer: string;
  device: string;
  at: number;
  detail: string;
}

/** One of resource, text, image, video, file: an object whose case the app tags it with. */
export type Attached = Record<string, unknown>;

export interface Cause {
  kind: string;
  label: string;
  key: string;
  person: string;
}

export interface Choice {
  id: string;
  label: string;
}

/** One of Bool, Int, Str: an object whose case the app tags it with. */
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

export interface Earlier {
  version: number;
  source: string;
  reads: string[];
  at: number;
}

export interface MarkSpan {
  start: number;
  end: number;
  name: string;
  value: MarkValue;
}

/** One of Bool, Int, Str: an object whose case the app tags it with. */
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
  question: Question | null;
  cause: Cause | null;
  stopped: Stopped | null;
  view: View | null;
  attempts: number;
}

export interface NewMessage {
  channel: string;
  text: string;
  parent: string | null;
  quoted: Quote | null;
  body: ClipBlock[] | null;
  attachments: Attached[];
  mentions: Mention[] | null;
  written: number | null;
  origin: Origin | null;
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

export interface MessagesOutcome {
  outcome: string;
  at: number;
  detail: string;
}

export interface ProviderStream {
  run: string;
  sequence: number;
  reasoning: string;
  progress: string;
  finish: AgentFinish | null;
  detail: string;
}

export interface Question {
  kind: string;
  person: string;
  answers: Choice[];
  detail: string;
  answered: Answered | null;
  outcome: MessagesOutcome | null;
  withdrawn: Withdrawn | null;
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

export interface Starting {
  kind: string;
  label: string;
  key: string;
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

export interface Stopped {
  attempt: number;
  at: number;
}

export interface Transition {
  state: AgentWorkState;
  since: number;
}

export interface View {
  source: string;
  reads: string[];
  version: number;
  at: number;
  earlier: Earlier[];
}

export interface WindowShare {
  access: ShareAccess;
  open: boolean;
  ended: boolean;
}

export interface Withdrawn {
  at: number;
  why: string;
}

export interface Work {
  stream: ProviderStream | null;
  state: Transition | null;
  trace: Array<tilo.Entry<Step>>;
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

export interface Fact {
  text: string;
  source: string;
  at: number;
  by: string | null;
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
  on_host: boolean;
  model: string | null;
  mcp_client: string | null;
  bound: Bound[];
  spend_limit_micros: number | null;
  spent: Spent | null;
  state: AgentState;
  created_at: number;
  instruction: string | null;
  sub: Sub | null;
  sub_depth: number | null;
  sub_at_once: number | null;
}

/** One of "active", "retired". */
export type AgentState = "active" | "retired";

export interface Bound {
  node: string;
  kinds: string[];
  answer: string;
}

export interface Coding {
  harness: string;
  harness_name: string;
  repository: string;
  directory: string;
  branch: string;
  base: string;
  session: string;
  process: CodingProcess | null;
  spent_micros: number;
}

export interface CodingHarness {
  id: string;
  name: string;
}

export interface CodingProcess {
  started_ms: number;
  ended_ms: number | null;
  ended: string;
  exit: number | null;
  words: string;
  stopped_by: string;
  command: string;
}

export interface NewCoding {
  harness: string | null;
  repository: string;
  branch: string | null;
  base: string | null;
}

export interface NewSubAgent {
  name: string;
  job: string;
  role: string | null;
  instruction: string | null;
  kind: string | null;
  model: string | null;
  task: string | null;
  id: string | null;
  coding: NewCoding | null;
}

export interface Note {
  id: string;
  by: string;
  text: string;
  at: number;
}

export interface Produced {
  id: string;
  kind: string;
  label: string;
  url: string | null;
  at: ProducedAt | null;
  at_ms: number;
}

export interface ProducedAt {
  app: string;
  document: string;
  resource: string;
}

export interface Spent {
  day: string;
  cost_micros: number;
}

/** One of "working", "needs_person", "done", "failed", "stopped". */
export type Stands = "working" | "needs_person" | "done" | "failed" | "stopped";

export interface Sub {
  started_by: string;
  job: string;
  role: string;
  kind: string;
  runs: string;
  depth: number;
  task: string | null;
  conversation: string;
  stands: Stands;
  since: number;
  line: string;
  notes: Note[];
  produced: Produced[];
  coding: Coding | null;
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
  person: unknown;
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
  through: string;
  through_ts: number;
  text: string;
  conversation: string;
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
  quoted?: Quote;
  body?: ClipBlock[];
  attachments?: Attached[];
  mentions?: Mention[];
  written?: number;
  origin?: Origin;
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

/** The arguments of `ask`. */
export interface AskArgs {
  channel: string;
  text: string;
  answers?: Choice[];
  kind?: string;
  detail?: unknown;
  parent?: string;
}

/** The arguments of `answer-question`. */
export interface AnswerQuestionArgs {
  message: string;
  answer: string;
  device?: string;
  detail?: unknown;
}

/** The arguments of `withdraw-question`. */
export interface WithdrawQuestionArgs {
  message: string;
  why?: string;
}

/** The arguments of `settle-question`. */
export interface SettleQuestionArgs {
  message: string;
  outcome: string;
  detail?: string;
}

/** The arguments of `ask-agent`. */
export interface AskAgentArgs {
  text: string;
  channel: string;
  cause: Starting;
  members?: unknown[];
}

/** The arguments of `stop-agent`. */
export interface StopAgentArgs {
  message: string;
}

/** The arguments of `post-view`. */
export interface PostViewArgs {
  channel: string;
  text: string;
  source: string;
  reads: string[];
  parent?: string;
}

/** The arguments of `answer-with-view`. */
export interface AnswerWithViewArgs {
  message: string;
  source: string;
  reads: string[];
  say?: string;
}

/** The arguments of `rewrite-view`. */
export interface RewriteViewArgs {
  message: string;
  source: string;
  reads: string[];
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

/** The arguments of `learn`. */
export interface LearnArgs {
  text: string;
  source: string;
}

/** The arguments of `correct`. */
export interface CorrectArgs {
  fact: string;
  text: string;
  source?: string;
}

/** The arguments of `forget`. */
export interface ForgetArgs {
  fact: string;
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

/** The arguments of `set-agent-on-host`. */
export interface SetAgentOnHostArgs {
  agent: string;
  on_host: boolean;
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

/** The arguments of `start-sub-agent`. */
export interface StartSubAgentArgs {
  name: string;
  job: string;
  role?: string;
  instruction?: string;
  kind?: string;
  model?: string;
  task?: string;
  id?: string;
  coding?: NewCoding;
}

/** The arguments of `note-sub-agent`. */
export interface NoteSubAgentArgs {
  agent: string;
  text: string;
}

/** The arguments of `stop-sub-agent`. */
export interface StopSubAgentArgs {
  agent: string;
}

/** The arguments of `sub-agent-stands`. */
export interface SubAgentStandsArgs {
  agent: string;
  stands: Stands;
  line?: string;
}

/** The arguments of `sub-agent-produced`. */
export interface SubAgentProducedArgs {
  agent: string;
  kind: string;
  label: string;
  url?: string;
  at?: ProducedAt;
}

/** The arguments of `sub-agent-coding`. */
export interface SubAgentCodingArgs {
  agent: string;
  process?: CodingProcess;
  session?: string;
  spent_micros?: number;
  harness?: CodingHarness;
}

/** The arguments of `set-sub-agent-limits`. */
export interface SetSubAgentLimitsArgs {
  agent: string;
  depth?: number;
  at_once?: number;
}

/** Chat — `chat.workspace`. */
export interface Workspace extends tilo.Resource {
  readonly categories: Array<tilo.Entry<Category>>;
  readonly channels: Array<tilo.Entry<Conversation>>;
  people: Array<tilo.Entry<Person>>;
  read: Record<string, Record<string, ReadMark>>;
  feedback: Array<tilo.Entry<FeedbackReport>>;
  moderation: Moderation;
  readonly files: WorkspaceFiles;
  readonly summaries: Record<string, Record<string, Summary>>;
  beginChat(args: {
    name: string;
    initials: string;
    color: number;
  }): Promise<void>;
  /** Runs `begin-chat` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  beginChatMany(items: BeginChatArgs[]): Promise<tilo.ActionAnswer[]>;
  introduce(args: {
    name: string;
    initials: string;
    color: number;
  }): Promise<void>;
  /** Runs `introduce` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  introduceMany(items: IntroduceArgs[]): Promise<tilo.ActionAnswer[]>;
  createCategory(args: {
    name: string;
  }): Promise<void>;
  /** Runs `create-category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  createCategoryMany(items: CreateCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  createChannel(args: {
    name: string;
    category?: string;
    kind?: ChannelKind;
    topic?: string;
  }): Promise<void>;
  /** Runs `create-channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  createChannelMany(items: CreateChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  createChannelWithId(args: {
    id: string;
    name: string;
    category?: string;
    kind?: ChannelKind;
    topic?: string;
    members?: unknown[];
  }): Promise<void>;
  /** Runs `create-channel-with-id` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  createChannelWithIdMany(items: CreateChannelWithIdArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteCategory(args: {
    category: string;
  }): Promise<void>;
  /** Runs `delete-category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteCategoryMany(items: DeleteCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  moveChannel(args: {
    channel: string;
    up: boolean;
  }): Promise<void>;
  /** Runs `move-channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  moveChannelMany(items: MoveChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  moveCategory(args: {
    category: string;
    up: boolean;
  }): Promise<void>;
  /** Runs `move-category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  moveCategoryMany(items: MoveCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  arrangeChannels(args: {
    categories: string[];
    channels: ChannelPlace[];
  }): Promise<void>;
  /** Runs `arrange-channels` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  arrangeChannelsMany(items: ArrangeChannelsArgs[]): Promise<tilo.ActionAnswer[]>;
  setChannelCategory(args: {
    channel: string;
    category?: string;
  }): Promise<void>;
  /** Runs `set-channel-category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setChannelCategoryMany(items: SetChannelCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  renameChannel(args: {
    channel: string;
    name: string;
  }): Promise<void>;
  /** Runs `rename-channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameChannelMany(items: RenameChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  setChannelTopic(args: {
    channel: string;
    topic: string;
  }): Promise<void>;
  /** Runs `set-channel-topic` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setChannelTopicMany(items: SetChannelTopicArgs[]): Promise<tilo.ActionAnswer[]>;
  archiveChannel(args: {
    channel: string;
    archived: boolean;
  }): Promise<void>;
  /** Runs `archive-channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  archiveChannelMany(items: ArchiveChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  renameCategory(args: {
    category: string;
    name: string;
  }): Promise<void>;
  /** Runs `rename-category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameCategoryMany(items: RenameCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteChannel(args: {
    channel: string;
  }): Promise<void>;
  /** Runs `delete-channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteChannelMany(items: DeleteChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  reportMessage(args: {
    message: string;
    author: unknown;
    reason: string;
  }): Promise<void>;
  /** Runs `report-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reportMessageMany(items: ReportMessageArgs[]): Promise<tilo.ActionAnswer[]>;
  reportPerson(args: {
    person: string;
    reason: string;
  }): Promise<void>;
  /** Runs `report-person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reportPersonMany(items: ReportPersonArgs[]): Promise<tilo.ActionAnswer[]>;
  decideReports(args: {
    id: string;
    person: unknown;
    reports: string[];
    ruling: Ruling;
  }): Promise<void>;
  /** Runs `decide-reports` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  decideReportsMany(items: DecideReportsArgs[]): Promise<tilo.ActionAnswer[]>;
  overturnDecision(args: {
    decision: string;
    note: string;
  }): Promise<void>;
  /** Runs `overturn-decision` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  overturnDecisionMany(items: OverturnDecisionArgs[]): Promise<tilo.ActionAnswer[]>;
  markRead(args: {
    channel: string;
    part: number;
    seen: number;
  }): Promise<void>;
  /** Runs `mark-read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  markReadMany(items: MarkReadArgs[]): Promise<tilo.ActionAnswer[]>;
  keepSummary(args: {
    through: string;
    through_ts: number;
    text: string;
    conversation: string;
  }): Promise<void>;
  /** Runs `keep-summary` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keepSummaryMany(items: KeepSummaryArgs[]): Promise<tilo.ActionAnswer[]>;
  submitFeedback(args: {
    channel: string;
    submission: unknown;
  }): Promise<void>;
  /** Runs `submit-feedback` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  submitFeedbackMany(items: SubmitFeedbackArgs[]): Promise<tilo.ActionAnswer[]>;
  triage(args: {
    report: string;
    update: unknown;
  }): Promise<void>;
  /** Runs `triage` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
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

/** The actions `Workspace` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKSPACE_ACTIONS = ["beginChat", "beginChatMany", "introduce", "introduceMany", "createCategory", "createCategoryMany", "createChannel", "createChannelMany", "createChannelWithId", "createChannelWithIdMany", "deleteCategory", "deleteCategoryMany", "moveChannel", "moveChannelMany", "moveCategory", "moveCategoryMany", "arrangeChannels", "arrangeChannelsMany", "setChannelCategory", "setChannelCategoryMany", "renameChannel", "renameChannelMany", "setChannelTopic", "setChannelTopicMany", "archiveChannel", "archiveChannelMany", "renameCategory", "renameCategoryMany", "deleteChannel", "deleteChannelMany", "reportMessage", "reportMessageMany", "reportPerson", "reportPersonMany", "decideReports", "decideReportsMany", "overturnDecision", "overturnDecisionMany", "markRead", "markReadMany", "keepSummary", "keepSummaryMany", "submitFeedback", "submitFeedbackMany", "triage", "triageMany", "exportContent", "startCall", "selectInput"] as const;

/** The `chat.workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path, SURFACE, WORKSPACE_ACTIONS) as unknown as Workspace;
}

/** Messages — `chat.messages`. */
export interface Messages extends tilo.Resource {
  readonly messages: Array<tilo.Entry<Message>>;
  readonly earlier: Record<string, Record<string, number>>;
  post(args: {
    channel: string;
    text: string;
    parent?: string;
    quoted?: Quote;
    body?: ClipBlock[];
    attachments?: Attached[];
    mentions?: Mention[];
    written?: number;
    origin?: Origin;
  }): Promise<void>;
  /** Runs `post` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  postMany(items: PostArgs[]): Promise<tilo.ActionAnswer[]>;
  postFile(args: {
    channel: string;
    content: string;
    name: string;
    text?: string;
    attachment?: Attached;
    mentions?: Mention[];
  }): Promise<void>;
  /** Runs `post-file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  postFileMany(items: PostFileArgs[]): Promise<tilo.ActionAnswer[]>;
  shareResource(args: {
    channel: string;
    attached: ResourceAt;
    text?: string;
    mentions?: Mention[];
  }): Promise<void>;
  /** Runs `share-resource` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  shareResourceMany(items: ShareResourceArgs[]): Promise<tilo.ActionAnswer[]>;
  shareWindow(args: {
    channel: string;
    attached: ResourceAt;
    access?: ShareAccess;
    text?: string;
    mentions?: Mention[];
  }): Promise<void>;
  /** Runs `share-window` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  shareWindowMany(items: ShareWindowArgs[]): Promise<tilo.ActionAnswer[]>;
  stopSharing(args: {
    message: string;
  }): Promise<void>;
  /** Runs `stop-sharing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopSharingMany(items: StopSharingArgs[]): Promise<tilo.ActionAnswer[]>;
  setShareAccess(args: {
    message: string;
    access: ShareAccess;
  }): Promise<void>;
  /** Runs `set-share-access` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setShareAccessMany(items: SetShareAccessArgs[]): Promise<tilo.ActionAnswer[]>;
  shareOpen(args: {
    message: string;
    open: boolean;
  }): Promise<void>;
  /** Runs `share-open` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  shareOpenMany(items: ShareOpenArgs[]): Promise<tilo.ActionAnswer[]>;
  deliverMessage(args: {
    channel: string;
    author: string;
    body: string;
    ts?: number;
  }): Promise<void>;
  /** Runs `deliver-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deliverMessageMany(items: DeliverMessageArgs[]): Promise<tilo.ActionAnswer[]>;
  editMessage(args: {
    message: string;
    text: string;
    body?: ClipBlock[];
    mentions?: Mention[];
  }): Promise<void>;
  /** Runs `edit-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editMessageMany(items: EditMessageArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteMessage(args: {
    message: string;
  }): Promise<void>;
  /** Runs `delete-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMessageMany(items: DeleteMessageArgs[]): Promise<tilo.ActionAnswer[]>;
  removeMessage(args: {
    message: string;
  }): Promise<void>;
  /** Runs `remove-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeMessageMany(items: RemoveMessageArgs[]): Promise<tilo.ActionAnswer[]>;
  pin(args: {
    message: string;
    pinned: boolean;
  }): Promise<void>;
  /** Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pinMany(items: PinArgs[]): Promise<tilo.ActionAnswer[]>;
  retry(args: {
    message: string;
    reply: string;
  }): Promise<void>;
  /** Runs `retry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  retryMany(items: RetryArgs[]): Promise<tilo.ActionAnswer[]>;
  recordWork(args: {
    message: string;
    work: WorkUpdate;
    say?: string;
  }): Promise<void>;
  /** Runs `record-work` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordWorkMany(items: RecordWorkArgs[]): Promise<tilo.ActionAnswer[]>;
  ask(args: {
    channel: string;
    text: string;
    answers?: Choice[];
    kind?: string;
    detail?: unknown;
    parent?: string;
  }): Promise<void>;
  /** Runs `ask` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  askMany(items: AskArgs[]): Promise<tilo.ActionAnswer[]>;
  answerQuestion(args: {
    message: string;
    answer: string;
    device?: string;
    detail?: unknown;
  }): Promise<void>;
  /** Runs `answer-question` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  answerQuestionMany(items: AnswerQuestionArgs[]): Promise<tilo.ActionAnswer[]>;
  withdrawQuestion(args: {
    message: string;
    why?: string;
  }): Promise<void>;
  /** Runs `withdraw-question` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  withdrawQuestionMany(items: WithdrawQuestionArgs[]): Promise<tilo.ActionAnswer[]>;
  settleQuestion(args: {
    message: string;
    outcome: string;
    detail?: string;
  }): Promise<void>;
  /** Runs `settle-question` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  settleQuestionMany(items: SettleQuestionArgs[]): Promise<tilo.ActionAnswer[]>;
  askAgent(args: {
    text: string;
    channel: string;
    cause: Starting;
    members?: unknown[];
  }): Promise<void>;
  /** Runs `ask-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  askAgentMany(items: AskAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  stopAgent(args: {
    message: string;
  }): Promise<void>;
  /** Runs `stop-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopAgentMany(items: StopAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  postView(args: {
    channel: string;
    text: string;
    source: string;
    reads: string[];
    parent?: string;
  }): Promise<void>;
  /** Runs `post-view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  postViewMany(items: PostViewArgs[]): Promise<tilo.ActionAnswer[]>;
  answerWithView(args: {
    message: string;
    source: string;
    reads: string[];
    say?: string;
  }): Promise<void>;
  /** Runs `answer-with-view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  answerWithViewMany(items: AnswerWithViewArgs[]): Promise<tilo.ActionAnswer[]>;
  rewriteView(args: {
    message: string;
    source: string;
    reads: string[];
  }): Promise<void>;
  /** Runs `rewrite-view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rewriteViewMany(items: RewriteViewArgs[]): Promise<tilo.ActionAnswer[]>;
  carryCounts(args: {
    earlier: Record<string, Record<string, number>>;
  }): Promise<void>;
  /** Runs `carry-counts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  carryCountsMany(items: CarryCountsArgs[]): Promise<tilo.ActionAnswer[]>;
  streamOutput(args: {
    channel: string;
    reply: string;
    event: unknown;
    name?: string;
  }): Promise<void>;
  /** Runs `stream-output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  streamOutputMany(items: StreamOutputArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Messages` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MESSAGES_ACTIONS = ["post", "postMany", "postFile", "postFileMany", "shareResource", "shareResourceMany", "shareWindow", "shareWindowMany", "stopSharing", "stopSharingMany", "setShareAccess", "setShareAccessMany", "shareOpen", "shareOpenMany", "deliverMessage", "deliverMessageMany", "editMessage", "editMessageMany", "deleteMessage", "deleteMessageMany", "removeMessage", "removeMessageMany", "pin", "pinMany", "retry", "retryMany", "recordWork", "recordWorkMany", "ask", "askMany", "answerQuestion", "answerQuestionMany", "withdrawQuestion", "withdrawQuestionMany", "settleQuestion", "settleQuestionMany", "askAgent", "askAgentMany", "stopAgent", "stopAgentMany", "postView", "postViewMany", "answerWithView", "answerWithViewMany", "rewriteView", "rewriteViewMany", "carryCounts", "carryCountsMany", "streamOutput", "streamOutputMany"] as const;

/** The `chat.messages` kind at `path`. */
export function messages(ws: tilo.Workspace, path: string): Messages {
  return resourceProxy(ws, path, SURFACE, MESSAGES_ACTIONS) as unknown as Messages;
}

/** Chat drafts — `chat.drafts`. */
export interface Drafts extends tilo.Resource {
  drafts: Record<string, string>;
  startDraft(args: {
    key: string;
  }): Promise<void>;
  /** Runs `start-draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  startDraftMany(items: StartDraftArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Drafts` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DRAFTS_ACTIONS = ["startDraft", "startDraftMany"] as const;

/** The `chat.drafts` kind at `path`. */
export function drafts(ws: tilo.Workspace, path: string): Drafts {
  return resourceProxy(ws, path, SURFACE, DRAFTS_ACTIONS) as unknown as Drafts;
}

/** What your agents learned — `chat.learned`. */
export interface Learned extends tilo.Resource {
  readonly facts: Array<tilo.Entry<Fact>>;
  learn(args: {
    text: string;
    source: string;
  }): Promise<void>;
  /** Runs `learn` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  learnMany(items: LearnArgs[]): Promise<tilo.ActionAnswer[]>;
  correct(args: {
    fact: string;
    text: string;
    source?: string;
  }): Promise<void>;
  /** Runs `correct` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  correctMany(items: CorrectArgs[]): Promise<tilo.ActionAnswer[]>;
  forget(args: {
    fact: string;
  }): Promise<void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Learned` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LEARNED_ACTIONS = ["learn", "learnMany", "correct", "correctMany", "forget", "forgetMany"] as const;

/** The `chat.learned` kind at `path`. */
export function learned(ws: tilo.Workspace, path: string): Learned {
  return resourceProxy(ws, path, SURFACE, LEARNED_ACTIONS) as unknown as Learned;
}

/** Workspace emoji — `tilo.workspace.emoji`. */
export interface WorkspaceEmoji extends tilo.Resource {
  emoji: Record<string, Emoji>;
  addEmoji(args: {
    name: string;
    image: string;
  }): Promise<void>;
  /** Runs `add-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addEmojiMany(items: AddEmojiArgs[]): Promise<tilo.ActionAnswer[]>;
  removeEmoji(args: {
    name: string;
  }): Promise<void>;
  /** Runs `remove-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeEmojiMany(items: RemoveEmojiArgs[]): Promise<tilo.ActionAnswer[]>;
  removeAnyEmoji(args: {
    name: string;
  }): Promise<void>;
  /** Runs `remove-any-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeAnyEmojiMany(items: RemoveAnyEmojiArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `WorkspaceEmoji` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKSPACEEMOJI_ACTIONS = ["addEmoji", "addEmojiMany", "removeEmoji", "removeEmojiMany", "removeAnyEmoji", "removeAnyEmojiMany"] as const;

/** The `tilo.workspace.emoji` kind at `path`. */
export function workspaceEmoji(ws: tilo.Workspace, path: string): WorkspaceEmoji {
  return resourceProxy(ws, path, SURFACE, WORKSPACEEMOJI_ACTIONS) as unknown as WorkspaceEmoji;
}

/** Agents — `chat.agent`. */
export interface ChatAgent extends tilo.Resource {
  agents: Array<tilo.Entry<Agent>>;
  createAgent(args: {
    name: string;
    keeper?: string;
    id?: string;
    model?: string;
    colour?: number;
    mcp_client?: string;
  }): Promise<void>;
  /** Runs `create-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  createAgentMany(items: CreateAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentSpendLimit(args: {
    agent: string;
    limit_micros?: number;
  }): Promise<void>;
  /** Runs `set-agent-spend-limit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setAgentSpendLimitMany(items: SetAgentSpendLimitArgs[]): Promise<tilo.ActionAnswer[]>;
  recordAgentSpend(args: {
    agent: string;
    day: string;
    cost_micros: number;
  }): Promise<void>;
  /** Runs `record-agent-spend` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordAgentSpendMany(items: RecordAgentSpendArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentOnHost(args: {
    agent: string;
    on_host: boolean;
  }): Promise<void>;
  /** Runs `set-agent-on-host` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setAgentOnHostMany(items: SetAgentOnHostArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentColour(args: {
    agent: string;
    colour: number;
  }): Promise<void>;
  /** Runs `set-agent-colour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setAgentColourMany(items: SetAgentColourArgs[]): Promise<tilo.ActionAnswer[]>;
  renameAgent(args: {
    agent: string;
    name: string;
  }): Promise<void>;
  /** Runs `rename-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameAgentMany(items: RenameAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentModel(args: {
    agent: string;
    model?: string;
  }): Promise<void>;
  /** Runs `set-agent-model` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setAgentModelMany(items: SetAgentModelArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentInstruction(args: {
    agent: string;
    instruction?: string;
  }): Promise<void>;
  /** Runs `set-agent-instruction` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setAgentInstructionMany(items: SetAgentInstructionArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentBound(args: {
    agent: string;
    bound: Bound[];
  }): Promise<void>;
  /** Runs `set-agent-bound` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setAgentBoundMany(items: SetAgentBoundArgs[]): Promise<tilo.ActionAnswer[]>;
  retireAgent(args: {
    agent: string;
  }): Promise<void>;
  /** Runs `retire-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  retireAgentMany(items: RetireAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  startSubAgent(args: {
    name: string;
    job: string;
    role?: string;
    instruction?: string;
    kind?: string;
    model?: string;
    task?: string;
    id?: string;
    coding?: NewCoding;
  }): Promise<void>;
  /** Runs `start-sub-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  startSubAgentMany(items: StartSubAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  noteSubAgent(args: {
    agent: string;
    text: string;
  }): Promise<void>;
  /** Runs `note-sub-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  noteSubAgentMany(items: NoteSubAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  stopSubAgent(args: {
    agent: string;
  }): Promise<void>;
  /** Runs `stop-sub-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopSubAgentMany(items: StopSubAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  subAgentStands(args: {
    agent: string;
    stands: Stands;
    line?: string;
  }): Promise<void>;
  /** Runs `sub-agent-stands` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  subAgentStandsMany(items: SubAgentStandsArgs[]): Promise<tilo.ActionAnswer[]>;
  subAgentProduced(args: {
    agent: string;
    kind: string;
    label: string;
    url?: string;
    at?: ProducedAt;
  }): Promise<void>;
  /** Runs `sub-agent-produced` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  subAgentProducedMany(items: SubAgentProducedArgs[]): Promise<tilo.ActionAnswer[]>;
  subAgentCoding(args: {
    agent: string;
    process?: CodingProcess;
    session?: string;
    spent_micros?: number;
    harness?: CodingHarness;
  }): Promise<void>;
  /** Runs `sub-agent-coding` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  subAgentCodingMany(items: SubAgentCodingArgs[]): Promise<tilo.ActionAnswer[]>;
  setSubAgentLimits(args: {
    agent: string;
    depth?: number;
    at_once?: number;
  }): Promise<void>;
  /** Runs `set-sub-agent-limits` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setSubAgentLimitsMany(items: SetSubAgentLimitsArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `ChatAgent` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHATAGENT_ACTIONS = ["createAgent", "createAgentMany", "setAgentSpendLimit", "setAgentSpendLimitMany", "recordAgentSpend", "recordAgentSpendMany", "setAgentOnHost", "setAgentOnHostMany", "setAgentColour", "setAgentColourMany", "renameAgent", "renameAgentMany", "setAgentModel", "setAgentModelMany", "setAgentInstruction", "setAgentInstructionMany", "setAgentBound", "setAgentBoundMany", "retireAgent", "retireAgentMany", "startSubAgent", "startSubAgentMany", "noteSubAgent", "noteSubAgentMany", "stopSubAgent", "stopSubAgentMany", "subAgentStands", "subAgentStandsMany", "subAgentProduced", "subAgentProducedMany", "subAgentCoding", "subAgentCodingMany", "setSubAgentLimits", "setSubAgentLimitsMany"] as const;

/** The `chat.agent` kind at `path`. */
export function chatAgent(ws: tilo.Workspace, path: string): ChatAgent {
  return resourceProxy(ws, path, SURFACE, CHATAGENT_ACTIONS) as unknown as ChatAgent;
}

