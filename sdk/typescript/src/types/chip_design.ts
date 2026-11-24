/**
 * Types for Chip Design (`so.tilo.chip-design`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Port` is a `Resource` of the app's `tl.so.tilo.chip-design.Port`, made by its factory with the
 * resource's path (`chip-design/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.chip-design@49f91fca41a22962";

/** The structural operator kinds a design is built from. One of "input", "gate_input", "norm_weight_input", "rms_norm", "apply_norm_weight", "silu", "multiply", "projection_input", "matrix_weight_input", "linear_projection", "add". */
export type OperatorKind = "input" | "gate_input" | "norm_weight_input" | "rms_norm" | "apply_norm_weight" | "silu" | "multiply" | "projection_input" | "matrix_weight_input" | "linear_projection" | "add";

/** Whether a storage report bounds its attention cache, or cannot say. One of "bound", "unsupported". */
export type CacheKind = "bound" | "unsupported";

/** How a reported check ended. One of "passed", "failed". */
export type CheckResult = "passed" | "failed";

/** What a numerical comparison asserts. One of "exact", "characterization". */
export type Comparison = "exact" | "characterization";

/** What a reported evidence stage observed. One of "none", "cpu_reference", "rtl_simulation", "generic_synthesis", "library_physical", "rms_norm", "silu", "gated_delta", "softmax". */
export type ObservationKind = "none" | "cpu_reference" | "rtl_simulation" | "generic_synthesis" | "library_physical" | "rms_norm" | "silu" | "gated_delta" | "softmax";

/** How a reported RTL simulation ended. One of "completed", "expected_refusal". */
export type Outcome = "completed" | "expected_refusal";

/** The reported macro placements' orientation. One of "r0", "r90", "r180", "r270", "mx", "my", "mxr90", "myr90". */
export type Orientation = "r0" | "r90" | "r180" | "r270" | "mx" | "my" | "mxr90" | "myr90";

/** How a reported run ended. One of "passed", "failed", "cancelled". */
export type RunResult = "passed" | "failed" | "cancelled";

/** What a reported evidence dependency carries. One of "data", "build", "verification". */
export type DependencyKind = "data" | "build" | "verification";

/** Which stage of an architecture comparison a receipt reports. One of "prepare", "numerical", "fault", "synthesis", "aggregate". */
export type StageKind = "prepare" | "numerical" | "fault" | "synthesis" | "aggregate";

/** The arguments of `import_file`. */
export interface ImportFileArgs {
  file: unknown;
}

/** The arguments of `import_report`. */
export interface ImportReportArgs {
  document: string;
}

/** The arguments of `import_verification`. */
export interface ImportVerificationArgs {
  document: string;
}

/** The arguments of `take_in`. */
export interface TakeInArgs {
  document: string;
}

/** The arguments of `keep_report`. */
export interface KeepReportArgs {
  digest: string;
  source: string;
  facts: unknown;
}

/** The arguments of `keep_receipt`. */
export interface KeepReceiptArgs {
  digest: string;
  source: string;
  facts: unknown;
}

/** The arguments of `remove_report`. */
export interface RemoveReportArgs {
  r: Report;
}

/** The arguments of `remove_verification`. */
export interface RemoveVerificationArgs {
  r: Receipt;
}

/** The arguments of `show_graph`. */
export interface ShowGraphArgs {
}

/** The arguments of `show_all`. */
export interface ShowAllArgs {
}

/** The arguments of `inspect_report`. */
export interface InspectReportArgs {
  r: Report;
}

/** The arguments of `inspect_receipt`. */
export interface InspectReceiptArgs {
  r: Receipt;
}

/** The arguments of `inspect_stage`. */
export interface InspectStageArgs {
  r: Receipt;
  s: Stage;
}

/** The arguments of `inspect_macro`. */
export interface InspectMacroArgs {
  r: Receipt;
  m: Macro;
}

/** The arguments of `open_subflow`. */
export interface OpenSubflowArgs {
  r: Receipt;
  s: Subflow;
}

/** The arguments of `dismiss`. */
export interface DismissArgs {
}

/** The arguments of `zoom_graph`. */
export interface ZoomGraphArgs {
  step: string;
}

/** The arguments of `select_output`. */
export interface SelectOutputArgs {
  op: Operator;
  port: string;
}

/** The arguments of `clear_output`. */
export interface ClearOutputArgs {
}

/** The arguments of `inspect_operator`. */
export interface InspectOperatorArgs {
  op: Operator;
}

/** The arguments of `close_operator`. */
export interface CloseOperatorArgs {
}

/** The arguments of `add_operator`. */
export interface AddOperatorArgs {
  name: string;
}

/** The arguments of `add_operator_at`. */
export interface AddOperatorAtArgs {
  node: string;
  kind: OperatorKind;
  shape: number[];
  output_width: number;
  x: number;
  y: number;
}

