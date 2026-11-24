/**
 * Types for Database Client (`so.tilo.db-client`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Connection` is a `Resource` of the app's `tl.so.tilo.db-client.Connection`, made by its factory with the
 * resource's path (`db-client/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.db-client@05c2dde9e2b5fec6";

/** The engine a connection targets. One of "sqlite", "postgres", "mysql". */
export type Engine = "sqlite" | "postgres" | "mysql";

/** Which of the connection's three views the window shows. One of "query", "data", "structure". */
export type Tab = "query" | "data" | "structure";

/** The arguments of `connect`. */
export interface ConnectArgs {
  c: Connection;
}

/** The arguments of `disconnect`. */
export interface DisconnectArgs {
}

/** The arguments of `add_connection`. */
export interface AddConnectionArgs {
  name: string;
  engine_index: number;
  host: string;
  port: number;
  database: string;
  user: string;
  file_path: string;
  password: string;
}

/** The arguments of `remove_connection`. */
export interface RemoveConnectionArgs {
  c: Connection;
}

/** The arguments of `remember_password`. */
export interface RememberPasswordArgs {
  c: Connection;
  password: string;
}

/** The arguments of `save_query`. */
export interface SaveQueryArgs {
  name: string;
}

/** The arguments of `remove_query`. */
export interface RemoveQueryArgs {
  q: SavedQuery;
}

/** The arguments of `load_query`. */
export interface LoadQueryArgs {
  q: SavedQuery;
}

/** The arguments of `load_starter`. */
export interface LoadStarterArgs {
  name: string;
  sql: string;
}

/** The arguments of `choose_tab`. */
export interface ChooseTabArgs {
  index: number;
}

/** The arguments of `show_tab`. */
export interface ShowTabArgs {
  tab: Tab;
}

/** The arguments of `begin_transaction`. */
export interface BeginTransactionArgs {
}

/** The arguments of `commit_transaction`. */
export interface CommitTransactionArgs {
}

/** The arguments of `rollback_transaction`. */
export interface RollbackTransactionArgs {
}

/** The arguments of `run`. */
export interface RunArgs {
}

/** The arguments of `confirm_run`. */
export interface ConfirmRunArgs {
}

/** The arguments of `dismiss_run`. */
export interface DismissRunArgs {
}

/** The arguments of `run_query`. */
export interface RunQueryArgs {
  q: SavedQuery;
}

/** The arguments of `rerun`. */
export interface RerunArgs {
  r: Run;
}

/** The arguments of `start_statement`. */
export interface StartStatementArgs {
  sql: string;
}

/** The arguments of `cancel`. */
export interface CancelArgs {
}

/** The arguments of `browse`. */
export interface BrowseArgs {
  t: TableInfo;
}

/** The arguments of `load_structure`. */
export interface LoadStructureArgs {
}

/** The arguments of `load_page`. */
export interface LoadPageArgs {
}

/** The arguments of `next_page`. */
export interface NextPageArgs {
  at?: number;
}

/** The arguments of `previous_page`. */
export interface PreviousPageArgs {
  at?: number;
}

/** The arguments of `refresh_catalog`. */
export interface RefreshCatalogArgs {
}

/** The arguments of `order_by`. */
export interface OrderByArgs {
  column: string;
}

/** The arguments of `dismiss`. */
export interface DismissArgs {
}

/** The arguments of `check`. */
export interface CheckArgs {
}

/** The arguments of `opened`. */
export interface OpenedArgs {
  answer: string;
}

/** The arguments of `queried`. */
export interface QueriedArgs {
  answer: string;
}

/** The arguments of `forget_password`. */
export interface ForgetPasswordArgs {
  c: Connection;
}

/** The arguments of `clear_tables`. */
export interface ClearTablesArgs {
}

/** The arguments of `clear_structure`. */
export interface ClearStructureArgs {
}

/** The arguments of `clear_columns`. */
export interface ClearColumnsArgs {
}

/** The arguments of `clear_rows`. */
export interface ClearRowsArgs {
  result: boolean;
}

/** The arguments of `failed_job`. */
export interface FailedJobArgs {
  why: string;
}

/** Connection — `tl.so.tilo.db-client.Connection`. */
export interface Connection extends tilo.Resource {
  "52sa65q7": string;  // name
  cg25mito: Engine;  // engine
  lsni3mrq: string;  // host
  axk7b6ff: number;  // port
  b3gtxaxj: string;  // database
  "3zatdgh2": string;  // user
  zt2vj4bb: string;  // file_path
  "7wyfzimv": string | null;  // used
}