/** The arguments of `set_shape`. */
export interface SetShapeArgs {
  op: Operator;
  shape: number[];
}

/** The arguments of `apply_shape`. */
export interface ApplyShapeArgs {
  op: Operator;
}

/** The arguments of `apply_width`. */
export interface ApplyWidthArgs {
  op: Operator;
}

/** The arguments of `tap_macro`. */
export interface TapMacroArgs {
  s: Stage;
  index: number;
}

/** The arguments of `set_output_width`. */
export interface SetOutputWidthArgs {
  op: Operator;
  width: number;
}

/** The arguments of `move_operator`. */
export interface MoveOperatorArgs {
  op: Operator;
  x: number;
  y: number;
}

/** The arguments of `remove_operator`. */
export interface RemoveOperatorArgs {
  op: Operator;
}

/** The arguments of `connect`. */
export interface ConnectArgs {
  from: Operator;
  from_port: string;
  to: Operator;
  to_port: string;
}

/** The arguments of `disconnect`. */
export interface DisconnectArgs {
  op: Operator;
  port: string;
}

/** The arguments of `resolve_graph`. */
export interface ResolveGraphArgs {
}

/** The arguments of `apply_graph`. */
export interface ApplyGraphArgs {
  answer: string;
}

/** The arguments of `inspect_seen_macro`. */
export interface InspectSeenMacroArgs {
  s: Stage;
  m: Macro;
  r?: Receipt;
}

/** The arguments of `tap_graph`. */
export interface TapGraphArgs {
  index: number;
}

/** Port — `tl.so.tilo.chip-design.Port`. */
export interface Port extends tilo.Resource {
  esvx2n72: string;  // key
  vjm4mdax: string;  // data_type
}

/** The actions `Port` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PORT_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Port` kind at `path`. */
export function port(ws: tilo.Workspace, path: string): Port {
  return resourceProxy(ws, path, SURFACE, PORT_ACTIONS) as unknown as Port;
}

/** Operator — `tl.so.tilo.chip-design.Operator`. */
export interface Operator extends tilo.Resource {
  wxkbwcn5: string;  // node
  e6aj4t7l: OperatorKind;  // kind
  nbs52ikt: number[];  // shape
  w6rehfmk: number;  // normalization_width
  gonrdrls: number;  // output_width
  u3ws6vjg: boolean;  // has_output_width
  nreyvv4r: string;  // shape_text
  sjbeclxu: string;  // width_text
  s4l6n47t: number;  // x
  pgjl7vwe: number;  // y
  neb6c6b5: Array<tilo.Entry<Port>>;  // inputs
  eb6j7t6x: Array<tilo.Entry<Port>>;  // outputs
}

/** The actions `Operator` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OPERATOR_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Operator` kind at `path`. */
export function operator(ws: tilo.Workspace, path: string): Operator {
  return resourceProxy(ws, path, SURFACE, OPERATOR_ACTIONS) as unknown as Operator;
}

/** Connection — `tl.so.tilo.chip-design.Connection`. */
export interface Connection extends tilo.Resource {
  sqqjxtyw: string;  // from_node
  hk3nk2x5: string;  // from_port
  xgc3rbc2: string;  // to_node
  hxqnvwsp: string;  // to_port
  "3tc244az": string;  // digest
  o2gagtnb: boolean;  // active
  qkwwqrxp: string;  // reason
}

/** The actions `Connection` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONNECTION_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Connection` kind at `path`. */
export function connection(ws: tilo.Workspace, path: string): Connection {
  return resourceProxy(ws, path, SURFACE, CONNECTION_ACTIONS) as unknown as Connection;
}

/** Assumptions — `tl.so.tilo.chip-design.Assumptions`. */
export interface Assumptions extends tilo.Resource {
  run6wd4l: number;  // logical_parameters
  smx2rwim: number;  // weight_bits
  kclrpitf: number;  // kv_bits
  "6r63anpq": number;  // context_tokens
}

/** The actions `Assumptions` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ASSUMPTIONS_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Assumptions` kind at `path`. */
export function assumptions(ws: tilo.Workspace, path: string): Assumptions {
  return resourceProxy(ws, path, SURFACE, ASSUMPTIONS_ACTIONS) as unknown as Assumptions;
}

/** Report — `tl.so.tilo.chip-design.Report`. */
export interface Report extends tilo.Resource {
  twijrbwn: string;  // digest
  "7rep5j6b": string;  // source
  hd2g3vdr: string;  // model
  fxx5zcjz: number;  // schema_version
  svry2b7t: string;  // revision
  xyjuenxt: string;  // config_sha256
  vyv4me7w: string;  // evidence
  b3g6lqvm: number;  // batch_size
  "5qj7upc2": Assumptions;  // assumptions
  "42kdgwvs": number;  // packed_weight_bytes
  "7jvkir3f": CacheKind;  // cache
  qw2v536a: string;  // cache_reason
  ogp5wc76: number;  // attention_layers
  he3itm4b: number;  // kv_elements
  biifjz5l: number;  // kv_bytes
  n3tcruqh: string[];  // cache_excludes
  mrp773gq: string[];  // not_estimated
  oypgaywq: string[];  // limitations
}

/** The actions `Report` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPORT_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Report` kind at `path`. */
export function report(ws: tilo.Workspace, path: string): Report {
  return resourceProxy(ws, path, SURFACE, REPORT_ACTIONS) as unknown as Report;
}

/** Check — `tl.so.tilo.chip-design.Check`. */
export interface Check extends tilo.Resource {
  i2clfcou: string;  // name
  arunp5tg: CheckResult;  // result
}

/** The actions `Check` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHECK_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Check` kind at `path`. */
export function check(ws: tilo.Workspace, path: string): Check {
  return resourceProxy(ws, path, SURFACE, CHECK_ACTIONS) as unknown as Check;
}

/** Tool — `tl.so.tilo.chip-design.Tool`. */
export interface Tool extends tilo.Resource {
  "6l36tmym": string;  // name
  ptuif53v: string;  // version
  "6ffjwxk2": string;  // executable_sha256
}

/** The actions `Tool` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TOOL_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Tool` kind at `path`. */
export function tool(ws: tilo.Workspace, path: string): Tool {
  return resourceProxy(ws, path, SURFACE, TOOL_ACTIONS) as unknown as Tool;
}

/** Artifact — `tl.so.tilo.chip-design.Artifact`. */
export interface Artifact extends tilo.Resource {
  "4d7avciq": string;  // name
  nwa3vepp: string;  // sha256
}

/** The actions `Artifact` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ARTIFACT_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Artifact` kind at `path`. */
export function artifact(ws: tilo.Workspace, path: string): Artifact {
  return resourceProxy(ws, path, SURFACE, ARTIFACT_ACTIONS) as unknown as Artifact;
}

/** Digest — `tl.so.tilo.chip-design.Digest`. */
export interface Digest extends tilo.Resource {
  r4lv3eu7: string;  // name
  pn5tgrsn: string;  // sha256
}

/** The actions `Digest` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DIGEST_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Digest` kind at `path`. */
export function digest(ws: tilo.Workspace, path: string): Digest {
  return resourceProxy(ws, path, SURFACE, DIGEST_ACTIONS) as unknown as Digest;
}

/** CellCount — `tl.so.tilo.chip-design.CellCount`. */
export interface CellCount extends tilo.Resource {
  r4l4iqx2: string;  // name
  vxrpbdeb: number;  // count
}

/** The actions `CellCount` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CELLCOUNT_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.CellCount` kind at `path`. */
export function cellCount(ws: tilo.Workspace, path: string): CellCount {
  return resourceProxy(ws, path, SURFACE, CELLCOUNT_ACTIONS) as unknown as CellCount;
}

/** Observation — `tl.so.tilo.chip-design.Observation`. */
export interface Observation extends tilo.Resource {
  l3my4c7u: string;  // label
  boatafil: ObservationKind;  // kind
  jbgmt4ie: Comparison;  // comparison
  "2m7xr7oi": number;  // compared_words
  "765n5dvn": number;  // differing_words
  r2sgywxz: string;  // output_sha256
  "2mvyzxqy": Outcome;  // outcome
  jqiqarxm: number;  // refused_inputs
  "7g5ljuf2": number;  // cycles
  tkyyux4t: number;  // accepted_inputs
  byvm4brz: number;  // accepted_outputs
  ysqpkcxy: number;  // logic_cells
  fmeekkru: number;  // logical_memory_bits
  fswivmpb: string;  // library
  e5eeqpa7: string;  // clock_constraint_ns
  orrpic3k: string;  // setup_slack_ns
  pbdqj47x: string;  // hold_slack_ns
  j7qjxpgt: number;  // routed_signal_nets
  js7iwiwx: number;  // router_violations
  bsogreds: string;  // library_area_um2
  "62qim76q": string;  // profile
  vdf52bkl: number;  // fp32_comparisons
  qvzkneov: number;  // total_inputs
  sofq7z7n: number;  // exp_differences
  rgsgq4i3: number;  // max_exp_ulp
  fwsjrjk3: number;  // heads
  shn33gs4: number;  // tokens
  fn6veddp: number;  // width
  d3y6qjsu: number;  // attention_words
  rfak7s5n: number;  // state_words
  "3mbqqhmz": string;  // name
  hif4kqoj: number;  // masked
  "3ijpmjgv": number;  // max_ulp
}

/** The actions `Observation` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OBSERVATION_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Observation` kind at `path`. */
export function observation(ws: tilo.Workspace, path: string): Observation {
  return resourceProxy(ws, path, SURFACE, OBSERVATION_ACTIONS) as unknown as Observation;
}