/** The actions `Connection` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONNECTION_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.Connection` kind at `path`. */
export function connection(ws: tilo.Workspace, path: string): Connection {
  return resourceProxy(ws, path, SURFACE, CONNECTION_ACTIONS) as unknown as Connection;
}

/** SavedQuery — `tl.so.tilo.db-client.SavedQuery`. */
export interface SavedQuery extends tilo.Resource {
  fxmhtvbz: string;  // name
  rnkrjcin: string;  // sql
}

/** The actions `SavedQuery` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVEDQUERY_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.SavedQuery` kind at `path`. */
export function savedQuery(ws: tilo.Workspace, path: string): SavedQuery {
  return resourceProxy(ws, path, SURFACE, SAVEDQUERY_ACTIONS) as unknown as SavedQuery;
}

/** Database — `tl.so.tilo.db-client.Database`. */
export interface Database extends tilo.Resource {
  djghd3ar: Array<tilo.Entry<Connection>>;  // connections
  dbkiebu3: Array<tilo.Entry<SavedQuery>>;  // queries
  r77o5x22: string;  // sql
  nvtukp2b: Array<tilo.Entry<Session>>;  // sessions
  /** This person's window, made by their own first action so it is theirs to read and write. */
  ensure_session(): Promise<Session>;
  /** Opens a saved connection: this device starts the engine on it and reads its catalog. */
  connect(args: {
    c: Connection;
  }): Promise<void>;
  /** Runs `connect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  connectMany(items: ConnectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Closes the open connection on this device. */
  disconnect(): Promise<void>;
  /** Runs `disconnect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  disconnectMany(items: DisconnectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a connection to the workspace, keeps its password on this device, and opens it. */
  add_connection(args: {
    name: string;
    engine_index: number;
    host: string;
    port: number;
    database: string;
    user: string;
    file_path: string;
    password: string;
  }): Promise<void>;
  /** Runs `add_connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_connectionMany(items: AddConnectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Removes a saved connection: its password goes with it on this device. */
  remove_connection(args: {
    c: Connection;
  }): Promise<void>;
  /** Runs `remove_connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_connectionMany(items: RemoveConnectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps `password` for `c` on this device, or forgets it when it is empty. */
  remember_password(args: {
    c: Connection;
    password: string;
  }): Promise<void>;
  /** Runs `remember_password` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remember_passwordMany(items: RememberPasswordArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Saves the statement in the editor under `name`. */
  save_query(args: {
    name: string;
  }): Promise<void>;
  /** Runs `save_query` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  save_queryMany(items: SaveQueryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Removes a saved query. */
  remove_query(args: {
    q: SavedQuery;
  }): Promise<void>;
  /** Runs `remove_query` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_queryMany(items: RemoveQueryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Loads a saved query into the editor, ready to run. */
  load_query(args: {
    q: SavedQuery;
  }): Promise<void>;
  /** Runs `load_query` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_queryMany(items: LoadQueryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a starter query in the editor, saved so the person can name it too. */
  load_starter(args: {
    name: string;
    sql: string;
  }): Promise<void>;
  /** Runs `load_starter` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_starterMany(items: LoadStarterArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the view the segment at `index` names. */
  choose_tab(args: {
    index: number;
  }): Promise<void>;
  /** Runs `choose_tab` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  choose_tabMany(items: ChooseTabArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one of the window's three views. */
  show_tab(args: {
    tab: Tab;
  }): Promise<void>;
  /** Runs `show_tab` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_tabMany(items: ShowTabArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a transaction on the open connection: what runs until the person commits is one change, and rolling it back takes all of it back. */
  begin_transaction(): Promise<void>;
  /** Runs `begin_transaction` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  begin_transactionMany(items: BeginTransactionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Commits the open transaction: what it changed is the database's from now on. */
  commit_transaction(): Promise<void>;
  /** Runs `commit_transaction` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  commit_transactionMany(items: CommitTransactionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Rolls the open transaction back: every change it made is taken back. */
  rollback_transaction(): Promise<void>;
  /** Runs `rollback_transaction` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rollback_transactionMany(items: RollbackTransactionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Runs the statement in the editor, asking first when it changes data. */
  run(): Promise<void>;
  /** Runs `run` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  runMany(items: RunArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Runs the statement the person confirmed. */
  confirm_run(): Promise<void>;
  /** Runs `confirm_run` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  confirm_runMany(items: ConfirmRunArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts away the statement that was waiting for confirmation. */
  dismiss_run(): Promise<void>;
  /** Runs `dismiss_run` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismiss_runMany(items: DismissRunArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Runs a saved query against the open connection. */
  run_query(args: {
    q: SavedQuery;
  }): Promise<void>;
  /** Runs `run_query` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  run_queryMany(items: RunQueryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Runs a statement this window ran before. */
  rerun(args: {
    r: Run;
  }): Promise<void>;
  /** Runs `rerun` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rerunMany(items: RerunArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts `sql` on the open connection as a job, so the window stays live and the statement can be stopped. */
  start_statement(args: {
    sql: string;
  }): Promise<void>;
  /** Runs `start_statement` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_statementMany(items: StartStatementArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the statement in flight. */
  cancel(): Promise<void>;
  /** Runs `cancel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancelMany(items: CancelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the table `t`: what it is made of, and its first page of rows. */
  browse(args: {
    t: TableInfo;
  }): Promise<void>;
  /** Runs `browse` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  browseMany(items: BrowseArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the selected table's keys and indexes; its columns came with the catalog. */
  load_structure(): Promise<void>;
  /** Runs `load_structure` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_structureMany(items: LoadStructureArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Loads one page of the selected table's rows. */
  load_page(): Promise<void>;
  /** Runs `load_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_pageMany(items: LoadPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the next page of the selected table's rows. */
  next_page(args?: {
    at?: number;
  }): Promise<void>;
  /** Runs `next_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  next_pageMany(items: NextPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the page before the one shown. */
  previous_page(args?: {
    at?: number;
  }): Promise<void>;
  /** Runs `previous_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  previous_pageMany(items: PreviousPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the open connection's catalog again, so a table made elsewhere shows. */
  refresh_catalog(): Promise<void>;
  /** Runs `refresh_catalog` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  refresh_catalogMany(items: RefreshCatalogArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Orders the result by the column the person chose, or back to the statement's own order. */
  order_by(args: {
    column: string;
  }): Promise<void>;
  /** Runs `order_by` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  order_byMany(items: OrderByArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies the result row at `at` as one line of text. */
  copy_row(args: {
    row: Row;
  }): Promise<void>;
  /** Copies the whole result as tab-separated text. */
  copy_result(): Promise<void>;
  /** The statement's result as a CSV file, for a person to keep. */
  export_csv(): Promise<unknown | null>;
  /** Clears what the window last said. */
  dismiss(): Promise<void>;
  /** Runs `dismiss` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismissMany(items: DismissArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks how far the job in flight has got, and folds its answer in. A job this device does not run says so, and leaves the window as it is. */
  check(): Promise<void>;
  /** Runs `check` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  checkMany(items: CheckArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a connection's handle in, once the engine opened it, and reads its catalog. */
  opened(args: {
    answer: string;
  }): Promise<void>;
  /** Runs `opened` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  openedMany(items: OpenedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a statement's result in, with the words the window shows of it. */
  queried(args: {
    answer: string;
  }): Promise<void>;
  /** Runs `queried` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  queriedMany(items: QueriedArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Database` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DATABASE_ACTIONS = ["ensure_session", "connect", "connectMany", "disconnect", "disconnectMany", "add_connection", "add_connectionMany", "remove_connection", "remove_connectionMany", "remember_password", "remember_passwordMany", "save_query", "save_queryMany", "remove_query", "remove_queryMany", "load_query", "load_queryMany", "load_starter", "load_starterMany", "choose_tab", "choose_tabMany", "show_tab", "show_tabMany", "begin_transaction", "begin_transactionMany", "commit_transaction", "commit_transactionMany", "rollback_transaction", "rollback_transactionMany", "run", "runMany", "confirm_run", "confirm_runMany", "dismiss_run", "dismiss_runMany", "run_query", "run_queryMany", "rerun", "rerunMany", "start_statement", "start_statementMany", "cancel", "cancelMany", "browse", "browseMany", "load_structure", "load_structureMany", "load_page", "load_pageMany", "next_page", "next_pageMany", "previous_page", "previous_pageMany", "refresh_catalog", "refresh_catalogMany", "order_by", "order_byMany", "copy_row", "copy_result", "export_csv", "dismiss", "dismissMany", "check", "checkMany", "opened", "openedMany", "queried", "queriedMany"] as const;

/** The `tl.so.tilo.db-client.Database` kind at `path`. */
export function database(ws: tilo.Workspace, path: string): Database {
  return resourceProxy(ws, path, SURFACE, DATABASE_ACTIONS) as unknown as Database;
}

/** TableInfo — `tl.so.tilo.db-client.TableInfo`. */
export interface TableInfo extends tilo.Resource {
  yofk7dul: string;  // schema
  h5zenygt: string;  // name
  tbo6xpjg: string;  // kind
  "7xr4zpm5": number | null;  // row_count
}

/** The actions `TableInfo` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TABLEINFO_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.TableInfo` kind at `path`. */
export function tableInfo(ws: tilo.Workspace, path: string): TableInfo {
  return resourceProxy(ws, path, SURFACE, TABLEINFO_ACTIONS) as unknown as TableInfo;
}

/** ColumnInfo — `tl.so.tilo.db-client.ColumnInfo`. */
export interface ColumnInfo extends tilo.Resource {
  kdq23jfd: string;  // schema
  wklm2rxq: string;  // table
  qmrcvjjn: string;  // name
  ffkug4qg: string;  // decl_type
  ifa6r5by: boolean;  // nullable
  sew352lt: boolean;  // primary_key
  "4ycs3rch": string | null;  // default_value
}

/** The actions `ColumnInfo` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COLUMNINFO_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.ColumnInfo` kind at `path`. */
export function columnInfo(ws: tilo.Workspace, path: string): ColumnInfo {
  return resourceProxy(ws, path, SURFACE, COLUMNINFO_ACTIONS) as unknown as ColumnInfo;
}

/** KeyInfo — `tl.so.tilo.db-client.KeyInfo`. */
export interface KeyInfo extends tilo.Resource {
  "4sqibo4s": string;  // column
  hmchr5lj: string;  // ref_table
  nse42ud4: string;  // ref_column
}

/** The actions `KeyInfo` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const KEYINFO_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.KeyInfo` kind at `path`. */
export function keyInfo(ws: tilo.Workspace, path: string): KeyInfo {
  return resourceProxy(ws, path, SURFACE, KEYINFO_ACTIONS) as unknown as KeyInfo;
}

/** IndexInfo — `tl.so.tilo.db-client.IndexInfo`. */
export interface IndexInfo extends tilo.Resource {
  bmcdtfyt: string;  // name
  co7n2x5l: boolean;  // unique
  ws3nxyhc: string;  // columns
}

/** The actions `IndexInfo` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const INDEXINFO_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.IndexInfo` kind at `path`. */
export function indexInfo(ws: tilo.Workspace, path: string): IndexInfo {
  return resourceProxy(ws, path, SURFACE, INDEXINFO_ACTIONS) as unknown as IndexInfo;
}

/** Answer — `tl.so.tilo.db-client.Answer`. */
export interface Answer extends tilo.Resource {
  n7guubug: string[];  // columns
  zmjmvjeu: string[][];  // rows
  ztnqa552: boolean[][];  // nulls
  gysqh6ps: boolean[];  // numeric
  h24jm6lc: number | null;  // rows_affected
  ydn3x76v: boolean;  // is_select
  "7vfwgci5": number;  // elapsed_ms
  qf7pdvx7: string;  // truncated
}

/** The actions `Answer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.Answer` kind at `path`. */
export function answer(ws: tilo.Workspace, path: string): Answer {
  return resourceProxy(ws, path, SURFACE, ANSWER_ACTIONS) as unknown as Answer;
}

/** Started — `tl.so.tilo.db-client.Started`. */
export interface Started extends tilo.Resource {
  ms2izpdh: string;  // job
}

/** The actions `Started` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STARTED_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.Started` kind at `path`. */
export function started(ws: tilo.Workspace, path: string): Started {
  return resourceProxy(ws, path, SURFACE, STARTED_ACTIONS) as unknown as Started;
}

/** Progress — `tl.so.tilo.db-client.Progress`. */
export interface Progress extends tilo.Resource {
  dday2ali: string;  // state
  bpgwkoxz: string;  // result
  gacluwxm: string;  // error
}

/** The actions `Progress` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROGRESS_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.Progress` kind at `path`. */
export function progress(ws: tilo.Workspace, path: string): Progress {
  return resourceProxy(ws, path, SURFACE, PROGRESS_ACTIONS) as unknown as Progress;
}

/** Opened — `tl.so.tilo.db-client.Opened`. */
export interface Opened extends tilo.Resource {
  yheverkf: number;  // connection
}

/** The actions `Opened` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OPENED_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.Opened` kind at `path`. */
export function opened(ws: tilo.Workspace, path: string): Opened {
  return resourceProxy(ws, path, SURFACE, OPENED_ACTIONS) as unknown as Opened;
}

/** Secret — `tl.so.tilo.db-client.Secret`. */
export interface Secret extends tilo.Resource {
  psin3vp7: string;  // connection
  "3twvmxwg": string;  // password
}

/** The actions `Secret` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SECRET_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.Secret` kind at `path`. */
export function secret(ws: tilo.Workspace, path: string): Secret {
  return resourceProxy(ws, path, SURFACE, SECRET_ACTIONS) as unknown as Secret;
}

/** Run — `tl.so.tilo.db-client.Run`. */
export interface Run extends tilo.Resource {
  "6abruthm": string;  // connection
  nxawaqfc: string;  // sql
  "3fza7fgn": string;  // summary
}

/** The actions `Run` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RUN_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.Run` kind at `path`. */
export function run(ws: tilo.Workspace, path: string): Run {
  return resourceProxy(ws, path, SURFACE, RUN_ACTIONS) as unknown as Run;
}

/** Row — `tl.so.tilo.db-client.Row`. */
export interface Row extends tilo.Resource {
  hxhdvye5: string[];  // cells
  gc6jygal: boolean[];  // nulls
}

/** The actions `Row` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ROW_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.Row` kind at `path`. */
export function row(ws: tilo.Workspace, path: string): Row {
  return resourceProxy(ws, path, SURFACE, ROW_ACTIONS) as unknown as Row;
}

/** Outcome — `tl.so.tilo.db-client.Outcome`. */
export interface Outcome extends tilo.Resource {
  qxwjsiht: number;  // seq
  rym2yft2: string[];  // columns
  m6wzfydf: boolean[];  // numeric
  "2k7ob6vx": number;  // affected
  kbnzmtdp: boolean;  // is_select
  "4ponyk7v": number;  // elapsed_ms
  iaudq64k: string;  // truncated
  qzyl3fzm: string;  // summary
  fywguxl3: string;  // order
  e5veu4zz: boolean;  // descending
}

/** The actions `Outcome` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OUTCOME_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.Outcome` kind at `path`. */
export function outcome(ws: tilo.Workspace, path: string): Outcome {
  return resourceProxy(ws, path, SURFACE, OUTCOME_ACTIONS) as unknown as Outcome;
}

/** Session — `tl.so.tilo.db-client.Session`. */
export interface Session extends tilo.Resource {
  elhp2adh: string;  // open
  "73sz5sht": string;  // name
  m2eenfte: boolean;  // connected
  l6ey6bmu: number;  // handle
  utbjkevx: Tab;  // tab
  "62jlzlao": string[];  // schemas
  c5axoen6: Array<tilo.Entry<TableInfo>>;  // tables
  solxuk72: Array<tilo.Entry<ColumnInfo>>;  // catalog_columns
  p5c7ywkh: Array<tilo.Entry<KeyInfo>>;  // keys
  zb4ttvcb: Array<tilo.Entry<IndexInfo>>;  // indexes
  y5w3id3d: string;  // selected_schema
  efbptxv4: string;  // selected_table
  "4e7a56ci": number;  // offset
  ikep24c2: number;  // page
  ymfu3vqr: string;  // status
  z4bjp5xk: string;  // notice
  p4wji46q: string;  // busy
  d2dlobjx: string;  // job_kind
  rxkpfssd: string;  // pending
  dvgcj2vb: boolean;  // confirming
  pmf3mqgy: Array<tilo.Entry<Outcome>>;  // queries
  d6drmo4f: Array<tilo.Entry<Outcome>>;  // pages
  qfvcjkgl: Array<tilo.Entry<Row>>;  // result_rows
  iimyuzxn: Array<tilo.Entry<Row>>;  // page_rows
  g6cz4ye2: Array<tilo.Entry<Run>>;  // history
  yxlfv3op: boolean;  // transaction
  ergnzsjf: string;  // started_sql
  qcislb6v: Array<tilo.Entry<Secret>>;  // secrets
  xrk5rswl: string;  // working
  /** Drops what this device kept for `c`. */
  forget_password(args: {
    c: Connection;
  }): Promise<void>;
  /** Runs `forget_password` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forget_passwordMany(items: ForgetPasswordArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Empties the tables a window holds, so a catalog read again never stands beside the one before. */
  clear_tables(): Promise<void>;
  /** Runs `clear_tables` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_tablesMany(items: ClearTablesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Empties the keys and indexes of the relation the window last chose. */
  clear_structure(): Promise<void>;
  /** Runs `clear_structure` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_structureMany(items: ClearStructureArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Empties the columns a window holds, so a catalog read again never stands beside the one before. */
  clear_columns(): Promise<void>;
  /** Runs `clear_columns` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_columnsMany(items: ClearColumnsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Empties a window's rows, so a fresh result never stands beside the one before it. */
  clear_rows(args: {
    result: boolean;
  }): Promise<void>;
  /** Runs `clear_rows` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_rowsMany(items: ClearRowsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The window's job could not be asked about at all: it is over, and the window says why. */
  failed_job(args: {
    why: string;
  }): Promise<void>;
  /** Runs `failed_job` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  failed_jobMany(items: FailedJobArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Session` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SESSION_ACTIONS = ["forget_password", "forget_passwordMany", "clear_tables", "clear_tablesMany", "clear_structure", "clear_structureMany", "clear_columns", "clear_columnsMany", "clear_rows", "clear_rowsMany", "failed_job", "failed_jobMany"] as const;

/** The `tl.so.tilo.db-client.Session` kind at `path`. */
export function session(ws: tilo.Workspace, path: string): Session {
  return resourceProxy(ws, path, SURFACE, SESSION_ACTIONS) as unknown as Session;
}

/** OpenAsk — `tl.so.tilo.db-client.OpenAsk`. */
export interface OpenAsk extends tilo.Resource {
  wr4rh4km: string;  // engine
  "7ub5mnfo": string;  // host
  rr35b5xd: number;  // port
  dmjf4ady: string;  // database
  g5wwnnue: string;  // user
  y5ginenz: string;  // password
  eak32p2q: string;  // file_path
}

/** The actions `OpenAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OPENASK_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.OpenAsk` kind at `path`. */
export function openAsk(ws: tilo.Workspace, path: string): OpenAsk {
  return resourceProxy(ws, path, SURFACE, OPENASK_ACTIONS) as unknown as OpenAsk;
}

/** QueryAsk — `tl.so.tilo.db-client.QueryAsk`. */
export interface QueryAsk extends tilo.Resource {
  "64plkkzv": number;  // connection
  eeqjqurk: string;  // sql
}

/** The actions `QueryAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const QUERYASK_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.QueryAsk` kind at `path`. */
export function queryAsk(ws: tilo.Workspace, path: string): QueryAsk {
  return resourceProxy(ws, path, SURFACE, QUERYASK_ACTIONS) as unknown as QueryAsk;
}

/** BrowseAsk — `tl.so.tilo.db-client.BrowseAsk`. */
export interface BrowseAsk extends tilo.Resource {
  ktmsx6gt: number;  // connection
  urg4wtul: string;  // schema
  "4j7eh7ld": string;  // table
  qexpbqva: number;  // limit
  ooei2svb: number;  // offset
}

/** The actions `BrowseAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BROWSEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.BrowseAsk` kind at `path`. */
export function browseAsk(ws: tilo.Workspace, path: string): BrowseAsk {
  return resourceProxy(ws, path, SURFACE, BROWSEASK_ACTIONS) as unknown as BrowseAsk;
}

/** TableAsk — `tl.so.tilo.db-client.TableAsk`. */
export interface TableAsk extends tilo.Resource {
  fmbqqhio: number;  // connection
  "2maemw4x": string;  // schema
  wwrvwb2w: string;  // table
}

/** The actions `TableAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TABLEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.TableAsk` kind at `path`. */
export function tableAsk(ws: tilo.Workspace, path: string): TableAsk {
  return resourceProxy(ws, path, SURFACE, TABLEASK_ACTIONS) as unknown as TableAsk;
}

/** Handle — `tl.so.tilo.db-client.Handle`. */
export interface Handle extends tilo.Resource {
  qjoudlls: number;  // connection
}

/** The actions `Handle` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HANDLE_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client.Handle` kind at `path`. */
export function handle(ws: tilo.Workspace, path: string): Handle {
  return resourceProxy(ws, path, SURFACE, HANDLE_ACTIONS) as unknown as Handle;
}

/** Database Client — `tl.so.tilo.db-client`. */
export interface DbClient extends tilo.Resource {
}

/** The actions `DbClient` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DBCLIENT_ACTIONS = [] as const;

/** The `tl.so.tilo.db-client` kind at `path`. */
export function dbClient(ws: tilo.Workspace, path: string): DbClient {
  return resourceProxy(ws, path, SURFACE, DBCLIENT_ACTIONS) as unknown as DbClient;
}