/** Memory — `tl.so.tilo.chip-design.Memory`. */
export interface Memory extends tilo.Resource {
  imhcxbpl: string;  // name
  eifw7p4n: number;  // width_bits
  "474at3uz": number;  // depth_words
  o76j7m2f: number;  // bits
  "2aqawzb3": number;  // read_ports
  he5exqe2: number;  // clocked_read_ports
  "6nk642cp": number;  // write_ports
  hovn72p4: boolean;  // fully_initialized
}

/** The actions `Memory` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MEMORY_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Memory` kind at `path`. */
export function memory(ws: tilo.Workspace, path: string): Memory {
  return resourceProxy(ws, path, SURFACE, MEMORY_ACTIONS) as unknown as Memory;
}

/** Variant — `tl.so.tilo.chip-design.Variant`. */
export interface Variant extends tilo.Resource {
  zl5mvubo: string;  // identity
  "4y2sweqe": boolean;  // numerical
  qe2ijn52: number;  // cold_program_cycles
  lvk3uaeb: number;  // vector_load_cycles
  pm72baqp: number;  // warm_cycles
  bctwv4iq: number;  // accepted_weight_bytes
  sbhcuyji: number;  // accepted_descriptor_bytes
  lrpqhjtv: number;  // cache_reads
  chkz2y4j: number;  // quantizer_packets
  f7om2r4s: number;  // comparison_words
  "5p4i7krn": string;  // output_sha256
  vte7iosv: boolean;  // synthesis
  z3aiymm6: number;  // generic_logic_cells
  wsvyc46d: number;  // activation_cache_bits
  ouotgooh: number;  // programmed_weight_bits
  htrm4etw: number;  // fixed_initialized_bits
  s3urh6zt: Array<tilo.Entry<Memory>>;  // memories
}

/** The actions `Variant` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VARIANT_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Variant` kind at `path`. */
export function variant(ws: tilo.Workspace, path: string): Variant {
  return resourceProxy(ws, path, SURFACE, VARIANT_ACTIONS) as unknown as Variant;
}

/** StageLink — `tl.so.tilo.chip-design.StageLink`. */
export interface StageLink extends tilo.Resource {
  "2bpp44k7": string;  // name
  cphzthhu: string;  // receipt_sha256
  vnv3rjs3: string;  // producer_sha256
  u5cz3l65: string;  // input_identity_sha256
}

/** The actions `StageLink` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STAGELINK_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.StageLink` kind at `path`. */
export function stageLink(ws: tilo.Workspace, path: string): StageLink {
  return resourceProxy(ws, path, SURFACE, STAGELINK_ACTIONS) as unknown as StageLink;
}

/** Stage — `tl.so.tilo.chip-design.Stage`. */
export interface Stage extends tilo.Resource {
  "4qfzbkq5": string;  // key
  htbbatar: string;  // title
  wyge75zm: RunResult;  // status
  uglh3san: string;  // original_receipt_artifact
  "46xvj5o3": string;  // producer_sha256
  "775i2e3d": string;  // input_identity_sha256
  vxt23uic: string[];  // limitations
  zistzyfl: ObservationKind;  // observed
  "4nxr5eoq": string;  // observation
  vqhz7zzv: Array<tilo.Entry<Check>>;  // checks
  znevuybg: boolean;  // placed
  "6mwfx57p": string;  // database_artifact
  t4u6picr: string;  // observation_artifact
  jquleaxm: number;  // dbu_per_micron
  abpu3jku: number;  // die_left
  zgxyfzp2: number;  // die_bottom
  cfo4rbhf: number;  // die_right
  ozebffaf: number;  // die_top
  lk5ymnxh: number;  // total_instances
  linro3fe: number;  // omitted_instances
  l7433uoi: Array<tilo.Entry<Macro>>;  // macros
}

/** The actions `Stage` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STAGE_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Stage` kind at `path`. */
export function stage(ws: tilo.Workspace, path: string): Stage {
  return resourceProxy(ws, path, SURFACE, STAGE_ACTIONS) as unknown as Stage;
}

/** Macro — `tl.so.tilo.chip-design.Macro`. */
export interface Macro extends tilo.Resource {
  l2pfdak5: string;  // slot
  shgem24v: string;  // name
  oo4l5j2f: string;  // master
  tpbo6p6i: Orientation;  // orientation
  mvnygfkc: string;  // placement
  sb4fvyic: number;  // left
  uwbrid6d: number;  // bottom
  bdrtkvnd: number;  // right
  "2f6kftyj": number;  // top
}

/** The actions `Macro` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MACRO_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Macro` kind at `path`. */
export function macro(ws: tilo.Workspace, path: string): Macro {
  return resourceProxy(ws, path, SURFACE, MACRO_ACTIONS) as unknown as Macro;
}

/** Dependency — `tl.so.tilo.chip-design.Dependency`. */
export interface Dependency extends tilo.Resource {
  lifw33iv: string;  // from
  lau4lbgx: string;  // to
  vuoz6p3k: DependencyKind;  // kind
  flo3szj2: string;  // artifact
}

/** The actions `Dependency` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DEPENDENCY_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Dependency` kind at `path`. */
export function dependency(ws: tilo.Workspace, path: string): Dependency {
  return resourceProxy(ws, path, SURFACE, DEPENDENCY_ACTIONS) as unknown as Dependency;
}

/** Subflow — `tl.so.tilo.chip-design.Subflow`. */
export interface Subflow extends tilo.Resource {
  utp25rwu: string;  // key
  "2hn4pjda": string;  // title
  qhfawhwq: string;  // summary_artifact
}

/** The actions `Subflow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SUBFLOW_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Subflow` kind at `path`. */
export function subflow(ws: tilo.Workspace, path: string): Subflow {
  return resourceProxy(ws, path, SURFACE, SUBFLOW_ACTIONS) as unknown as Subflow;
}

/** Receipt — `tl.so.tilo.chip-design.Receipt`. */
export interface Receipt extends tilo.Resource {
  mhsaondd: string;  // digest
  nw7f6brj: string;  // source
  x37f53ms: string;  // evidence
  nj3b4qpo: RunResult;  // status
  jhyibxbz: string;  // imported_bytes_sha256
  "3zyb62tt": string;  // failure
  zon6lior: string;  // failure_stage
  bp2hd5ou: CheckResult;  // input_validation
  sp7jdjll: boolean;  // has_input_validation
  gcb6foq7: number;  // lanes
  ufj5tgp7: boolean;  // has_lanes
  ntsw3ejx: number;  // group_size
  t5yt4cgs: boolean;  // has_group_size
  ghx5dlez: string;  // declared_toolchain_image
  sofs7yem: string;  // sample_manifest_sha256
  tu4t57su: string;  // verifier_sha256
  qvghxafg: string;  // artifact_validator_sha256
  snhxqhnq: string;  // mapper_sha256
  "7ifmtq3n": string;  // library_revision
  pyl5qiwx: number;  // mapped_cell_count
  psq5vxbm: boolean;  // has_mapped_cell_count
  "325si2ag": string;  // reported_library_area
  "4ldwfggs": number;  // abc_delay_target_ps
  sdqy2njd: boolean;  // has_abc_target
  jxgfvcs7: boolean;  // manufacturable
  d6eyywsn: boolean;  // has_manufacturable
  hu5xloo5: boolean;  // synthetic_coefficients
  rmrnw3gy: boolean;  // has_synthetic_coefficients
  gvdrc4h2: boolean;  // synthetic_activations
  "3uovqvrr": boolean;  // has_synthetic_activations
  "6fqc7ll4": string;  // sample_sha256
  hsospyt4: string;  // sample_receipt_sha256
  zenayb65: string;  // rtl_source_sha256
  mbf2wgz7: string;  // real_protocol
  "4ddzxjoe": string;  // c_instrumentation
  cbi2ze7u: number;  // external_scalar_comparisons
  "277q5ghg": boolean;  // has_external_scalar_comparisons
  bdd52yao: number;  // generic_cell_count
  bsobd2xf: boolean;  // has_generic_cell_count
  "7bk6ofeo": number;  // excluded_metadata_cells
  "3sohcddq": boolean;  // has_excluded_metadata_cells
  "5rnyzfkw": string[];  // compiler_flags
  u44pzow2: Array<tilo.Entry<Check>>;  // checks
  zg5xxom6: Array<tilo.Entry<Tool>>;  // tools
  zxqh5fqc: Array<tilo.Entry<Artifact>>;  // artifacts
  "4nv3clxq": string[];  // excludes
  upp3twxb: Array<tilo.Entry<Digest>>;  // input_digests
  eryiyu4m: Array<tilo.Entry<Digest>>;  // primary_sources
  ljs2gw3t: Array<tilo.Entry<Digest>>;  // sources
  ev5xwwya: Array<tilo.Entry<CellCount>>;  // generic_cells
  yd5juegt: boolean;  // operator
  ytpaenff: string;  // op_original_receipt_sha256
  "67ramk2w": string;  // op_primary_revision
  l2hy4vng: string;  // op_compiler_version
  "7mcamyri": string;  // op_compiler_executable_sha256
  gbjecqi4: string[];  // op_compiler_flags
  s52ihrs2: Array<tilo.Entry<Digest>>;  // op_primary_sources
  el3wnlvr: Array<tilo.Entry<Digest>>;  // op_local_sources
  vayzuyd3: Array<tilo.Entry<Digest>>;  // op_libraries
  ph5z4g57: Array<tilo.Entry<Observation>>;  // observations
  "3tunzd7f": boolean;  // study
  yu7n73kw: StageKind;  // study_stage
  o34vwu7q: string;  // study_original_receipt_sha256
  czwcogej: string;  // study_producer_sha256
  iehv2mu5: string;  // study_input_identity_sha256
  idk7oh4r: number;  // study_expected_comparison_words
  tietricm: Array<tilo.Entry<StageLink>>;  // links
  p4nm3blp: Array<tilo.Entry<Variant>>;  // variants
  "4zxln2bs": boolean;  // flow
  x4ibyx74: string;  // flow_title
  eti75aww: string;  // flow_input_identity_sha256
  dj67bcy5: Array<tilo.Entry<Stage>>;  // stages
  "6cz7yifm": Array<tilo.Entry<Dependency>>;  // dependencies
  ol3j2h7j: Array<tilo.Entry<Subflow>>;  // subflows
}

/** The actions `Receipt` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECEIPT_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.Receipt` kind at `path`. */
export function receipt(ws: tilo.Workspace, path: string): Receipt {
  return resourceProxy(ws, path, SURFACE, RECEIPT_ACTIONS) as unknown as Receipt;
}

/** Project — `tl.so.tilo.chip-design.Project`. */
export interface Project extends tilo.Resource {
  "5vvat77h": Array<tilo.Entry<Report>>;  // reports
  lst2bswm: Array<tilo.Entry<Receipt>>;  // verifications
  qajzmqsz: Array<tilo.Entry<Operator>>;  // operators
  ij6pw4z7: Array<tilo.Entry<Connection>>;  // connections
  lmh6n2cu: string;  // selected
  iwadzlgx: string;  // subject
  u7dhnpij: string;  // node
  mppe3c5h: string;  // output_node
  ocvjm3oh: string;  // output_port
  vrtmdnei: number;  // zoom
  "3oydn2gz": string;  // stage
  uuloznl2: string;  // macro
  yl422q4s: string;  // failure
  "2cb665aj": string;  // notice
  /** Imports the engineering evidence a JSON file holds: an analytical storage report or a hardware verification receipt. This is what a person's files and the file chooser hand the app, and importing the same bytes twice keeps one record. */
  import_file(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `import_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_fileMany(items: ImportFileArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Imports an analytical storage report from its JSON. */
  import_report(args: {
    document: string;
  }): Promise<void>;
  /** Runs `import_report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_reportMany(items: ImportReportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Imports a hardware verification receipt from its JSON. */
  import_verification(args: {
    document: string;
  }): Promise<void>;
  /** Runs `import_verification` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_verificationMany(items: ImportVerificationArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes in one imported document, whichever it is: the device checks it first, and what it holds reaches the project only once its own contract accepts it. */
  take_in(args: {
    document: string;
  }): Promise<void>;
  /** Runs `take_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  take_inMany(items: TakeInArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps one analytical storage report and the assumptions and scope it states. */
  keep_report(args: {
    digest: string;
    source: string;
    facts: unknown;
  }): Promise<void>;
  /** Runs `keep_report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_reportMany(items: KeepReportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps one hardware verification receipt: its scope, its checks, the identities it reports, and whichever of the three reported protocols it speaks. */
  keep_receipt(args: {
    digest: string;
    source: string;
    facts: unknown;
  }): Promise<void>;
  /** Runs `keep_receipt` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_receiptMany(items: KeepReceiptArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Removes a report and everything it stated. */
  remove_report(args: {
    r: Report;
  }): Promise<void>;
  /** Runs `remove_report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_reportMany(items: RemoveReportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Removes a verification receipt and everything it stated. */
  remove_verification(args: {
    r: Receipt;
  }): Promise<void>;
  /** Runs `remove_verification` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_verificationMany(items: RemoveVerificationArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the operator graph. */
  show_graph(): Promise<void>;
  /** Runs `show_graph` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_graphMany(items: ShowGraphArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Lists every report and receipt the project holds. */
  show_all(): Promise<void>;
  /** Runs `show_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_allMany(items: ShowAllArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one report. */
  inspect_report(args: {
    r: Report;
  }): Promise<void>;
  /** Runs `inspect_report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  inspect_reportMany(items: InspectReportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one verification receipt. */
  inspect_receipt(args: {
    r: Receipt;
  }): Promise<void>;
  /** Runs `inspect_receipt` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  inspect_receiptMany(items: InspectReceiptArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one stage of the receipt the window holds open. */
  inspect_stage(args: {
    r: Receipt;
    s: Stage;
  }): Promise<void>;
  /** Runs `inspect_stage` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  inspect_stageMany(items: InspectStageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one macro of the placement the window holds open. */
  inspect_macro(args: {
    r: Receipt;
    m: Macro;
  }): Promise<void>;
  /** Runs `inspect_macro` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  inspect_macroMany(items: InspectMacroArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the receipt an evidence flow's subflow reference names, when its summary is imported. */
  open_subflow(args: {
    r: Receipt;
    s: Subflow;
  }): Promise<void>;
  /** Runs `open_subflow` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_subflowMany(items: OpenSubflowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shrugs off the line about the last refusal or success. */
  dismiss(): Promise<void>;
  /** Runs `dismiss` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismissMany(items: DismissArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Zooms the operator graph in, out, or back to its whole extent. */
  zoom_graph(args: {
    step: string;
  }): Promise<void>;
  /** Runs `zoom_graph` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  zoom_graphMany(items: ZoomGraphArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Chooses the output a person connects from, naming the inputs it can feed. */
  select_output(args: {
    op: Operator;
    port: string;
  }): Promise<void>;
  /** Runs `select_output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  select_outputMany(items: SelectOutputArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Leaves the graph without a chosen output. */
  clear_output(): Promise<void>;
  /** Runs `clear_output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_outputMany(items: ClearOutputArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one operator's own facts, or closes them. */
  inspect_operator(args: {
    op: Operator;
  }): Promise<void>;
  /** Runs `inspect_operator` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  inspect_operatorMany(items: InspectOperatorArgs[]): Promise<tilo.ActionAnswer[]>;
  close_operator(): Promise<void>;
  /** Runs `close_operator` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  close_operatorMany(items: CloseOperatorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds an operator of `kind` after the ones already in the graph, on a four-wide grid. */
  add_operator(args: {
    name: string;
  }): Promise<void>;
  /** Runs `add_operator` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_operatorMany(items: AddOperatorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds one operator with exactly the shape, matrix width and place a caller gives it. The device checks the whole design before any of it is kept. */
  add_operator_at(args: {
    node: string;
    kind: OperatorKind;
    shape: number[];
    output_width: number;
    x: number;
    y: number;
  }): Promise<void>;
  /** Runs `add_operator_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_operator_atMany(items: AddOperatorAtArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reshapes one operator. Its last axis sets the normalization width, and every connection the new shape cannot hold stops holding. */
  set_shape(args: {
    op: Operator;
    shape: number[];
  }): Promise<void>;
  /** Runs `set_shape` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_shapeMany(items: SetShapeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Applies the shape a person typed for one operator, when it names one. */
  apply_shape(args: {
    op: Operator;
  }): Promise<void>;
  /** Runs `apply_shape` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  apply_shapeMany(items: ApplyShapeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Applies the matrix output width a person typed for one operator. */
  apply_width(args: {
    op: Operator;
  }): Promise<void>;
  /** Runs `apply_width` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  apply_widthMany(items: ApplyWidthArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one placement macro of the stage the window holds open. */
  tap_macro(args: {
    s: Stage;
    index: number;
  }): Promise<void>;
  /** Runs `tap_macro` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  tap_macroMany(items: TapMacroArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the matrix output width of one operator. */
  set_output_width(args: {
    op: Operator;
    width: number;
  }): Promise<void>;
  /** Runs `set_output_width` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_output_widthMany(items: SetOutputWidthArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves one operator to a place on the canvas. */
  move_operator(args: {
    op: Operator;
    x: number;
    y: number;
  }): Promise<void>;
  /** Runs `move_operator` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_operatorMany(items: MoveOperatorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Removes one operator, and every connection it held. Every other connection holds as it did: removing an operator takes only its own edges, and no other operator's ports change. */
  remove_operator(args: {
    op: Operator;
  }): Promise<void>;
  /** Runs `remove_operator` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_operatorMany(items: RemoveOperatorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Connects one output to one input. The device refuses a connection whose types differ, whose input another connection holds, or that would close a cycle. */
  connect(args: {
    from: Operator;
    from_port: string;
    to: Operator;
    to_port: string;
  }): Promise<void>;
  /** Runs `connect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  connectMany(items: ConnectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Disconnects every connection into one input. */
  disconnect(args: {
    op: Operator;
    port: string;
  }): Promise<void>;
  /** Runs `disconnect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  disconnectMany(items: DisconnectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Resolves the whole design again, so what the window shows is what the stored design makes. */
  resolve_graph(): Promise<void>;
  /** Runs `resolve_graph` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resolve_graphMany(items: ResolveGraphArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps the ports and connection outcomes the device answered, so the window draws and refuses by the design's own resolution. */
  apply_graph(args: {
    answer: string;
  }): Promise<void>;
  /** Runs `apply_graph` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  apply_graphMany(items: ApplyGraphArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one macro the person picked from the placement's own list. */
  inspect_seen_macro(args: {
    s: Stage;
    m: Macro;
    r?: Receipt;
  }): Promise<void>;
  /** Runs `inspect_seen_macro` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  inspect_seen_macroMany(items: InspectSeenMacroArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A press on the canvas: an operator's title opens its facts, an output is what the next input connects from, and an input connects or opens its operator. */
  tap_graph(args: {
    index: number;
  }): Promise<void>;
  /** Runs `tap_graph` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  tap_graphMany(items: TapGraphArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Project` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROJECT_ACTIONS = ["import_file", "import_fileMany", "import_report", "import_reportMany", "import_verification", "import_verificationMany", "take_in", "take_inMany", "keep_report", "keep_reportMany", "keep_receipt", "keep_receiptMany", "remove_report", "remove_reportMany", "remove_verification", "remove_verificationMany", "show_graph", "show_graphMany", "show_all", "show_allMany", "inspect_report", "inspect_reportMany", "inspect_receipt", "inspect_receiptMany", "inspect_stage", "inspect_stageMany", "inspect_macro", "inspect_macroMany", "open_subflow", "open_subflowMany", "dismiss", "dismissMany", "zoom_graph", "zoom_graphMany", "select_output", "select_outputMany", "clear_output", "clear_outputMany", "inspect_operator", "inspect_operatorMany", "close_operator", "close_operatorMany", "add_operator", "add_operatorMany", "add_operator_at", "add_operator_atMany", "set_shape", "set_shapeMany", "apply_shape", "apply_shapeMany", "apply_width", "apply_widthMany", "tap_macro", "tap_macroMany", "set_output_width", "set_output_widthMany", "move_operator", "move_operatorMany", "remove_operator", "remove_operatorMany", "connect", "connectMany", "disconnect", "disconnectMany", "resolve_graph", "resolve_graphMany", "apply_graph", "apply_graphMany", "inspect_seen_macro", "inspect_seen_macroMany", "tap_graph", "tap_graphMany"] as const;

/** The `tl.so.tilo.chip-design.Project` kind at `path`. */
export function project(ws: tilo.Workspace, path: string): Project {
  return resourceProxy(ws, path, SURFACE, PROJECT_ACTIONS) as unknown as Project;
}

/** ImportAsk — `tl.so.tilo.chip-design.ImportAsk`. */
export interface ImportAsk extends tilo.Resource {
  fmuu7ss2: string;  // json
}

/** The actions `ImportAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IMPORTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.ImportAsk` kind at `path`. */
export function importAsk(ws: tilo.Workspace, path: string): ImportAsk {
  return resourceProxy(ws, path, SURFACE, IMPORTASK_ACTIONS) as unknown as ImportAsk;
}

/** NodeAsk — `tl.so.tilo.chip-design.NodeAsk`. */
export interface NodeAsk extends tilo.Resource {
  zcgjq6hw: string;  // id
  xj4chp77: OperatorKind;  // kind
  qzhhc7sz: number[];  // shape
  gpmyx3jk: number;  // output_width
  lcejfaxh: number;  // x
  sjxnosz6: number;  // y
}

/** The actions `NodeAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NODEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.NodeAsk` kind at `path`. */
export function nodeAsk(ws: tilo.Workspace, path: string): NodeAsk {
  return resourceProxy(ws, path, SURFACE, NODEASK_ACTIONS) as unknown as NodeAsk;
}

/** EdgeAsk — `tl.so.tilo.chip-design.EdgeAsk`. */
export interface EdgeAsk extends tilo.Resource {
  cem5h5og: string;  // from_node
  og4slduv: string;  // from_port
  esecrerr: string;  // to_node
  "7ydvdfsk": string;  // to_port
}

/** The actions `EdgeAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EDGEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.EdgeAsk` kind at `path`. */
export function edgeAsk(ws: tilo.Workspace, path: string): EdgeAsk {
  return resourceProxy(ws, path, SURFACE, EDGEASK_ACTIONS) as unknown as EdgeAsk;
}

/** GraphAsk — `tl.so.tilo.chip-design.GraphAsk`. */
export interface GraphAsk extends tilo.Resource {
  "6ydpdgb2": NodeAsk[];  // nodes
  "5sfjy4uo": EdgeAsk[];  // connections
}

/** The actions `GraphAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GRAPHASK_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.GraphAsk` kind at `path`. */
export function graphAsk(ws: tilo.Workspace, path: string): GraphAsk {
  return resourceProxy(ws, path, SURFACE, GRAPHASK_ACTIONS) as unknown as GraphAsk;
}

/** OutcomeOf — `tl.so.tilo.chip-design.OutcomeOf`. */
export interface OutcomeOf extends tilo.Resource {
  bf2avrry: string;  // digest
  wfpvvvxh: boolean;  // active
  "7ezbtipq": string;  // reason
}

/** The actions `OutcomeOf` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OUTCOMEOF_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design.OutcomeOf` kind at `path`. */
export function outcomeOf(ws: tilo.Workspace, path: string): OutcomeOf {
  return resourceProxy(ws, path, SURFACE, OUTCOMEOF_ACTIONS) as unknown as OutcomeOf;
}

/** Chip Design — `tl.so.tilo.chip-design`. */
export interface ChipDesign extends tilo.Resource {
}

/** The actions `ChipDesign` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHIPDESIGN_ACTIONS = [] as const;

/** The `tl.so.tilo.chip-design` kind at `path`. */
export function chipDesign(ws: tilo.Workspace, path: string): ChipDesign {
  return resourceProxy(ws, path, SURFACE, CHIPDESIGN_ACTIONS) as unknown as ChipDesign;
}

