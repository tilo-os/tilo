app so.tilo.shortcuts "Shortcuts" (
    icon: .zap,
    category: .utility,
    tagline: "Chains your apps' actions into shortcuts",
    keywords: ["automation", "workflows", "automate", "scripts", "recipes", "macros", "automator", "power automate", "kurzbefehle", "automatisierung", "skripte"],
    splash_budget: 250,
)

use tilo.actions as actions
use tilo.invocations as invocations

/// Which block of a step the step inside it runs in.
enum Branch {
    top
    then
    otherwise
    each
}

/// What a step does.
enum Head {
    invoke
    branch
    each
    set
    ask
    ai
    task
    read
}

/// What a run does when a step fails.
enum Policy {
    fail
    retry
    fallback
    carry_on
}

/// How a step names the resource its action is called on.
enum Selector {
    fixed
    kind
    binding
}

/// The kinds of value a shortcut takes when it is started.
enum InputType {
    text
    number
    integer
    boolean
    date_time
    resource
}

/// What a focus-mode trigger watches: Do Not Disturb or the focus schedule.
enum FocusMode {
    do_not_disturb
    focus
}

/// Whether it watches the mode switching on or off.
enum FocusTransition {
    enter
    exit
}

/// Which network changes a network trigger counts.
enum Connected {
    any
    connected
    disconnected
}

/// Whose changes a record trigger counts.
enum By {
    anyone
    people
    not_bots
}

/// What starts a saved shortcut on its own.
enum TriggerKind {
    keyboard
    app_opened
    schedule_day
    schedule_every
    once
    workspace_joined
    file_changed
    focus_mode
    network_changed
    device_connected
    resource_changed
    app_event
    incoming_message
    nearby_device
    resource_time
    source_changed
}

/// One parameter an action takes, as the catalogue names it.
type Param (title: name, untitled: "Parameter") {
    name: Text
    /// Its type in the catalogue's words: `text`, `int`, `float`, `bool`, `resource`, `object` or
    /// `list`.
    ty: Text = "text"
    required: Bool = true
    description: Text = ""
}

/// How a value is held: typed, one of the shortcut's inputs, a variable, an earlier step's
/// output, text with placeholders, or an object or list of values.
enum Bind {
    none
    literal
    input
    variable
    step
    template
    object
    list
}

/// One value an object or a list holds: the name it stands at in the object, or its position
/// in the list.
type Member {
    at: Text = ""
    value: BindValue
}

/// A value an argument, a block, an output or a condition is bound to, with what an object or
/// list holds inside it. It is a field of the shortcut rather than a JSON document, so two
/// people editing different parts both keep their edit, and views, search and agents read it.
type BindValue (untitled: "Value") {
    kind: Bind = .literal
    /// Its type in the catalogue's words: `text`, `int`, `bool`, `list`, a record's kind.
    ty: Text = "text"
    /// The input or variable it names (with `/` parts into it), an earlier step's id, or a
    /// template's text.
    name: Text = ""
    /// The earlier step's output it reads, by its RFC 6901 pointer.
    pointer: Text = ""
    /// The value typed for a literal, as the person wrote it.
    literal: Text = ""
    /// An object's members, by the name each stands at, or a list's values in order.
    members: Collection<Member>
}

/// What a branch asks.
enum Cond {
    truthy
    equals
    negate
    all
    any
}

/// A branch's condition: a value being set, two values being equal, or conditions joined.
type Condition {
    kind: Cond = .truthy
    left: BindValue
    right: BindValue
    /// The conditions `not`, `all` and `any` hold.
    inner: Collection<Condition>
}

/// One value a parameter may be bound to, as the checker accepts it. It carries a value's parts,
/// so a person's pick is copied into the argument's own value rather than shared with the answer.
type Choice {
    kind: Bind = .none
    ty: Text = "text"
    /// The input or variable it names, an earlier step's id, or a template's text.
    name: Text = ""
    pointer: Text = ""
    literal: Text = ""
    describes: Text = ""
}

/// How a value a person chose is kept: not chosen, one of the values in scope, or a value typed.
enum Pick {
    none
    choice
    literal
}

/// One parameter of an action step and where its value comes from.
type Arg (title: name, untitled: "Argument") {
    name: Text
    /// Its type in the catalogue's words.
    ty: Text = "text"
    pick: Pick = .none
    value: BindValue
    /// What the checker accepts for it, read when the draft was last checked.
    choices: Collection<Choice>
}

/// One value a step's block reads (a condition, a repeat's items, a variable's value), kept the
/// way an argument is.
type Slot (title: name, untitled: "Value") {
    name: Text
    ty: Text = "text"
    pick: Pick = .none
    value: BindValue
}

/// What the checker found with a draft, at one step (or at the whole of it).
type Problem {
    path: Text = ""
    message: Text = ""
}

/// One thing saving a draft would let it do, in the words a person reads.
type Grant {
    label: Text
    kind: Text = ""
}

/// One step of a draft, wherever it sits: its block and its head are stored beside its arguments
/// and the blocks inside it, so two people editing different steps both keep their edit.
type Step (untitled: "Step") {
    /// The step whose block holds this one; nothing at the top level.
    parent: Option<Step>
    branch: Branch = .top
    /// Its own id, which a binding to its output names.
    step_id: Text = ""
    head: Head = .invoke
    /// An action step's action and where the catalogue put it.
    app_id: Text = ""
    action_kind: Text = ""
    action_name: Text = ""
    action_label: Text = ""
    resource_label: Text = ""
    selector: Selector = .fixed
    /// The resource it acts on: a stable id, or a kind when the step picks the first one.
    target: Text = ""
    /// The binding it picks its resource from, when the resource is known only at the run.
    resource_binding: BindValue
    params: Collection<Param>
    args: Collection<Arg>
    /// A branch's condition, a repeat's items, a variable's value and what the agent reads: the
    /// bindings each value row holds, by the name the row goes by.
    slots: Collection<Slot>
    /// A branch's condition, as the checker reads one.
    condition: Condition
    /// A repeat's items, as a binding's JSON, and the variable each one takes.
    items: Text = ""
    variable: Text = "item"
    /// A variable step's name and value.
    name: Text = "value"
    value: Text = ""
    /// What an ask step asks for.
    ask_name: Text = "answer"
    ask_type: InputType = .text
    ask_description: Text = "Your answer"
    /// What an agent step is told, and what it reads.
    prompt: Text = ""
    ai_input: BindValue
    ai_output: Text = ""
    /// What a task step is to do, what it may use, and the shape of its answer.
    instruction: Text = ""
    allow: Text = "[]"
    task_output: Text = ""
    /// What a read step reads.
    read_app: Text = ""
    read_record: Text = ""
    read_joined: Bool = false
    /// What the run does when the step fails.
    policy: Policy = .fail
    attempts: Float = 3.0
    backoff_ms: Float = 1000.0
    default_value: BindValue
}

/// One value a shortcut is started with.
type Input (title: name, untitled: "Input") {
    name: Text
    ty: InputType = .text
    required: Bool = true
    description: Text = ""
    /// What the form that runs the shortcut holds for it; empty until typed.
    value: Text = ""
}

/// What starts a saved shortcut on its own. Every kind's fields are kept, so switching the kind
/// of an existing trigger keeps what the person set for the other ones.
type Trigger (untitled: "Trigger") {
    kind: TriggerKind = .keyboard
    chord: Text = ""
    hour: Float = 8.0
    minute: Float = 0.0
    weekdays: List<Text>
    catch_up: Bool = true
    period_secs: Float = 3600.0
    /// The one moment it fires at, in milliseconds since the Unix epoch.
    once_ms: Int = 0
    /// The moment as the person wrote it, which the form edits.
    once_text: Text = ""
    app_id: Text = ""
    workspace: Text = ""
    folder: Text = ""
    focus_mode: FocusMode = .do_not_disturb
    focus_transition: FocusTransition = .enter
    network: Text = ""
    connected: Connected = .any
    device_class: Text = ""
    record_kind: Text = ""
    actions: List<Text>
    only_fields: List<Text>
    /// The same two as the trigger form edits them, one per comma.
    actions_text: Text = ""
    fields_text: Text = ""
    by: By = .anyone
    event_kind: Text = ""
    from: Text = ""
    device: Text = ""
    time_field: Text = ""
    offset_secs: Float = 0.0
    filter: Text = ""
    source_at: Text = ""
    every_secs: Float = 0.0
}

/// What a person can add in the action picker while it is open: one action an app
/// offers, with what it takes.
type Offer (title: label, untitled: "Action") {
    app_id: Text = ""
    app_label: Text = ""
    label: Text = ""
    resource: Text = ""
    resource_kind: Text = ""
    resource_id: Text = ""
    address: Text = ""
    /// Whether it only reads.
    reads: Bool = false
    params: Collection<Param>
}

/// What the agent answered when a person asked it to draft a shortcut.
/// What a shortcut is made of: its inputs, its steps, what it reports, what starts it and what
/// its steps may do. It is the shape a draft is written in and a saved shortcut keeps, so a saved
/// shortcut is read, searched and edited like the draft it came from.
type Program {
    inputs: Collection<Input>
    steps: Collection<Step>
    triggers: Collection<Trigger>
    /// What a finished run reports back; a value of kind `none` reports nothing.
    output: BindValue
    /// How long each step may take, in seconds; zero is the engine's own patience.
    step_timeout_secs: Int = 0
    /// The least grant saving it stores, as the engine's rules.
    grant: Text = "[]"
}

type Suggested (untitled: "Suggested shortcut", readers: author) {
    name: Text = ""
    /// The shortcut the agent wrote, read into the same typed program a draft is.
    program: Program
    problems: List<Text>
    changes: List<Text>
}

/// One shortcut saved in the workspace, as the app last read the library.
type Saved (title: name, untitled: "Shortcut", label: "Saved shortcut", noun: "shortcut") {
    id: Text = ""
    name: Text = ""
    /// The saved shortcut: its steps, its grant and its triggers.
    program: Program
    paused: Bool = false
    /// The newest run of it the app started, by its invocation's id, and when it started.
    last_call: Text = ""
    last_started: Int = 0
}

/// A shortcut being written: its inputs, its steps, what it reports and what starts it.
type Draft (title: name, untitled: "New shortcut") {
    name: Text = ""
    /// The id it is saved under.
    shortcut_id: Text = ""
    replaces_saved: Bool = false
    program: Program
    /// What the checker said when the draft last changed, kept in front of the person while they
    /// build it.
    problems: Collection<Problem>
    grants: Collection<Grant>
    /// The steps the person folded away, by path, and whether they read the program as text.
    folded: Text = ""
    as_text: Bool = false
}

/// The workspace's Shortcuts app: the drafts people are writing, the shortcuts saved from them,
/// and what the agent offered.
type Workbench (untitled: "Shortcuts") {
    drafts: Collection<Draft>
    saved: Collection<Saved>
    suggested: Collection<Suggested>
    /// What the last save, run or delete came to, and what went wrong, until the next one.
    notice: Text = ""
    failure: Text = ""
    /// The trigger form's fields, and the trigger it replaces when one is being edited.
    form: Trigger
    editing: Option<Trigger>
    /// Which sheet the window shows (`save` or empty), what the person typed as a description
    /// and a refine comment, and what the open picker's filter holds.
    sheet: Text = ""
    description: Text = ""
    comment: Text = ""
    picker_query: Text = ""
    new_input_name: Text = ""
    new_input_type: InputType = .text
    /// The repeated work the person said no to, by its key.
    ignored: Set<Text>
    /// The actions the open picker offers, read when it opened, and the draft and block it adds
    /// to.
    picker: Collection<Offer>
    picker_for: Option<Draft>
    picker_block: Text = ""
    /// The catalogue is being read for the open picker, and why it could not be when it could not.
    picker_loading: Bool = false
    picker_failure: Text = ""
    /// The draft New shortcut last made, which the window shows: a press of that button runs the
    /// action itself, so the draft it makes is opened here and never only listed in the sidebar.
    opened: Text = ""
}

root workbench: Workbench

/// `text` as JSON holds it.
// ---------------------------------------------------------------------------
// JSON text: what the shortcut engine and the action registry read
// ---------------------------------------------------------------------------
fn js(text: Text) -> Text {
    json.encode(text)
}

fn js_opt(text: Text) -> Text {
    if text == "" { "null" } else { js(text) }
}

fn js_flag(flag: Bool) -> Text {
    if flag { "true" } else { "false" }
}

fn js_list(items: List<Text>) -> Text {
    "[" + items.join(",") + "]"
}

/// The value at `key` of a JSON object, and nothing for anything else.
fn at(value: json.Value, key: Text) -> Option<json.Value> {
    match value {
        .object(fields) => fields.get(key),
        .array(items) => .none,
        .string(text) => .none,
        .number(number) => .none,
        .bool(flag) => .none,
        .null => .none,
    }
}

/// The text at `key`, and empty when it holds none.
fn at_text(value: json.Value, key: Text) -> Text {
    match at(value, key) {
        .none => "",
        .some(held) => text_of(held),
    }
}

fn at_int(value: json.Value, key: Text) -> Int {
    match at(value, key) {
        .none => 0,
        .some(held) => match held {
            .number(number) => number.to_int(),
            .string(text) => text.to_int() ?? 0,
            .object(fields) => 0,
            .array(items) => 0,
            .bool(flag) => 0,
            .null => 0,
        },
    }
}

fn at_flag(value: json.Value, key: Text) -> Bool {
    match at(value, key) {
        .none => false,
        .some(held) => match held {
            .bool(flag) => flag,
            .string(text) => text == "true",
            .object(fields) => false,
            .array(items) => false,
            .number(number) => false,
            .null => false,
        },
    }
}

/// The entries at `key`, and nothing for anything else.
fn at_list(value: json.Value, key: Text) -> List<json.Value> {
    match at(value, key) {
        .none => [],
        .some(held) => match held {
            .array(items) => items,
            .object(fields) => [],
            .string(text) => [],
            .number(number) => [],
            .bool(flag) => [],
            .null => [],
        },
    }
}

fn at_texts(value: json.Value, key: Text) -> List<Text> {
    at_list(value, key).map(one -> text_of(one)).filter(one -> one != "")
}

/// A JSON value as the text it holds, and nothing for anything else.
fn text_of(value: json.Value) -> Text {
    match value {
        .string(text) => text,
        .object(fields) => "",
        .array(items) => "",
        .number(number) => "",
        .bool(flag) => "",
        .null => "",
    }
}

/// `value` as the JSON text it is.
fn written(value: json.Value) -> Text {
    json.encode(value)
}

/// A JSON object or list as its text, and `fallback` for anything else.
fn piece_text(value: Option<json.Value>, fallback: Text) -> Text {
    match value {
        .none => fallback,
        .some(held) => match held {
            .object(fields) => written(held),
            .array(items) => written(held),
            .string(text) => fallback,
            .number(number) => fallback,
            .bool(flag) => fallback,
            .null => fallback,
        },
    }
}

/// Text the person typed as a JSON value of `ty`, for a text-like parameter.
fn literal_json(text: Text, ty: Text) -> Text {
    if ty == "bool" {
        if text.trim() == "true" { "true" } else { "false" }
    } else if ty == "int" {
        let number = text.trim().to_int() ?? 0
        "{number}"
    } else if ty == "float" {
        let number = text.trim().to_float() ?? 0.0
        "{number}"
    } else if ty == "list" or ty == "object" {
        match json.parse(text) {
            .ok(value) => written(value),
            .err(why) => empty_json(ty),
        }
    } else { js(text) }
}

/// An empty list or object, as `ty` names one.
fn empty_json(ty: Text) -> Text {
    if ty == "list" { "[]" } else { brace_open() + brace_close() }
}

/// A JSON object's opening and closing marks, written where a text is not interpolated.
fn brace_open() -> Text {
    "\{"
}

fn brace_close() -> Text {
    "\}"
}

/// The JSON an input's typed value is: what a run is started with.
fn input_value(i: Input) -> Text {
    if i.ty == .boolean {
        js_flag(i.value == "true")
    } else if i.ty == .integer {
        let number = i.value.trim().to_int() ?? 0
        "{number}"
    } else if i.ty == .number {
        let number = i.value.trim().to_float() ?? 0.0
        "{number}"
    } else { js(i.value) }
}

// ---------------------------------------------------------------------------
// The engine's words and the app's
// ---------------------------------------------------------------------------
fn input_word(ty: InputType) -> Text {
    if ty == .text {
        "text"
    } else if ty == .integer {
        "integer"
    } else if ty == .number {
        "number"
    } else if ty == .boolean {
        "boolean"
    } else if ty == .date_time { "datetime" } else { "resource" }
}

fn input_of(word: Text) -> InputType {
    if word == "integer" or word == "int" {
        .integer
    } else if word == "number" or word == "float" {
        .number
    } else if word == "boolean" or word == "bool" {
        .boolean
    } else if word == "datetime" {
        .date_time
    } else if word == "resource" { .resource } else { .text }
}

fn input_label(ty: InputType) -> Text {
    if ty == .text {
        "Text"
    } else if ty == .integer {
        "Whole number"
    } else if ty == .number {
        "Number"
    } else if ty == .boolean {
        "Yes or no"
    } else if ty == .date_time { "Date and time" } else { "Resource" }
}

fn branch_word(branch: Branch) -> Text {
    if branch == .then {
        "then"
    } else if branch == .otherwise { "else" } else if branch == .each { "each" } else { "" }
}

fn branch_of(word: Text) -> Branch {
    if word == "then" {
        .then
    } else if word == "else" { .otherwise } else if word == "each" { .each } else { .top }
}

fn head_word(head: Head) -> Text {
    if head == .invoke {
        "invoke"
    } else if head == .branch {
        "if"
    } else if head == .each {
        "for_each"
    } else if head == .set {
        "set_var"
    } else if head == .ask {
        "ask"
    } else if head == .ai { "ai" } else if head == .task { "task" } else { "read" }
}

fn head_of(word: Text) -> Option<Head> {
    if word == "invoke" {
        .some(.invoke)
    } else if word == "if" {
        .some(.branch)
    } else if word == "for_each" {
        .some(.each)
    } else if word == "set_var" {
        .some(.set)
    } else if word == "ask" {
        .some(.ask)
    } else if word == "ai" {
        .some(.ai)
    } else if word == "task" { .some(.task) } else if word == "read" { .some(.read) } else { .none }
}

fn trigger_word(kind: TriggerKind) -> Text {
    if kind == .keyboard {
        "keyboard"
    } else if kind == .app_opened {
        "app_opened"
    } else if kind == .schedule_day {
        "schedule"
    } else if kind == .schedule_every {
        "schedule"
    } else if kind == .once {
        "once"
    } else if kind == .workspace_joined {
        "workspace_joined"
    } else if kind == .file_changed {
        "file_changed"
    } else if kind == .focus_mode {
        "focus_mode"
    } else if kind == .network_changed {
        "network_changed"
    } else if kind == .device_connected {
        "device_connected"
    } else if kind == .resource_changed {
        "resource_changed"
    } else if kind == .app_event {
        "app_event"
    } else if kind == .incoming_message {
        "incoming_message"
    } else if kind == .nearby_device {
        "nearby_device"
    } else if kind == .resource_time { "resource_time" } else { "source_changed" }
}

/// Where a step moves to: the block (a step and its branch, or the top level) and its place there.
// ---------------------------------------------------------------------------
// A draft's steps, as a tree
// ---------------------------------------------------------------------------
type Place {
    parent: Option<Step>
    branch: Branch = .top
    index: Int = 0
}

/// The steps of one block, in the order the draft keeps them.
fn siblings(p: Program, parent: Option<Step>, branch: Branch) -> List<Step> {
    p.steps.filter(s -> s.parent == parent and s.branch == branch)
}

/// The steps of the block an editor path names (`""`, `2.then`, `2.else` or `3.each`).
fn block_steps(p: Program, address: Text) -> List<Step> {
    if address == "" {
        return siblings(p, .none, .top)
    }
    let parts = address.split(".")
    let owner = parts.take(parts.len() - 1).join(".")
    let branch = branch_of(parts.last() ?? "")
    if let step = step_at(p, owner) { siblings(p, .some(step), branch) } else { [] }
}

/// The step at an editor path such as `2.then.1`.
fn step_at(p: Program, path: Text) -> Option<Step> {
    if path == "" {
        return .none
    }
    let parts = path.split(".")
    let index = (parts.last() ?? "").to_int() ?? -1
    if index < 0 {
        return .none
    }
    block_steps(p, parts.take(parts.len() - 1).join(".")).at(index)
}

/// The path the checker and the editor name a step by.
fn path_of(prog: Program, s: Step) -> Text {
    let index = siblings(prog, s.parent, s.branch).index_of(s) ?? 0
    if let parent = s.parent {
        "{path_of(prog, parent)}.{branch_word(s.branch)}.{index}"
    } else { "{index}" }
}

/// The steps of the draft in reading order, branches included.
fn reading(p: Program) -> List<Step> {
    read_block(p, .none, .top)
}

fn read_block(p: Program, parent: Option<Step>, branch: Branch) -> List<Step> {
    let found: List<Step> = []
    for s in siblings(p, parent, branch) {
        found = [found, [s]].flat_map(one -> one)
        if s.head == .branch {
            found = [found, read_block(p, .some(s), .then)].flat_map(one -> one)
            found = [found, read_block(p, .some(s), .otherwise)].flat_map(one -> one)
        } else if s.head == .each {
            found = [found, read_block(p, .some(s), .each)].flat_map(one -> one)
        }
    }
    found
}

/// The position (from 1) the editor shows for a step: steps are numbered in reading order.
fn number_of(p: Program, s: Step) -> Int {
    (reading(p).index_of(s) ?? -1) + 1
}

/// The first branch a step opens, if it holds steps of its own.
fn opened_branch(s: Step) -> Option<Branch> {
    if s.head == .branch { .some(.then) } else if s.head == .each { .some(.each) } else { .none }
}

/// Where a step can move deeper: into the block of the step just above it.
fn nest_place(p: Program, s: Step) -> Option<Place> {
    let beside = siblings(p, s.parent, s.branch)
    let index = beside.index_of(s) ?? 0
    if index == 0 {
        return .none
    }
    let above = beside.at(index - 1) ?? s
    if let branch = opened_branch(above) {
        .some(
            Place(
                parent: .some(above),
                branch: branch,
                index: siblings(p, .some(above), branch).len(),
            ),
        )
    } else { .none }
}

/// Where a step can move out: into its own block's, just after the step that holds it.
fn lift_place(prog: Program, s: Step) -> Option<Place> {
    if let parent = s.parent {
        let beside = siblings(prog, parent, parent.branch)
        return .some(
            Place(
                parent: parent.parent,
                branch: parent.branch,
                index: (beside.index_of(parent) ?? 0) + 1,
            ),
        )
    }
    return .none
}

/// A readable step id no other step has.
fn fresh_step_id(p: Program, base: Text) -> Text {
    let start = base.trim().replace(".", "-").replace("/", "-").replace(" ", "-")
    free_step_id(p, if start == "" { "step" } else { start }, 1)
}

fn free_step_id(p: Program, base: Text, number: Int) -> Text {
    let candidate = if number <= 1 { base } else { "{base}{number}" }
    if p.steps.any(s -> s.step_id == candidate) {
        free_step_id(p, base, number + 1)
    } else { candidate }
}

// ---------------------------------------------------------------------------
// A draft, as the shortcut the engine reads
// ---------------------------------------------------------------------------
fn input_json(i: Input) -> Text {
    brace_open()
        + "\"name\":"
        + js(i.name)
        + ",\"ty\":"
        + js(input_word(i.ty))
        + ",\"required\":"
        + js_flag(i.required)
        + ",\"description\":"
        + js(i.description)
        + brace_close()
}

fn inputs_json(p: Program) -> Text {
    "[" + p.inputs.map(i -> input_json(i)).join(",") + "]"
}

/// A value as the engine's binding JSON.
fn value_json(v: BindValue) -> Text {
    if v.kind == .input {
        brace_open() + "\"input\":" + js(v.name) + brace_close()
    } else if v.kind == .variable {
        brace_open() + "\"var\":" + js(v.name) + brace_close()
    } else if v.kind == .step {
        let out = "\"step_output\":" + brace_open() + "\"step\":" + js(v.name)
        if v.pointer == "" {
            out + brace_close()
        } else {
            out + ",\"pointer\":" + js(v.pointer) + brace_close()
        }
    } else if v.kind == .template {
        brace_open() + "\"template\":" + js(v.name) + brace_close()
    } else if v.kind == .object {
        brace_open()
            + "\"object\":"
            + brace_open()
            + v.members.map(m -> js(m.at) + ":" + value_json(m.value)).join(",")
            + brace_close()
            + brace_close()
    } else if v.kind == .list {
        brace_open()
            + "\"list\":["
            + v.members.map(m -> value_json(m.value)).join(",")
            + "]"
            + brace_close()
    } else {
        brace_open() + "\"literal\":" + literal_json(v.literal, v.ty) + brace_close()
    }
}

/// A branch's condition as the engine reads one.
fn condition_json(c: Condition) -> Text {
    if c.kind == .equals {
        brace_open()
            + "\"equals\":"
            + brace_open()
            + "\"left\":"
            + value_json(c.left)
            + ",\"right\":"
            + value_json(c.right)
            + brace_close()
            + brace_close()
    } else if c.kind == .negate {
        brace_open() + "\"not\":" + condition_json(c.inner.first() ?? Condition()) + brace_close()
    } else if c.kind == .all {
        brace_open()
            + "\"all\":["
            + c.inner.map(one -> condition_json(one)).join(",")
            + "]"
            + brace_close()
    } else if c.kind == .any {
        brace_open()
            + "\"any\":["
            + c.inner.map(one -> condition_json(one)).join(",")
            + "]"
            + brace_close()
    } else {
        brace_open() + "\"truthy\":" + value_json(c.left) + brace_close()
    }
}

/// The value a person picked for `name`, as the engine's JSON; empty when nothing is picked.
fn slot_value(s: Step, name: Text) -> Text {
    if let slot = s.slots.find(one -> one.name == name) {
        if slot.pick == .none { "" } else { value_json(slot.value) }
    } else { "" }
}

fn arg_value(a: Arg) -> Text {
    if a.pick == .none { "" } else { value_json(a.value) }
}

fn shortcut_json(d: Draft, grant: Text) -> Text {
    let out = "\"id\":" + js(d.shortcut_id) + ",\"name\":" + js(d.name.trim())
    if not d.program.inputs.is_empty() {
        out = out + ",\"inputs\":" + inputs_json(d.program)
    }
    out = out + ",\"steps\":" + block_json(d.program, .none, .top)
    if d.program.output.kind != .none {
        out = out + ",\"output\":" + value_json(d.program.output)
    }
    if d.program.step_timeout_secs > 0 {
        let secs = d.program.step_timeout_secs
        out = out + ",\"step_timeout_secs\":{secs}"
    }
    out = out + ",\"grant\":" + grant
    if not d.program.triggers.is_empty() {
        out = out + ",\"triggers\":" + triggers_json(d.program)
    }
    brace_open() + out + brace_close()
}

fn block_json(p: Program, parent: Option<Step>, branch: Branch) -> Text {
    "[" + siblings(p, parent, branch).map(s -> step_json(p, s)).join(",") + "]"
}

fn step_json(p: Program, s: Step) -> Text {
    let out = "\"id\":" + js(s.step_id)
    if s.policy != .fail {
        out = out + "," + policy_json(s)
    }
    out = out + "," + head_json(p, s)
    brace_open() + out + brace_close()
}

fn policy_json(s: Step) -> Text {
    if s.policy == .retry {
        "\"on_error\":"
            + brace_open()
            + "\"on_error\":\"retry\",\"attempts\":"
            + "{s.attempts.to_int()}"
            + ",\"backoff_ms\":"
            + "{s.backoff_ms.to_int()}"
            + brace_close()
    } else if s.policy == .fallback {
        "\"on_error\":" + brace_open() + "\"on_error\":\"fallback\",\"steps\":[]" + brace_close()
    } else {
        let value = if s.default_value.kind == .none { "null" } else { value_json(s.default_value) }
        "\"on_error\":"
            + brace_open()
            + "\"on_error\":\"default\",\"value\":"
            + value
            + brace_close()
    }
}

fn head_json(p: Program, s: Step) -> Text {
    if s.head == .branch {
        let value = slot_value(s, "condition")
        let cond = if value != "" {
            brace_open() + "\"truthy\":" + value + brace_close()
        } else { condition_json(s.condition) }
        "\"step\":\"if\",\"cond\":"
            + cond
            + ",\"then\":"
            + block_json(p, .some(s), .then)
            + ",\"else\":"
            + block_json(p, .some(s), .otherwise)
    } else if s.head == .each {
        let value = slot_value(s, "items")
        let items = if value == "" {
            brace_open() + "\"literal\":[]" + brace_close()
        } else { value }
        "\"step\":\"for_each\",\"items\":"
            + items
            + ",\"item\":"
            + js(s.variable)
            + ",\"body\":"
            + block_json(p, .some(s), .each)
    } else if s.head == .set {
        let value = slot_value(s, "value")
        let held = if value == "" {
            brace_open() + "\"literal\":\"\"" + brace_close()
        } else { value }
        "\"step\":\"set_var\",\"name\":" + js(s.name) + ",\"value\":" + held
    } else if s.head == .ask {
        "\"step\":\"ask\",\"param\":"
            + brace_open()
            + "\"name\":"
            + js(s.ask_name)
            + ",\"ty\":"
            + js(input_word(s.ask_type))
            + ",\"required\":true,\"description\":"
            + js(s.ask_description)
            + brace_close()
    } else if s.head == .ai {
        let out = "\"step\":\"ai\",\"prompt\":" + js(s.prompt)
        let source = slot_value(s, "ai_input")
        let with_input = if source != "" { out + ",\"input\":" + source } else { out }
        let with_output = if s.ai_output == "" {
            with_input
        } else {
            with_input + ",\"output\":" + s.ai_output
        }
        with_output
    } else if s.head == .task {
        let out = "\"step\":\"task\",\"instruction\":"
            + js(s.instruction)
            + ",\"allow\":"
            + if s.allow == "" { "[]" } else { s.allow }
        if s.task_output == "" { out } else { out + ",\"output\":" + s.task_output }
    } else if s.head == .read {
        let out = "\"step\":\"read\",\"app\":" + js(s.read_app) + ",\"record\":" + js(s.read_record)
        if s.read_joined { out + ",\"joined\":true" } else { out }
    } else {
        let selector = if s.selector == .kind {
            brace_open() + "\"first_of_kind\":" + js(s.target) + brace_close()
        } else if s.selector == .binding {
            brace_open() + "\"from_binding\":" + if s.resource_binding.kind == .none { "null" } else { value_json(s.resource_binding) } + brace_close()
        } else {
            brace_open() + "\"fixed\":" + js(s.target) + brace_close()
        }
        let given = s.args.filter(one -> arg_value(one) != "")
        let args = if given.is_empty() {
            ""
        } else {
            ",\"args\":"
                + brace_open()
                + given.map(one -> js(one.name) + ":" + arg_value(one)).join(",")
                + brace_close()
        }
        "\"step\":\"invoke\",\"action\":"
            + brace_open()
            + "\"app\":"
            + js(s.app_id)
            + ",\"kind\":"
            + js(s.action_kind)
            + ",\"action\":"
            + js(s.action_name)
            + brace_close()
            + ",\"resource\":"
            + selector
            + args
    }
}

// ---------------------------------------------------------------------------
// What starts a saved shortcut
// ---------------------------------------------------------------------------
fn triggers_json(p: Program) -> Text {
    "[" + p.triggers.map(t -> trigger_json(t)).join(",") + "]"
}

fn focus_word(mode: FocusMode) -> Text {
    if mode == FocusMode.do_not_disturb { "do_not_disturb" } else { "focus" }
}

fn focus_of(word: Text) -> FocusMode {
    if word == "focus" { FocusMode.focus } else { FocusMode.do_not_disturb }
}

fn transition_word(transition: FocusTransition) -> Text {
    if transition == FocusTransition.exit { "exit" } else { "enter" }
}

fn transition_of(word: Text) -> FocusTransition {
    if word == "exit" { FocusTransition.exit } else { FocusTransition.enter }
}

fn by_word(by: By) -> Text {
    if by == By.people { "people" } else if by == By.not_bots { "not_bots" } else { "anyone" }
}

fn by_of(word: Text) -> By {
    if word == "people" { By.people } else if word == "not_bots" { By.not_bots } else { By.anyone }
}

fn catch_up_word(run_once: Bool) -> Text {
    if run_once { "run_once" } else { "skip" }
}

/// A filter written as a JSON object, and nothing for text that is not one.
fn filter_field(text: Text) -> Text {
    if let held = object_of(text) {
        return ",\"filter\":" + written(held)
    }
    return ""
}

fn object_of(text: Text) -> Option<json.Value> {
    let held = text.trim()
    if held == "" {
        return .none
    }
    match json.parse(held) {
        .ok(value) => {
            match value {
                .object(fields) => {
                    if written(value) == brace_open() + brace_close() {
                        .none
                    } else { .some(value) }
                }
                .array(items) => .none,
                .string(word) => .none,
                .number(number) => .none,
                .bool(flag) => .none,
                .null => .none,
            }
        }
        .err(why) => .none,
    }
}

/// The moment a person wrote, in milliseconds since the Unix epoch; zero when it is not one.
fn moment_ms(text: Text, near: Time) -> Int {
    let held = text.trim()
    if held == "" {
        return 0
    }
    match Time.parse(held, near: near) {
        .none => 0,
        .some(at) => (at - Time("1970-01-01T00:00:00Z")).seconds() * 1000,
    }
}

fn trigger_json(t: Trigger) -> Text {
    if t.kind == .keyboard {
        "\"trigger\":\"keyboard\",\"chord\":" + js(t.chord)
    } else if t.kind == .app_opened {
        "\"trigger\":\"app_opened\",\"app_id\":" + js(t.app_id)
    } else if t.kind == .schedule_day {
        "\"trigger\":\"schedule\",\"catch_up\":"
            + js(catch_up_word(t.catch_up))
            + ",\"schedule\":"
            + brace_open()
            + "\"every\":\"calendar\",\"at\":"
            + brace_open()
            + "\"hour\":"
            + "{t.hour.to_int()}"
            + ",\"minute\":"
            + "{t.minute.to_int()}"
            + brace_close()
            + ",\"weekdays\":"
            + js_list(t.weekdays.map(day -> js(day)))
            + brace_close()
    } else if t.kind == .schedule_every {
        "\"trigger\":\"schedule\",\"catch_up\":"
            + js(catch_up_word(t.catch_up))
            + ",\"schedule\":"
            + brace_open()
            + "\"every\":\"interval\",\"seconds\":"
            + "{t.period_secs.to_int()}"
            + brace_close()
    } else if t.kind == .once {
        "\"trigger\":\"once\",\"at_ms\":" + "{t.once_ms}"
    } else if t.kind == .workspace_joined {
        "\"trigger\":\"workspace_joined\"" + optional_field("workspace", t.workspace)
    } else if t.kind == .file_changed {
        "\"trigger\":\"file_changed\",\"capability\":" + js(t.folder)
    } else if t.kind == .focus_mode {
        "\"trigger\":\"focus_mode\",\"mode\":"
            + js(focus_word(t.focus_mode))
            + ",\"transition\":"
            + js(transition_word(t.focus_transition))
    } else if t.kind == .network_changed {
        let out = "\"trigger\":\"network_changed\"" + optional_field("network", t.network)
        if t.connected == Connected.connected {
            out + ",\"connected\":true"
        } else if t.connected == Connected.disconnected {
            out + ",\"connected\":false"
        } else { out }
    } else if t.kind == .device_connected {
        "\"trigger\":\"device_connected\",\"device_class\":" + js(t.device_class)
    } else if t.kind == .resource_changed {
        "\"trigger\":\"resource_changed\",\"kind\":"
            + js(t.record_kind)
            + ",\"actions\":"
            + js_list(t.actions.map(one -> js(one)))
            + ",\"fields\":"
            + js_list(t.only_fields.map(one -> js(one)))
            + ",\"by\":"
            + js(by_word(t.by))
    } else if t.kind == .app_event {
        "\"trigger\":\"app_event\",\"app_id\":" + js(t.app_id) + ",\"kind\":" + js(t.event_kind)
    } else if t.kind == .incoming_message {
        "\"trigger\":\"incoming_message\""
            + optional_field("app_id", t.app_id)
            + optional_field("from", t.from)
    } else if t.kind == .nearby_device {
        "\"trigger\":\"nearby_device\"" + optional_field("device", t.device)
    } else if t.kind == .resource_time {
        "\"trigger\":\"resource_time\",\"kind\":"
            + js(t.record_kind)
            + ",\"field\":"
            + js(t.time_field)
            + ",\"offset_secs\":"
            + "{t.offset_secs.to_int()}"
            + filter_field(t.filter)
    } else {
        "\"trigger\":\"source_changed\",\"kind\":"
            + js(t.record_kind)
            + optional_field("at", t.source_at)
            + every_field(t.every_secs)
            + filter_field(t.filter)
    }
}

fn optional_field(name: Text, value: Text) -> Text {
    if value.trim() == "" { "" } else { "," + js(name) + ":" + js(value.trim()) }
}

fn every_field(seconds: Float) -> Text {
    if seconds <= 0.0 {
        ""
    } else {
        ",\"every_secs\":" + "{seconds.to_int()}"
    }
}

/// The entries of a JSON list, and nothing for anything else.
// ---------------------------------------------------------------------------
// Reading a shortcut the library or the agent wrote
// ---------------------------------------------------------------------------
fn entries_of(value: json.Value) -> List<json.Value> {
    match value {
        .array(items) => items,
        .object(fields) => [],
        .string(text) => [],
        .number(number) => [],
        .bool(flag) => [],
        .null => [],
    }
}

/// A typed value as the person typed it: its own text, or the JSON it is.
fn literal_text(value: json.Value) -> Text {
    match value {
        .string(text) => text,
        .object(fields) => written(value),
        .array(items) => written(value),
        .number(number) => written(value),
        .bool(flag) => written(value),
        .null => "",
    }
}

/// What type a value in a document is, in the catalogue's words.
fn json_words(value: json.Value) -> Text {
    match value {
        .string(text) => "text",
        .object(fields) => "object",
        .array(items) => "list",
        .number(number) => if written(value).contains(".") { "float" } else { "int" },
        .bool(flag) => "bool",
        .null => "none",
    }
}

/// Reads a value the engine wrote into `into`, and what an object or list holds inside it.
action load_value(into: BindValue, value: json.Value) {
    if let one = at(value, "literal") {
        into.kind = .literal
        into.literal = literal_text(one)
        into.ty = json_words(one)
    } else if let name = at(value, "input") {
        into.kind = .input
        into.name = text_of(name)
    } else if let name = at(value, "var") {
        into.kind = .variable
        into.name = text_of(name)
    } else if let out = at(value, "step_output") {
        into.kind = .step
        into.name = at_text(out, "step")
        into.pointer = at_text(out, "pointer")
    } else if let words = at(value, "template") {
        into.kind = .template
        into.name = text_of(words)
    } else if let members = at(value, "object") {
        into.kind = .object
        match members {
            .object(fields) => {
                for name in fields.keys() {
                    if let one = fields.get(name) {
                        let made = into.members.insert(Member(at: name))
                        load_value(made.value, one)
                    }
                }
            }
            .array(items) => {}
            .string(text) => {}
            .number(number) => {}
            .bool(flag) => {}
            .null => {}
        }
    } else if let items = at(value, "list") {
        into.kind = .list
        let index = 0
        for one in entries_of(items) {
            let made = into.members.insert(Member(at: "{index}"))
            load_value(made.value, one)
            index = index + 1
        }
    } else {
        into.kind = .none
    }
}

/// Reads one value the checker offered into `choice`, whose parts are the value's own.
action load_choice(choice: Choice, value: json.Value) {
    choice.kind = .none
    if let one = at(value, "literal") {
        choice.kind = .literal
        choice.literal = literal_text(one)
        choice.ty = json_words(one)
    } else if let name = at(value, "input") {
        choice.kind = .input
        choice.name = text_of(name)
    } else if let name = at(value, "var") {
        choice.kind = .variable
        choice.name = text_of(name)
    } else if let out = at(value, "step_output") {
        choice.kind = .step
        choice.name = at_text(out, "step")
        choice.pointer = at_text(out, "pointer")
    } else if let words = at(value, "template") {
        choice.kind = .template
        choice.name = text_of(words)
    }
}

/// Reads a condition the engine wrote into `c`.
action load_condition(c: Condition, value: json.Value) {
    if let one = at(value, "truthy") {
        c.kind = .truthy
        load_value(c.left, one)
    } else if let sides = at(value, "equals") {
        c.kind = .equals
        if let left = at(sides, "left") { load_value(c.left, left) }
        if let right = at(sides, "right") { load_value(c.right, right) }
    } else if let one = at(value, "not") {
        c.kind = .negate
        let made = c.inner.insert(Condition())
        load_condition(made, one)
    } else if let all = at(value, "all") {
        c.kind = .all
        for one in entries_of(all) {
            let made = c.inner.insert(Condition())
            load_condition(made, one)
        }
    } else if let any = at(value, "any") {
        c.kind = .any
        for one in entries_of(any) {
            let made = c.inner.insert(Condition())
            load_condition(made, one)
        }
    }
}

/// A value a step's block reads, as a slot: a literal typed, or the binding that holds it.
action load_slot(p: Program, s: Step, name: Text, value: json.Value) {
    let slot = s.slots.insert(Slot(name: name, pick: .choice, ty: json_words(value)))
    if at(value, "literal") != .none {
        slot.pick = .literal
    }
    load_value(slot.value, value)
}

action load_arg(s: Step, name: Text, value: json.Value) {
    let arg = s.args.insert(Arg(name: name, pick: .choice, ty: json_words(value)))
    if at(value, "literal") != .none {
        arg.pick = .literal
    }
    load_value(arg.value, value)
}

fn input_from(value: json.Value) -> Input {
    Input(
        name: at_text(value, "name"),
        ty: input_of(at_text(value, "ty")),
        required: at_flag(value, "required"),
        description: at_text(value, "description"),
    )
}

/// Reads the program a shortcut is made of into `p`.
/// Reads one saved shortcut's entry in the library into `saved`.
action load_saved(saved: Saved, value: json.Value) {
    saved.paused = at_flag(value, "paused")
    load_program(saved.program, value)
}

/// Load `item`'s steps, inputs and output into `p`, and its triggers when `armed` says: a
/// shortcut read from outside is loaded with its triggers left out, so nothing starts before a
/// person saves it.
action load_program(p: Program, item: json.Value, armed: Bool = true) {
    for value in at_list(item, "inputs") {
        p.inputs.insert(input_from(value))
    }
    load_block(p, at(item, "steps"), .none, .top)
    if let out = at(item, "output") { load_value(p.output, out) }
    p.step_timeout_secs = at_int(item, "step_timeout_secs")
    p.grant = piece_text(at(item, "grant"), "[]")
    if armed {
        load_triggers(p, at_list(item, "triggers"))
    }
}

/// A shortcut written as JSON, loaded into `d` as its steps, inputs and triggers.
action load_shortcut(d: Draft, program: json.Value, armed: Bool = true) {
    let name = at_text(program, "name")
    if name != "" {
        d.name = name
    }
    let id = at_text(program, "id")
    if id != "" {
        d.shortcut_id = id
    }
    load_program(d.program, program, armed: armed)
}

action load_block(p: Program, steps: Option<json.Value>, parent: Option<Step>, branch: Branch) {
    if let held = steps {
        for item in entries_of(held) {
            load_step(p, item, parent, branch)
        }
    }
}

action load_step(p: Program, item: json.Value, parent: Option<Step>, branch: Branch) {
    let s = p.steps.insert(Step(parent: parent, branch: branch, step_id: at_text(item, "id")))
    let head = head_of(at_text(item, "step")) ?? .invoke
    s.head = head
    if head == .invoke {
        if let held = at(item, "action") {
            s.app_id = at_text(held, "app")
            s.action_kind = at_text(held, "kind")
            s.action_name = at_text(held, "action")
        }
        if let resource = at(item, "resource") {
            if let fixed = at(resource, "fixed") {
                s.selector = Selector.fixed
                s.target = text_of(fixed)
            } else if let kind = at(resource, "first_of_kind") {
                s.selector = Selector.kind
                s.target = text_of(kind)
            } else if let from = at(resource, "from_binding") {
                s.selector = Selector.binding
                load_value(s.resource_binding, from)
            }
        }
        if let given = at(item, "args") {
            match given {
                .object(fields) => {
                    for name in fields.keys() {
                        if let value = fields.get(name) { load_arg(s, name, value) }
                    }
                }
                .array(items) => {}
                .string(text) => {}
                .number(number) => {}
                .bool(flag) => {}
                .null => {}
            }
        }
    } else if head == .branch {
        if let cond = at(item, "cond") { load_condition(s.condition, cond) }
        load_block(p, at(item, "then"), .some(s), .then)
        load_block(p, at(item, "else"), .some(s), .otherwise)
    } else if head == .each {
        s.variable = at_text(item, "item")
        if s.variable == "" {
            s.variable = "item"
        }
        if let source = at(item, "items") { load_slot(p, s, "items", source) }
        load_block(p, at(item, "body"), .some(s), .each)
    } else if head == .set {
        s.name = at_text(item, "name")
        if let value = at(item, "value") { load_slot(p, s, "value", value) }
    } else if head == .ask {
        if let param = at(item, "param") {
            s.ask_name = at_text(param, "name")
            s.ask_type = input_of(at_text(param, "ty"))
            s.ask_description = at_text(param, "description")
        }
    } else if head == .ai {
        s.prompt = at_text(item, "prompt")
        s.ai_output = piece_text(at(item, "output"), "")
        if let source = at(item, "input") { load_slot(p, s, "ai_input", source) }
    } else if head == .task {
        s.instruction = at_text(item, "instruction")
        s.allow = piece_text(at(item, "allow"), "[]")
        s.task_output = piece_text(at(item, "output"), "")
    } else {
        s.read_app = at_text(item, "app")
        s.read_record = at_text(item, "record")
        s.read_joined = at_flag(item, "joined")
    }
    if let policy = at(item, "on_error") {
        let word = at_text(policy, "on_error")
        if word == "retry" {
            s.policy = Policy.retry
            s.attempts = at_int(policy, "attempts").to_float()
            s.backoff_ms = at_int(policy, "backoff_ms").to_float()
        } else if word == "fallback" {
            s.policy = Policy.fallback
        } else if word == "default" {
            s.policy = Policy.carry_on
            if let value = at(policy, "value") { load_value(s.default_value, value) }
        }
    }
}

action load_triggers(p: Program, triggers: List<json.Value>) {
    for value in triggers {
        let t = Trigger(kind: .keyboard)
        let word = at_text(value, "trigger")
        if word == "keyboard" {
            t.kind = TriggerKind.keyboard
            t.chord = at_text(value, "chord")
        } else if word == "app_opened" {
            t.kind = TriggerKind.app_opened
            t.app_id = at_text(value, "app_id")
        } else if word == "schedule" {
            let schedule = at(value, "schedule") ?? value
            t.catch_up = at_text(value, "catch_up") != "skip"
            if at_text(schedule, "every") == "interval" {
                t.kind = TriggerKind.schedule_every
                t.period_secs = at_int(schedule, "seconds").to_float()
            } else {
                t.kind = TriggerKind.schedule_day
                if let moment = at(schedule, "at") {
                    t.hour = at_int(moment, "hour").to_float()
                    t.minute = at_int(moment, "minute").to_float()
                }
                t.weekdays = at_texts(schedule, "weekdays")
            }
        } else if word == "once" {
            t.kind = TriggerKind.once
            t.once_ms = at_int(value, "at_ms")
        } else if word == "workspace_joined" {
            t.kind = TriggerKind.workspace_joined
            t.workspace = at_text(value, "workspace")
        } else if word == "file_changed" {
            t.kind = TriggerKind.file_changed
            t.folder = at_text(value, "capability")
        } else if word == "focus_mode" {
            t.kind = TriggerKind.focus_mode
            t.focus_mode = focus_of(at_text(value, "mode"))
            t.focus_transition = transition_of(at_text(value, "transition"))
        } else if word == "network_changed" {
            t.kind = TriggerKind.network_changed
            t.network = at_text(value, "network")
            t.connected = Connected.any
            if at(value, "connected") != .none {
                t.connected = if at_flag(value, "connected") {
                    Connected.connected
                } else { Connected.disconnected }
            }
        } else if word == "device_connected" {
            t.kind = TriggerKind.device_connected
            t.device_class = at_text(value, "device_class")
        } else if word == "resource_changed" {
            t.kind = TriggerKind.resource_changed
            t.record_kind = at_text(value, "kind")
            t.actions = at_texts(value, "actions")
            t.only_fields = at_texts(value, "fields")
            t.actions_text = t.actions.join(", ")
            t.fields_text = t.only_fields.join(", ")
            t.by = by_of(at_text(value, "by"))
        } else if word == "app_event" {
            t.kind = TriggerKind.app_event
            t.app_id = at_text(value, "app_id")
            t.event_kind = at_text(value, "kind")
        } else if word == "incoming_message" {
            t.kind = TriggerKind.incoming_message
            t.app_id = at_text(value, "app_id")
            t.from = at_text(value, "from")
        } else if word == "nearby_device" {
            t.kind = TriggerKind.nearby_device
            t.device = at_text(value, "device")
        } else if word == "resource_time" {
            t.kind = TriggerKind.resource_time
            t.record_kind = at_text(value, "kind")
            t.time_field = at_text(value, "field")
            t.offset_secs = at_int(value, "offset_secs").to_float()
            t.filter = piece_text(at(value, "filter"), "")
        } else if word == "source_changed" {
            t.kind = TriggerKind.source_changed
            t.record_kind = at_text(value, "kind")
            t.source_at = at_text(value, "at")
            t.every_secs = at_int(value, "every_secs").to_float()
            t.filter = piece_text(at(value, "filter"), "")
        }
        p.triggers.insert(t)
    }
}

// ---------------------------------------------------------------------------
// The draft, in words
// ---------------------------------------------------------------------------
fn indent(depth: Int) -> Text {
    if depth <= 0 { "" } else { "  " + indent(depth - 1) }
}

/// The step `id` names, by what the editor numbers it.
fn number_of_id(p: Program, id: Text) -> Int {
    if let s = reading(p).find(one -> one.step_id == id) { number_of(p, s) } else { 0 }
}

/// `text` read as JSON, when it is.
fn parsed(text: Text) -> Option<json.Value> {
    match json.parse(text) {
        .ok(value) => .some(value),
        .err(why) => .none,
    }
}

/// A value as a person reads it: an input, a variable, a step's result, a typed value, or the
/// object or list it builds.
fn describe_value(p: Program, v: BindValue) -> Text {
    if v.kind == .input {
        "input {v.name}"
    } else if v.kind == .variable {
        "variable {v.name}"
    } else if v.kind == .step {
        "the result of step {number_of_id(p, v.name)}"
    } else if v.kind == .template {
        "“{v.name}”"
    } else if v.kind == .object {
        "an object"
    } else if v.kind == .list { "a list" } else { literal_words(v.literal, v.ty) }
}

/// A condition as a person reads it.
fn condition_words(p: Program, c: Condition) -> Text {
    if c.kind == .equals {
        "{describe_value(p, c.left)} is {describe_value(p, c.right)}"
    } else if c.kind == .negate {
        "not ({condition_words(p, c.inner.first() ?? Condition())})"
    } else if c.kind == .all {
        "(" + c.inner.map(one -> condition_words(p, one)).join(" and ") + ")"
    } else if c.kind == .any {
        "(" + c.inner.map(one -> condition_words(p, one)).join(" or ") + ")"
    } else { "{describe_value(p, c.left)} is set" }
}

/// The value a step's block reads, as a person reads it; nothing when it is not set.
fn slot_words(p: Program, s: Step, name: Text) -> Text {
    if let slot = s.slots.find(one -> one.name == name) {
        if slot.pick == .none { "Not set" } else { describe_value(p, slot.value) }
    } else { "Not set" }
}

/// A binding as a person reads it: an input, a variable, a step's result or a typed value.
fn describe_binding(p: Program, text: Text) -> Text {
    let held = text.trim()
    if held == "" {
        return "nothing"
    }
    if let value = parsed(held) {
        if let name = at(value, "input") {
            return "input " + text_of(name)
        }
        if let name = at(value, "var") {
            return "variable " + text_of(name)
        }
        if let out = at(value, "step_output") {
            let number = number_of_id(p, at_text(out, "step"))
            return "the result of step {number}"
        }
        if let one = at(value, "literal") {
            return literal_words(literal_text(one), json_words(one))
        }
        if let words = at(value, "template") {
            return "“" + text_of(words) + "”"
        }
        if at(value, "object") != .none {
            return "an object"
        }
        if at(value, "list") != .none {
            return "a list"
        }
    }
    return held
}

fn literal_words(text: Text, ty: Text) -> Text {
    if ty == "text" {
        "“{text}”"
    } else if ty == "object" {
        "an object"
    } else if ty == "list" { "a list" } else if ty == "none" { "nothing" } else { text }
}

/// What a step does, in one line.
fn step_words(p: Program, s: Step) -> Text {
    if s.head == .branch {
        "If {condition_words(p, s.condition)}"
    } else if s.head == .each {
        "For each {s.variable} in {slot_words(p, s, "items")}"
    } else if s.head == .set {
        "Set {s.name} to {slot_words(p, s, "value")}"
    } else if s.head == .ask {
        "Ask for {s.ask_description}"
    } else if s.head == .ai {
        "Ask the agent: {s.prompt}"
    } else if s.head == .task {
        "Let the agent: {s.instruction}"
    } else if s.head == .read {
        "Read {s.read_record} of {s.read_app}"
    } else {
        let label = if s.action_label != "" { s.action_label } else { s.action_name }
        let target = if s.resource_label != "" { s.resource_label } else { s.target }
        "Call “{label}” on {target}"
    }
}

/// How a step's arguments read beside it.
fn arguments_words(p: Program, s: Step) -> Text {
    let given = s.args.filter(one -> one.pick != Pick.none)
    if given.is_empty() {
        return ""
    }
    " with " + given.map(one -> one.name + ": " + describe_value(p, one.value)).join(", ")
}

/// The draft's program as text: the same steps, in the same order, each with where its values
/// come from.
/// The program's fields as the engine's JSON: its steps, inputs, result, triggers and grant.
fn program_fields(p: Program) -> Text {
    let out = "\"steps\":" + block_json(p, .none, .top) + ",\"grant\":" + p.grant
    if not p.inputs.is_empty() {
        out = out + ",\"inputs\":" + inputs_json(p)
    }
    if p.output.kind != .none {
        out = out + ",\"output\":" + value_json(p.output)
    }
    if p.step_timeout_secs > 0 {
        let secs = p.step_timeout_secs
        out = out + ",\"step_timeout_secs\":{secs}"
    }
    if not p.triggers.is_empty() {
        out = out + ",\"triggers\":" + triggers_json(p)
    }
    out
}

/// The program as the engine's JSON.
fn program_json(p: Program) -> Text {
    brace_open() + program_fields(p) + brace_close()
}

/// A saved shortcut in the engine's words: its name, id and program, as importing one reads it.
fn saved_json(s: Saved) -> Text {
    brace_open()
        + "\"id\":"
        + js(s.id)
        + ",\"name\":"
        + js(s.name)
        + ","
        + program_fields(s.program)
        + brace_close()
}

fn program_text(p: Program) -> Text {
    text_block(p, .none, .top, 0, "")
}

fn text_block(p: Program, parent: Option<Step>, branch: Branch, depth: Int, title: Text) -> Text {
    let out = if title == "" { "" } else { indent(depth) + title + ":\n" }
    let lines: List<Text> = []
    for s in siblings(p, parent, branch) {
        let number = number_of(p, s)
        lines = [lines, [indent(depth) + "{number}. " + step_words(p, s) + arguments_words(p, s)]]
            .flat_map(one -> one)
        if s.head == .branch {
            lines = [lines, [text_block(p, .some(s), .then, depth + 1, "Then")]]
                .flat_map(one -> one)
            if not siblings(p, .some(s), .otherwise).is_empty() {
                lines = [lines, [text_block(p, .some(s), .otherwise, depth + 1, "Otherwise")]]
                    .flat_map(one -> one)
            }
        } else if s.head == .each {
            lines = [lines, [text_block(p, .some(s), .each, depth + 1, "For each")]]
                .flat_map(one -> one)
        }
    }
    out + lines.join("\n")
}

fn offer_of(a: actions.Offered) -> Offer {
    Offer(
        app_id: a.app,
        app_label: a.app_label,
        label: a.label,
        resource: a.resource,
        resource_kind: a.resource_kind,
        resource_id: a.resource_id,
        address: a.address,
        reads: a.kind == .read,
    )
}

/// The parameters an offer names, read into the ones a step keeps.
fn params_of(a: actions.Offered) -> List<Param> {
    a.params.map(p -> Param(name: p.name, ty: p.ty, required: p.required))
}

/// The values a step's block may read at its place: this shortcut's inputs, the variables set
/// before it, and the results of the steps before it.
fn values_before(p: Program, s: Step) -> List<Choice> {
    let found: List<Choice> = []
    for input in p.inputs {
        found = [found, [Choice(kind: .input, name: input.name, describes: input_label(input.ty))]]
            .flat_map(one -> one)
    }
    for earlier in reading(p) {
        if earlier == s {
            return found
        }
        if earlier.head == .set {
            found = [found, [Choice(kind: .variable, name: earlier.name, describes: "a variable")]]
                .flat_map(one -> one)
        }
        if earlier.head == .each {
            found = [
                found,
                [Choice(kind: .variable, name: earlier.variable, describes: "the repeated item")],
            ]
                .flat_map(one -> one)
        }
        if earlier.step_id != "" {
            found = [
                found,
                [
                    Choice(
                        kind: .step,
                        name: earlier.step_id,
                        describes: "the result of step {number_of(p, earlier)}",
                    ),
                ],
            ]
                .flat_map(one -> one)
        }
    }
    found
}

/// "New shortcut", numbered past the drafts that already go by it.
// ---------------------------------------------------------------------------
// A draft's name, and the drafts on the workbench
// ---------------------------------------------------------------------------
/// A key a draft has of its own: every draft's differs, so saving one never replaces another.
fn fresh_shortcut_id(d: Draft) -> Text {
    "s" + d.id()
}

fn unused_name(w: Workbench, name: Text) -> Text {
    let base = name.trim()
    let numbered = w.drafts
        .map(d -> d.name)
        .filter(one -> one == base or one.starts_with(base + " "))
    if numbered.is_empty() {
        return name
    }
    free_draft_name(w, base, 2)
}

fn free_draft_name(w: Workbench, base: Text, number: Int) -> Text {
    let candidate = if number <= 1 { base } else { "{base} {number}" }
    if w.drafts.any(d -> d.name == candidate) {
        free_draft_name(w, base, number + 1)
    } else { candidate }
}

/// Put the open draft away: the overview stands again, and the window names no draft.
action close_draft(w: Workbench) (icon: .x, label: "Close the draft") {
    w.opened = ""
}

primary action new_draft(w: Workbench, name: Text = "New shortcut") (
    icon: .plus,
    label: "New shortcut",
) {
    let draft = w.drafts.insert(Draft(name: unused_name(w, name)))
    draft.shortcut_id = fresh_shortcut_id(draft)
    w.opened = draft.shortcut_id
    w.notice = ""
    w.failure = ""
}

action rename_draft(w: Workbench, d: Draft, name: Text) (icon: .pencil, label: "Rename") {
    if name.trim() == "" {
        w.failure = "Name the shortcut"
        return
    }
    d.name = name.trim()
    review(d)
}

action duplicate_draft(w: Workbench, d: Draft) (icon: .copy, label: "Duplicate draft") {
    let copy = w.drafts.insert(Draft(name: unused_name(w, d.name + " copy")))
    copy.shortcut_id = fresh_shortcut_id(copy)
    for input in d.program.inputs {
        copy.program.inputs.insert(
            Input(
                name: input.name,
                ty: input.ty,
                required: input.required,
                description: input.description,
            ),
        )
    }
    copy_block(d, copy, .none, .top)
    for trigger in d.program.triggers {
        let made = copy.program.triggers.insert(Trigger())
        apply_trigger(made, trigger)
    }
    copy_value(copy.program.output, d.program.output)
    copy.program.step_timeout_secs = d.program.step_timeout_secs
    copy.program.grant = d.program.grant
    review(copy)
}

/// Copies the steps of one block, and every block inside them, into another draft.
action copy_block(from: Draft, to: Draft, parent: Option<Step>, branch: Branch) {
    for s in siblings(from.program, parent, branch) {
        let made = to.program.steps.insert(
            Step(
                parent: parent,
                branch: branch,
                step_id: s.step_id,
                head: s.head,
                app_id: s.app_id,
                action_kind: s.action_kind,
                action_name: s.action_name,
                action_label: s.action_label,
                resource_label: s.resource_label,
                selector: s.selector,
                target: s.target,
                variable: s.variable,
                name: s.name,
                ask_name: s.ask_name,
                ask_type: s.ask_type,
                ask_description: s.ask_description,
                prompt: s.prompt,
                ai_output: s.ai_output,
                instruction: s.instruction,
                allow: s.allow,
                task_output: s.task_output,
                read_app: s.read_app,
                read_record: s.read_record,
                read_joined: s.read_joined,
                policy: s.policy,
                attempts: s.attempts,
                backoff_ms: s.backoff_ms,
            ),
        )
        copy_value(made.resource_binding, s.resource_binding)
        copy_condition(made.condition, s.condition)
        copy_value(made.default_value, s.default_value)
        for param in s.params {
            made.params.insert(
                Param(
                    name: param.name,
                    ty: param.ty,
                    required: param.required,
                    description: param.description,
                ),
            )
        }
        for arg in s.args {
            let copy = made.args.insert(Arg(name: arg.name, ty: arg.ty, pick: arg.pick))
            copy_value(copy.value, arg.value)
        }
        for slot in s.slots {
            let copy = made.slots.insert(Slot(name: slot.name, ty: slot.ty, pick: slot.pick))
            copy_value(copy.value, slot.value)
        }
        for branch in [Branch.then, Branch.otherwise, Branch.each] {
            copy_block(from, to, .some(s), branch)
        }
    }
}

action discard(w: Workbench, d: Draft) (icon: .trash, label: "Discard draft") {
    for step in d.program.steps {
        d.program.steps.remove(step)
    }
    w.drafts.remove(d)
    w.notice = ""
    w.failure = ""
}

// ---------------------------------------------------------------------------
// Shortcuts the library or the agent wrote
// ---------------------------------------------------------------------------
action draft_shortcut(w: Workbench, shortcut: Text) (icon: .download, label: "Import a shortcut") {
    if shortcut.trim().len() > 16384 {
        w.failure = "That shortcut is too large"
        return
    }
    if let value = parsed(shortcut) {
        let name = at_text(value, "name")
        if name != "" and w.drafts.any(d -> d.name.trim() == name.trim()) {
            w.failure = "A draft called “{name}” is already open"
            return
        }
        let draft = w.drafts.insert(
            Draft(name: unused_name(w, if name == "" { "Imported shortcut" } else { name })),
        )
        draft.shortcut_id = fresh_shortcut_id(draft)
        load_shortcut(draft, value, armed: false)
        match_catalogue(draft)
        review(draft)
        w.notice = "Imported “{draft.name}”"
        w.failure = ""
    } else {
        w.failure = "That is not a shortcut"
    }
}

action open_shortcut(w: Workbench, shortcut: Text) (icon: .pencil, label: "Edit a saved shortcut") {
    if let value = parsed(shortcut) {
        let draft = w.drafts.insert(Draft(name: at_text(value, "name")))
        draft.shortcut_id = fresh_shortcut_id(draft)
        load_shortcut(draft, value)
        match_catalogue(draft)
        review(draft)
        draft.replaces_saved = true
        w.notice = "Opened “{draft.name}”"
        w.failure = ""
    } else {
        w.failure = "That is not a shortcut"
    }
}

action duplicate_shortcut(w: Workbench, shortcut: Text) (
    icon: .copy,
    label: "Duplicate saved shortcut",
) {
    if let value = parsed(shortcut) {
        let saved_name = at_text(value, "name")
        let draft = w.drafts.insert(Draft())
        load_shortcut(draft, value)
        // The copy is a shortcut of its own: a key of its own, and a name that tells it apart
        // from the one it copied, whatever the saved one carried.
        draft.shortcut_id = "s" + draft.id().slice(0, 16)
        draft.name = unused_name(w, if saved_name == "" { "Copy" } else { saved_name + " copy" })
        match_catalogue(draft)
        review(draft)
        w.notice = "Copied “{draft.name}”"
        w.failure = ""
    } else {
        w.failure = "That is not a shortcut"
    }
}

/// Take every trigger off a draft, so nothing starts it until a person gives it one again.
action disarm(d: Draft) {
    let armed = d.program.triggers
    for trigger in armed {
        d.program.triggers.remove(trigger)
    }
}

/// Open a shortcut the app proposed, named after the work it noticed, as an unsaved draft.
action draft_proposal(w: Workbench, program: Text) (
    icon: .wand_sparkles,
    label: "Open proposed shortcut",
) {
    draft_shortcut(w, program)
}

action ignore_proposal(w: Workbench, key: Text) (icon: .x, label: "Ignore the proposal") {
    if key.trim() != "" { w.ignored.add(key.trim()) }
}

action allow_proposal(w: Workbench, key: Text) (label: "Offer the proposal again") {
    w.ignored.remove(key)
}

// ---------------------------------------------------------------------------
// The action picker
// ---------------------------------------------------------------------------
fn library_address() -> Text {
    "so.tilo.shortcuts/shortcuts"
}

fn inspect_address() -> Text {
    "so.tilo.shortcuts/shortcuts#inspect"
}

fn choices_address() -> Text {
    "so.tilo.shortcuts/shortcuts#choices"
}

fn list_address() -> Text {
    "so.tilo.shortcuts/shortcuts#list"
}

fn agent_app() -> Text {
    "so.tilo.agent"
}

/// The picker opens at once and the catalogue is read for it a moment later, so a person sees
/// that the actions are being read rather than a bare sheet. The read is the same wherever it
/// happens: nothing is kept once the picker closes.
action open_picker(w: Workbench, d: Draft, block: Text) (icon: .plus, label: "Add step") {
    close_picker(w)
    w.picker_for = .some(d)
    w.picker_block = block
    w.picker_loading = true
}

/// Read the actions the person is offered for the open picker: what the app drew as being read.
action read_picker(w: Workbench) {
    if not w.picker_loading {
        return
    }
    let answer = actions.catalogue(query: .none, app: .none)
    for offer in w.picker {
        w.picker.remove(offer)
    }
    for a in answer.offers {
        let offer = w.picker.insert(offer_of(a))
        for p in params_of(a) {
            offer.params.insert(p)
        }
    }
    w.picker_failure = answer.failure
    w.picker_loading = false
}

action close_picker(w: Workbench) {
    for offer in w.picker {
        w.picker.remove(offer)
    }
    w.picker_for = .none
    w.picker_block = ""
    w.picker_loading = false
    w.picker_failure = ""
}

/// Where a new step goes in a block an editor path names.
fn place_of_block(d: Draft, address: Text) -> Place {
    if address == "" {
        return Place()
    }
    let parts = address.split(".")
    let owner = parts.take(parts.len() - 1).join(".")
    let branch = branch_of(parts.last() ?? "")
    if let step = step_at(d.program, owner) {
        return Place(
            parent: .some(step),
            branch: branch,
            index: siblings(d.program, .some(step), branch).len(),
        )
    }
    return Place()
}

action add_offer(w: Workbench, d: Draft, o: Offer) {
    let place = place_of_block(d, w.picker_block)
    let step = d.program.steps.insert(Step(parent: place.parent, branch: place.branch))
    let named = o.address.split("#").last() ?? ""
    step.head = Head.invoke
    step.step_id = fresh_step_id(d.program, if named == "" { "step" } else { named })
    step.app_id = o.app_id
    step.action_kind = o.resource_kind
    step.action_name = named
    step.action_label = o.label
    step.resource_label = o.resource
    step.selector = Selector.fixed
    step.target = o.resource_id
    for param in o.params {
        step.params.insert(
            Param(
                name: param.name,
                ty: param.ty,
                required: param.required,
                description: param.description,
            ),
        )
        step.args.insert(Arg(name: param.name, ty: param.ty))
    }
    close_picker(w)
    review(d)
}

/// Add a step that is not an app's action: `if`, `for_each`, `set_var`, `ask`, `ai` or `task`.
action add_block(w: Workbench, d: Draft, head: Head) {
    let place = place_of_block(d, w.picker_block)
    let step = d.program.steps.insert(Step(parent: place.parent, branch: place.branch))
    step.head = head
    step.step_id = fresh_step_id(d.program, head_word(head))
    if head == Head.branch {
        step.slots.insert(Slot(name: "condition", ty: "bool"))
    } else if head == Head.each {
        step.slots.insert(Slot(name: "items", ty: "list"))
    } else if head == Head.set {
        step.slots.insert(Slot(name: "value", ty: "text"))
    } else if head == Head.ai {
        step.slots.insert(Slot(name: "ai_input", ty: "text"))
    }
    if head == Head.ask {
        step.ask_description = "Your answer"
    } else if head == Head.ai {
        step.prompt = "Summarise this"
    } else if head == Head.task {
        step.instruction = "Tidy the inbox"
    }
    close_picker(w)
    review(d)
}

// ---------------------------------------------------------------------------
// Editing steps
// ---------------------------------------------------------------------------
action remove_step(w: Workbench, d: Draft, s: Step) (icon: .trash, label: "Remove") {
    remove_below(d, s)
    d.program.steps.remove(s)
    review(d)
}

action remove_below(d: Draft, s: Step) {
    for branch in [Branch.then, Branch.otherwise, Branch.each] {
        for child in siblings(d.program, .some(s), branch) {
            remove_below(d, child)
        }
    }
    for branch in [Branch.then, Branch.otherwise, Branch.each] {
        for child in siblings(d.program, .some(s), branch) {
            d.program.steps.remove(child)
        }
    }
}

action move_step(w: Workbench, d: Draft, s: Step, earlier: Bool) (label: "Move") {
    let beside = siblings(d.program, s.parent, s.branch)
    let at = beside.index_of(s) ?? 0
    if earlier {
        if let other = beside.at(at - 1) { d.program.steps.move(s, before: other) }
    } else {
        if let other = beside.at(at + 1) { d.program.steps.move(s, after: other) }
    }
    review(d)
}

action nest_step(w: Workbench, d: Draft, s: Step) (
    icon: .arrow_right,
    label: "Move into the step above",
) {
    if let place = nest_place(d.program, s) {
        // Under the block's own step, in the branch it opens: it stands with that block's steps,
        // and its place among them is the collection's.
        d.program.steps.place(s, .parent, inside: place.parent)
        s.branch = place.branch
    }
    review(d)
}

action lift_step(w: Workbench, d: Draft, s: Step) (
    icon: .arrow_left,
    label: "Move out of its step",
) {
    if let holder = s.parent {
        if let place = lift_place(d.program, s) {
            // Beside the block's step, just after it and all it holds: out of its block.
            d.program.steps.place(s, .parent, after: holder)
            s.branch = place.branch
        }
    }
    review(d)
}

action set_policy(w: Workbench, d: Draft, s: Step, policy: Policy) (label: "When this step fails") {
    s.policy = policy
    review(d)
}

/// Whether a value a person set is the one a checker answer offers.
fn same_choice(value: BindValue, choice: Choice) -> Bool {
    value.kind == choice.kind
        and value.name == choice.name
        and value.pointer == choice.pointer
        and value.literal == choice.literal
}

/// Copies a value the checker offered into an argument's or a slot's own value.
action adopt_choice(into: BindValue, choice: Choice) {
    into.kind = choice.kind
    into.ty = choice.ty
    into.name = choice.name
    into.pointer = choice.pointer
    into.literal = choice.literal
    for member in into.members {
        into.members.remove(member)
    }
}

/// Copies `from` into `into`, with what an object or a list holds.
action copy_value(into: BindValue, from: BindValue) {
    into.kind = from.kind
    into.ty = from.ty
    into.name = from.name
    into.pointer = from.pointer
    into.literal = from.literal
    for member in into.members {
        into.members.remove(member)
    }
    for member in from.members {
        let made = into.members.insert(Member(at: member.at))
        copy_value(made.value, member.value)
    }
}

/// Copies a condition into `into`, with the conditions it joins.
action copy_condition(into: Condition, from: Condition) {
    into.kind = from.kind
    copy_value(into.left, from.left)
    copy_value(into.right, from.right)
    for inner in into.inner {
        into.inner.remove(inner)
    }
    for inner in from.inner {
        let made = into.inner.insert(Condition())
        copy_condition(made, inner)
    }
}

action set_output(w: Workbench, d: Draft, choice: Choice) (label: "Result") {
    adopt_choice(d.program.output, choice)
    review(d)
}

action clear_output(w: Workbench, d: Draft) {
    d.program.output.kind = .none
    review(d)
}

/// Where one value a step reads comes from.
action set_slot(w: Workbench, d: Draft, s: Step, name: Text, pick: Pick, choice: Choice) {
    if let slot = s.slots.find(one -> one.name == name) {
        slot.pick = pick
        adopt_choice(slot.value, choice)
    } else {
        let made = s.slots.insert(Slot(name: name, pick: pick))
        adopt_choice(made.value, choice)
    }
    review(d)
}

action type_slot(w: Workbench, d: Draft, s: Step, name: Text) {
    if let slot = s.slots.find(one -> one.name == name) {
        slot.pick = .literal
        slot.value.kind = .literal
    } else {
        let made = s.slots.insert(Slot(name: name, pick: .literal))
        made.value.kind = .literal
    }
    review(d)
}

action set_argument(w: Workbench, d: Draft, s: Step, a: Arg, pick: Pick, choice: Choice) {
    a.pick = pick
    adopt_choice(a.value, choice)
    review(d)
}

action clear_argument(w: Workbench, d: Draft, s: Step, a: Arg) {
    a.pick = .none
    a.value.kind = .none
    review(d)
}

action clear_slot(w: Workbench, d: Draft, s: Step, name: Text) {
    if let slot = s.slots.find(one -> one.name == name) {
        slot.pick = .none
        slot.value.kind = .none
    }
    review(d)
}

action type_argument(w: Workbench, d: Draft, s: Step, a: Arg) {
    a.pick = .literal
    a.value.kind = .literal
    review(d)
}

action set_condition_typed(w: Workbench, d: Draft, s: Step, text: Text) {
    s.condition.kind = .truthy
    s.condition.left.kind = .literal
    s.condition.left.ty = "text"
    s.condition.left.literal = text
    review(d)
}

// ---------------------------------------------------------------------------
// Inputs and triggers
// ---------------------------------------------------------------------------
action add_input(w: Workbench, d: Draft, name: Text, ty: InputType) (
    icon: .plus,
    label: "Add input",
) {
    let held = name.trim()
    if held == "" {
        w.failure = "Name the input"
        return
    }
    if d.program.inputs.any(one -> one.name == held) {
        w.failure = "There is already an input “{held}”"
        return
    }
    d.program.inputs.insert(Input(name: held, ty: ty, description: held))
    review(d)
}

action remove_input(w: Workbench, d: Draft, i: Input) (icon: .x, label: "Remove") {
    d.program.inputs.remove(i)
    review(d)
}

/// What a trigger needs to be able to arm, in words; empty when it can.
fn cannot_arm(t: Trigger) -> Text {
    if t.kind == .keyboard and t.chord.trim() == "" {
        return "A keyboard trigger needs a key chord"
    }
    if t.kind == .schedule_day
        and (t.hour > 23.0 or t.minute > 59.0 or t.hour < 0.0 or t.minute < 0.0) {
        return "The time of day must be a real one"
    }
    if t.kind == .schedule_every and t.period_secs <= 0.0 {
        return "A repeating trigger needs a period"
    }
    if t.kind == .once and t.once_ms <= 0 {
        return "A one-moment trigger needs a date and time"
    }
    if t.kind == .app_opened and t.app_id.trim() == "" {
        return "An app-opened trigger needs an app"
    }
    if t.kind == .file_changed and t.folder.trim() == "" {
        return "A file-changed trigger needs a folder"
    }
    if t.kind == .resource_changed and t.record_kind.trim() == "" {
        return "A record-changed trigger needs what sort of record"
    }
    if (t.kind == .resource_time or t.kind == .source_changed) and t.record_kind.trim() == "" {
        return "A trigger on a source needs what sort of source"
    }
    if t.kind == .resource_time and t.time_field.trim() == "" {
        return "A trigger on a time needs the field holding it"
    }
    if t.kind == .app_event and (t.app_id.trim() == "" or t.event_kind.trim() == "") {
        return "An app-event trigger needs an app and what sort of event"
    }
    if t.kind == .device_connected and t.device_class.trim() == "" {
        return "A device trigger needs a kind of device"
    }
    return ""
}

action add_trigger(w: Workbench, d: Draft, t: Trigger) uses time (
    icon: .plus,
    label: "Add trigger",
) {
    let refused = cannot_arm(t)
    if refused != "" {
        w.failure = refused
        return
    }
    d.program.triggers.insert(t)
    w.failure = ""
    review(d)
}

/// Add what the form holds to the open draft, or replace the trigger the form is editing.
action apply_form(w: Workbench, d: Draft) uses time (icon: .check, label: "Add trigger") {
    let made = w.form
    made.actions = commas(made.actions_text)
    made.only_fields = commas(made.fields_text)
    if made.kind == .once {
        made.once_ms = moment_ms(made.once_text, now)
    }
    let refused = cannot_arm(made)
    if refused != "" {
        w.failure = refused
        return
    }
    let target = w.editing
    if let target = target {
        if d.program.triggers.any(one -> one == target) {
            apply_trigger(target, made)
        } else {
            let fresh = d.program.triggers.insert(Trigger())
            apply_trigger(fresh, made)
        }
    } else {
        let fresh = d.program.triggers.insert(Trigger())
        apply_trigger(fresh, made)
    }
    w.editing = .none
    apply_trigger(w.form, .none)
    w.failure = ""
    review(d)
}

/// Load a trigger the draft carries into the form, to change it.
action edit_trigger(w: Workbench, t: Trigger) (icon: .pencil, label: "Edit the trigger") {
    apply_trigger(w.form, t)
    w.editing = .some(t)
}

action cancel_edit(w: Workbench) (icon: .x, label: "Cancel") {
    w.editing = .none
    apply_trigger(w.form, .none)
}

action remove_trigger(w: Workbench, d: Draft, t: Trigger) (icon: .trash, label: "Remove") {
    d.program.triggers.remove(t)
    review(d)
}

/// Copies every field of one trigger onto another, or the type's own defaults onto it when none is
/// named: a trigger built here is a value no action takes, so the action builds what it copies.
action apply_trigger(to: Trigger, from: Option<Trigger>) {
    let source = from ?? Trigger()
    to.kind = source.kind
    to.chord = source.chord
    to.hour = source.hour
    to.minute = source.minute
    to.weekdays = source.weekdays
    to.catch_up = source.catch_up
    to.period_secs = source.period_secs
    to.once_ms = source.once_ms
    to.once_text = source.once_text
    to.app_id = source.app_id
    to.workspace = source.workspace
    to.folder = source.folder
    to.focus_mode = source.focus_mode
    to.focus_transition = source.focus_transition
    to.network = source.network
    to.connected = source.connected
    to.device_class = source.device_class
    to.record_kind = source.record_kind
    to.actions = source.actions
    to.only_fields = source.only_fields
    to.actions_text = source.actions_text
    to.fields_text = source.fields_text
    to.by = source.by
    to.event_kind = source.event_kind
    to.from = source.from
    to.device = source.device
    to.time_field = source.time_field
    to.offset_secs = source.offset_secs
    to.filter = source.filter
    to.source_at = source.source_at
    to.every_secs = source.every_secs
}

/// Check the draft, and what each of its steps' arguments may be bound to, so the editor shows a
/// person what saving would accept without the view reading anything itself.
// ---------------------------------------------------------------------------
// Checking a draft: what saving it would allow, and what the checker refuses
// ---------------------------------------------------------------------------
action review(d: Draft) {
    let shortcut = shortcut_json(d, "[]")
    for problem in d.problems {
        d.problems.remove(problem)
    }
    for grant in d.grants {
        d.grants.remove(grant)
    }
    d.program.grant = "[]"
    let arguments = brace_open() + "\"shortcut\":" + shortcut + brace_close()
    if let value = parsed(arguments) {
        let answer = actions.call(address: inspect_address(), arguments: value)
        if answer.status == .ended {
            if let out = answer.output {
                if let held = parsed(written(out)) {
                    d.program.grant = piece_text(at(held, "grant"), "[]")
                    for problem in at_list(held, "problems") {
                        d.problems.insert(
                            Problem(
                                path: at_text(problem, "path"),
                                message: at_text(problem, "message"),
                            ),
                        )
                    }
                    for grant in at_list(held, "permissions") {
                        d.grants.insert(
                            Grant(label: at_text(grant, "label"), kind: at_text(grant, "kind")),
                        )
                    }
                }
            }
        } else {
            d.problems.insert(Problem(message: "The checker could not read the draft"))
        }
    }
    for step in d.program.steps.filter(one -> one.head == .invoke) {
        let path = path_of(d.program, step)
        for argument in step.args {
            for choice in argument.choices {
                argument.choices.remove(choice)
            }
            if let param = step.params.find(one -> one.name == argument.name) {
                argument.ty = param.ty
            }
            let asked = brace_open()
                + "\"shortcut\":"
                + shortcut
                + ",\"path\":"
                + js(path)
                + ",\"param\":"
                + js(argument.name)
                + brace_close()
            if let value = parsed(asked) {
                let answer = actions.call(address: choices_address(), arguments: value)
                if answer.status == .ended {
                    if let out = answer.output {
                        if let held = parsed(written(out)) {
                            for one in entries_of(held) {
                                let choice = argument.choices.insert(
                                    Choice(describes: at_text(one, "describes")),
                                )
                                if let binding = at(one, "binding") { load_choice(choice, binding) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Keeps every action step's parameters and the type of each argument what the catalogue says
/// they are, reading the catalogue once.
action match_catalogue(d: Draft) {
    let found = actions.catalogue(query: .none, app: .none).offers
    for step in d.program.steps.filter(one -> one.head == .invoke) {
        for offered in found {
            let named = offered.address.split("#").last() ?? ""
            if offered.app == step.app_id
                and named == step.action_name
                and (step.action_kind == "" or offered.resource_kind == step.action_kind) {
                apply_offer(step, offered)
            }
        }
    }
}

/// What an offered action says about a step that calls it: its name, its resource, and what every
/// parameter takes.
action apply_offer(step: Step, offered: actions.Offered) {
    step.action_label = offered.label
    step.resource_label = offered.resource
    if step.action_kind == "" {
        step.action_kind = offered.resource_kind
    }
    if step.target == "" {
        step.target = offered.resource_id
    }
    for param in step.params {
        step.params.remove(param)
    }
    for param in offered.params {
        step.params.insert(Param(name: param.name, ty: param.ty, required: param.required))
        if let existing = step.args.find(one -> one.name == param.name) {
            existing.ty = param.ty
        } else {
            step.args.insert(Arg(name: param.name, ty: param.ty))
        }
    }
}

// ---------------------------------------------------------------------------
// The library: saving, running and the runs it keeps
// ---------------------------------------------------------------------------
action refresh(w: Workbench) (icon: .refresh, label: "Reload shortcuts") {
    for saved in w.saved {
        w.saved.remove(saved)
    }
    let arguments = brace_open() + brace_close()
    if let value = parsed(arguments) {
        let answer = actions.call(address: list_address(), arguments: value)
        if answer.status == .ended {
            if let out = answer.output {
                for one in entries_of(out) {
                    let saved = w.saved.insert(
                        Saved(id: at_text(one, "id"), name: at_text(one, "name")),
                    )
                    load_saved(saved, one)
                }
            }
            w.failure = ""
        } else {
            w.failure = "Saved shortcuts are not available here"
        }
    }
}

/// Save the draft with the least grant saving would store, after the same check the editor shows.
action save(w: Workbench, d: Draft) (icon: .check, label: "Save shortcut") {
    if d.program.steps.is_empty() {
        w.failure = "Add a step first: an empty shortcut does nothing"
        return
    }
    review(d)
    if not d.problems.is_empty() {
        w.failure = "Fix what the checker found before saving"
        return
    }
    let shortcut = shortcut_json(d, d.program.grant)
    let verb = if w.saved.any(one -> one.id == d.shortcut_id) { "update" } else { "create" }
    let arguments = brace_open() + "\"shortcut\":" + shortcut + brace_close()
    if let value = parsed(arguments) {
        let answer = actions.call(address: library_address() + "#" + verb, arguments: value)
        if answer.status == .ended {
            d.replaces_saved = true
            w.notice = "Saved “{d.name}”"
            w.failure = ""
            refresh(w)
        } else {
            w.failure = if let why = answer.failure { why } else { "Saving was refused" }
        }
    }
}

/// Run a saved shortcut with what its inputs hold.
action run(w: Workbench, s: Saved) (icon: .play, label: "Run") {
    let given = s.program.inputs.map(one -> js(one.name) + ":" + input_value(one)).join(",")
    let arguments = brace_open() + given + brace_close()
    if let value = parsed(arguments) {
        let answer = actions.call(
            address: "so.tilo.shortcuts/shortcuts.shortcut:{s.id}#run",
            arguments: value,
        )
        let called = answer.id
        if called != "" {
            s.last_call = called
        }
        s.last_started = answer.started_ms
        if answer.status == .failed {
            w.failure = if let why = answer.failure { why } else { "“{s.name}” could not run" }
        } else {
            w.notice = "Running “{s.name}”"
            w.failure = ""
        }
    }
}

action delete_saved(w: Workbench, s: Saved) (icon: .trash, label: "Delete") {
    let arguments = brace_open() + "\"id\":" + js(s.id) + brace_close()
    if let value = parsed(arguments) {
        let answer = actions.call(address: library_address() + "#delete", arguments: value)
        if answer.status == .ended {
            w.saved.remove(s)
            w.notice = "Deleted “{s.name}”"
            w.failure = ""
            refresh(w)
        } else {
            w.failure = "“{s.name}” could not be deleted"
        }
    }
}

action pause_saved(w: Workbench, s: Saved) (icon: .pause, label: "Pause") {
    let arguments = brace_open() + "\"id\":" + js(s.id) + brace_close()
    if let value = parsed(arguments) {
        let answer = actions.call(address: library_address() + "#pause", arguments: value)
        if answer.status == .ended {
            s.paused = true
            w.notice = "“{s.name}” waits until you resume it"
        } else {
            w.failure = "“{s.name}” could not be paused"
        }
    }
}

action resume_saved(w: Workbench, s: Saved) (icon: .play, label: "Resume") {
    let arguments = brace_open() + "\"id\":" + js(s.id) + brace_close()
    if let value = parsed(arguments) {
        let answer = actions.call(address: library_address() + "#resume", arguments: value)
        if answer.status == .ended {
            s.paused = false
            w.notice = "“{s.name}” starts on its own again"
        } else {
            w.failure = "“{s.name}” could not be resumed"
        }
    }
}

action stop_saved(w: Workbench, s: Saved) (icon: .square, label: "Stop the run") {
    let arguments = brace_open() + "\"id\":" + js(s.id) + brace_close()
    if let value = parsed(arguments) {
        let answer = actions.call(address: library_address() + "#stop", arguments: value)
        if answer.status == .ended {
            w.notice = "“{s.name}” was stopped; what it did is kept"
        } else {
            w.failure = "“{s.name}” could not be stopped"
        }
    }
}

action stop_all(w: Workbench) (icon: .square, label: "Stop every run") {
    let arguments = brace_open() + brace_close()
    if let value = parsed(arguments) {
        let answer = actions.call(address: library_address() + "#stop-all", arguments: value)
        if answer.status == .ended {
            w.notice = "Every running shortcut was stopped"
        } else {
            w.failure = "The runs could not be stopped"
        }
    }
}

// ---------------------------------------------------------------------------
// Drafting with the agent
// ---------------------------------------------------------------------------
fn instructions() -> Text {
    "Write a Tilo shortcut that does what the person describes, using only the actions listed in the input (one line each). Answer with the shortcut as JSON: \{\"name\", \"inputs\"? , \"steps\", \"output\"?\}. Each step has a unique \"id\" (letters, digits, - or _) and a \"step\" kind: \"invoke\" with \"action\" (the listed action's \{app, kind, action\}), \"resource\": \{\"fixed\": resource id\} and \"args\" mapping parameter names to bindings; \"if\" with \"cond\": \{\"truthy\": binding\} and \"then\" and \"else\" step lists; \"for_each\" with \"items\", \"item\" and \"body\"; \"set_var\" with \"name\" and \"value\"; \"ask\" with \"param\"; \"ai\" with \"prompt\"; \"task\" with \"instruction\". A binding is \{\"literal\": value\}, \{\"input\": name\}, \{\"var\": name\} or \{\"step_output\": \{\"step\": id\}\}. Bind every required parameter. Do not invent actions. The person asked for:\n"
}

/// The shape asked of the agent: a shortcut's name, inputs, steps and result.
fn schema_json() -> Text {
    brace_open()
        + "\"type\":\"object\",\"required\":[\"name\",\"steps\"],\"properties\":"
        + brace_open()
        + "\"name\":"
        + brace_open()
        + "\"type\":\"string\",\"minLength\":1"
        + brace_close()
        + ",\"inputs\":"
        + brace_open()
        + "\"type\":\"array\",\"items\":"
        + brace_open()
        + "\"type\":\"object\""
        + brace_close()
        + brace_close()
        + ",\"steps\":"
        + brace_open()
        + "\"type\":\"array\",\"items\":"
        + brace_open()
        + "\"type\":\"object\",\"required\":[\"id\",\"step\"]"
        + brace_close()
        + brace_close()
        + ",\"output\":"
        + brace_open()
        + "\"type\":\"object\""
        + brace_close()
        + brace_close()
        + brace_close()
}

/// One action the agent may use, as a line of its input: where it is, what it takes and what it
/// is called.
fn catalogue_line(a: actions.Offered) -> Text {
    let params = a.params.map(one -> one.name + ": " + one.ty).join(", ")
    js(a.address)
        + " calls "
        + js(a.label)
        + " on "
        + js(a.resource)
        + " of "
        + js(a.app)
        + " ("
        + params
        + ")"
}

/// The agent's action that drafts, when this workspace is offered it; empty when it is not.
action agent_address() -> Text {
    let transforms = actions.catalogue(query: .none, app: .some(agent_app())).offers
    if let freeform = transforms.find(one -> one.address.ends_with("#freeform")) {
        return freeform.address
    }
    return ""
}

/// The actions a draft may use, one line each, as the agent is told about them.
action catalogue_summary() -> Text {
    actions
        .catalogue(query: .none, app: .none)
        .offers
        .filter(one -> one.app != "so.tilo.shortcuts" and one.app != agent_app())
        .map(one -> catalogue_line(one))
        .join("\n")
}

/// Ask the agent to draft a shortcut doing what the description says.
action describe(w: Workbench, description: Text) (icon: .wand_sparkles, label: "Draft it") {
    let asked = description.trim()
    if asked == "" {
        w.failure = "Describe what the shortcut should do"
        return
    }
    let address = agent_address()
    if address != "" {
        let arguments = brace_open()
            + "\"prompt\":"
            + js(instructions() + asked)
            + ",\"input\":"
            + js(catalogue_summary())
            + ",\"schema\":"
            + schema_json()
            + brace_close()
        if let value = parsed(arguments) {
            w.notice = "Asking the agent to draft it…"
            let answer = actions.call(address: address, arguments: value)
            if answer.status == .ended {
                if let out = answer.output {
                    let no_changes: List<Text> = []
                    adopt(w, written(out), no_changes)
                } else {
                    w.failure = "The agent answered nothing"
                }
            } else {
                w.failure = "The agent could not draft it here"
            }
        }
    } else {
        w.failure = "The agent does not offer drafting in this workspace"
    }
}

/// Ask the agent what the shortcut should do differently.
action refine(w: Workbench, s: Saved, comment: Text) (icon: .wand_sparkles, label: "Refine it") {
    let asked = comment.trim()
    if asked == "" {
        w.failure = "Say what the shortcut should do differently"
        return
    }
    let address = agent_address()
    if address != "" {
        let before = program_json(s.program)
        let arguments = brace_open()
            + "\"prompt\":"
            + js(
                instructions()
                    + "What this shortcut does now:\n"
                    + program_text(s.program)
                    + "\nThe person says:\n"
                    + asked,
            )
            + ",\"input\":"
            + js(catalogue_summary())
            + ",\"schema\":"
            + schema_json()
            + brace_close()
        if let value = parsed(arguments) {
            w.notice = "Asking the agent to refine “{s.name}”…"
            let answer = actions.call(address: address, arguments: value)
            if answer.status == .ended {
                if let out = answer.output {
                    adopt(w, written(out), program_changes(before, written(out)))
                } else {
                    w.failure = "The agent answered nothing"
                }
            } else {
                w.failure = "The agent could not refine it here"
            }
        }
    } else {
        w.failure = "The agent does not offer drafting in this workspace"
    }
}

/// The steps that differ between two shortcuts, as the lines a person reviews.
fn program_changes(before: Text, after: Text) -> List<Text> {
    let changes: List<Text> = []
    if let older = parsed(before) {
        if let newer = parsed(after) {
            let was = at_list(older, "steps")
            let now = at_list(newer, "steps")
            let shared = was.len().min(now.len())
            let at = 0
            for step in was {
                if at < shared {
                    if let changed = now.at(at) {
                        if written(step) != written(changed) {
                            changes = [changes, ["Step {at + 1} changes"]].flat_map(one -> one)
                        }
                    }
                    at = at + 1
                }
            }
            if now.len() > shared {
                let added = now.len() - shared
                changes = [changes, ["{added} more steps"]].flat_map(one -> one)
            } else if was.len() > shared {
                let gone = was.len() - shared
                changes = [changes, ["{gone} fewer steps"]].flat_map(one -> one)
            }
        }
    }
    changes
}

/// Offer what the agent answered as a draft to open, with what the checker says of it.
action adopt(w: Workbench, answer: Text, changes: List<Text>) {
    w.notice = ""
    if let value = parsed(answer) {
        let name = at_text(value, "name")
        if name == "" or at(value, "steps") == .none {
            w.failure = "The agent's answer is not a shortcut"
            return
        }
        for old in w.suggested {
            w.suggested.remove(old)
        }
        let made = w.suggested.insert(Suggested(name: name))
        load_program(made.program, value)
        for change in changes {
            made.changes = [made.changes, [change]].flat_map(one -> one)
        }
        let arguments = brace_open() + "\"shortcut\":" + written(value) + brace_close()
        if let asked = parsed(arguments) {
            let answer = actions.call(address: inspect_address(), arguments: asked)
            if let out = answer.output {
                if let held = parsed(written(out)) {
                    for problem in at_list(held, "problems") {
                        made.problems = [
                            made.problems,
                            [at_text(problem, "path") + ": " + at_text(problem, "message")],
                        ]
                            .flat_map(one -> one)
                    }
                }
            }
        }
        w.failure = ""
    } else {
        w.failure = "The agent's answer is not a shortcut"
    }
}

action open_suggested(w: Workbench, s: Suggested) (icon: .arrow_right, label: "Open as a draft") {
    let name = s.name
    let json = program_json(s.program)
    w.suggested.remove(s)
    draft_shortcut(w, json)
    w.notice = ""
    w.failure = ""
}

action dismiss_suggested(w: Workbench, s: Suggested) (icon: .x, label: "Put it away") {
    w.suggested.remove(s)
}

// ---------------------------------------------------------------------------
// Repeated work, from the runs the workspace keeps
// ---------------------------------------------------------------------------
type Heard {
    app_id: Text = ""
    action_name: Text = ""
    kind: Text = ""
    resource: Text = ""
    label: Text = ""
}

type Repeated {
    steps: List<Heard>
    times: Int = 3
}

/// The invocations this app caused, newest first, as the actions one person took.
fn heard() -> List<Heard> {
    let found: List<Heard> = []
    for call in query(invocations.Invocation) {
        let taken = call.action ?? ""
        if taken.starts_with("so.tilo.shortcuts") == false and taken != "" {
            if let origin = call.origin {
                let parts = taken.split("#")
                let publisher = parts.first() ?? ""
                let verb = parts.last() ?? ""
                if publisher != "" and verb != "" and origin.kind != "" {
                    found = [
                        found,
                        [
                            Heard(
                                app_id: publisher,
                                action_name: verb,
                                kind: origin.kind,
                                resource: origin.id,
                                label: call.title ?? "",
                            ),
                        ],
                    ]
                        .flat_map(one -> one)
                }
            }
        }
    }
    found
}

fn same_heard(one: Heard, other: Heard) -> Bool {
    one.app_id == other.app_id and one.action_name == other.action_name and one.kind == other.kind
}

fn same_run(all: List<Heard>, first: Int, second: Int, length: Int) -> Bool {
    let same = true
    for i in [0, 1, 2, 3, 4, 5, 6, 7] {
        if i < length {
            if let one = all.at(first + i) {
                if let other = all.at(second + i) {
                    if not same_heard(one, other) {
                        same = false
                    }
                } else {
                    same = false
                }
            } else {
                same = false
            }
        }
    }
    same
}

/// The work the person found themselves repeating: a run of actions taken three times over,
/// most recent first, unless they said no to it.
fn repeated_work(w: Workbench) -> Option<Repeated> {
    let all = heard()
    for length in [8, 6, 4, 3, 2, 1] {
        if all.len() >= length * 3 {
            if same_run(all, 0, length, length) and same_run(all, 0, length * 2, length) {
                let steps = all.take(length)
                if ignored(w, key_of(steps)) == false {
                    return .some(Repeated(steps: steps))
                }
            }
        }
    }
    return .none
}

fn key_of(steps: List<Heard>) -> Text {
    steps.map(one -> one.app_id + "/" + one.kind + "/" + one.action_name).join(" > ")
}

fn ignored(w: Workbench, key: Text) -> Bool {
    w.ignored.contains(key)
}

fn proposal_evidence(r: Repeated) -> Text {
    let steps = r.steps.len()
    "You did these {steps} steps three times in a row"
}

fn proposal_name(r: Repeated) -> Text {
    let names = r.steps.map(one -> if one.label != "" { one.label } else { one.action_name })
    names.join(", then ")
}

fn proposal_program(r: Repeated) -> Text {
    let parts: List<Text> = []
    for one in r.steps {
        parts = [
            parts,
            [
                brace_open()
                    + "\"app\":"
                    + js(one.app_id)
                    + ",\"kind\":"
                    + js(one.kind)
                    + ",\"action\":"
                    + js(one.action_name)
                    + brace_close(),
            ],
        ]
            .flat_map(step -> step)
    }
    "[" + parts.join(",") + "]"
}

// ---------------------------------------------------------------------------
// What the window shows
// ---------------------------------------------------------------------------
fn draft_label(name: Text) -> Text {
    if name.trim() == "" { "Untitled shortcut" } else { name }
}

fn is_folded(folded: Text, path: Text) -> Bool {
    ",{folded},".contains(",{path},")
}

fn toggle_fold_text(folded: Text, path: Text) -> Text {
    if is_folded(folded, path) {
        ",{folded},".replace(",{path},", ",")
    } else if folded == "" { path } else { "{folded},{path}" }
}

fn branch_title(branch: Branch) -> Text {
    if branch == .then {
        "Then"
    } else if branch == .otherwise {
        "Otherwise"
    } else if branch == .each { "For each item" } else { "" }
}

/// The editor path of the block inside a step, when it opens one.
fn inside_address(d: Draft, s: Step) -> Text {
    if let branch = opened_branch(s) {
        "{path_of(d.program, s)}.{branch_word(branch)}"
    } else { path_of(d.program, s) }
}

fn arg_text(d: Draft, a: Arg) -> Text {
    if a.pick == .literal {
        if a.value.literal == "" { "Type a value" } else { "“{a.value.literal}”" }
    } else if a.pick == .choice { describe_value(d.program, a.value) } else { "Not set" }
}

fn slot_text(d: Draft, slot: Slot) -> Text {
    if slot.pick == .literal {
        if slot.value.literal == "" { "Type a value" } else { "“{slot.value.literal}”" }
    } else if slot.pick == .choice { describe_value(d.program, slot.value) } else { "Not set" }
}

/// Every value the whole shortcut may read, for what it reports when it finishes.
fn output_choices(p: Program) -> List<Choice> {
    let found: List<Choice> = []
    for input in p.inputs {
        found = [found, [Choice(kind: .input, name: input.name, describes: input_label(input.ty))]]
            .flat_map(one -> one)
    }
    for step in reading(p) {
        if step.head == .set {
            found = [found, [Choice(kind: .variable, name: step.name, describes: "a variable")]]
                .flat_map(one -> one)
        }
        if step.step_id != "" {
            found = [
                found,
                [
                    Choice(
                        kind: .step,
                        name: step.step_id,
                        describes: "the result of step {number_of(p, step)}",
                    ),
                ],
            ]
                .flat_map(one -> one)
        }
    }
    found
}

fn has_day(days: List<Text>, day: Text) -> Bool {
    days.contains(day)
}

fn when(ms: Int) -> Text {
    let at = Time("1970-01-01T00:00:00Z") + Duration(ms / 1000)
    "{at.day()}, {at.clock()}"
}

fn run_words(call: invocations.Invocation) -> Text {
    if call.status == .running {
        "Running"
    } else if call.status == .waiting {
        "Waiting for you"
    } else if call.status == .failed {
        "Stopped"
    } else if call.status == .cancelled {
        "Stopped"
    } else if call.status == .undone {
        "Taken back"
    } else if let out = call.output { written(out) } else { "Finished" }
}

/// The newest run of a saved shortcut this app started.
fn last_run(s: Saved) -> Option<invocations.Invocation> {
    if s.last_call == "" {
        return .none
    }
    return query(invocations.Invocation).find(call -> call.id == s.last_call)
}

/// The runs of shortcuts, newest first.
fn recent_runs() -> List<invocations.Invocation> {
    query(invocations.Invocation)
        .filter(call -> (call.action ?? "").starts_with("so.tilo.shortcuts"))
        .take(20)
}

fn trigger_words(t: Trigger) -> Text {
    if t.kind == .keyboard {
        "A key chord"
    } else if t.kind == .app_opened {
        "An app opens"
    } else if t.kind == .schedule_day {
        "At a time of day"
    } else if t.kind == .schedule_every {
        "Every so often"
    } else if t.kind == .once {
        "Once, at a moment"
    } else if t.kind == .workspace_joined {
        "A workspace is joined"
    } else if t.kind == .file_changed {
        "A file changes"
    } else if t.kind == .focus_mode {
        "Focus changes"
    } else if t.kind == .network_changed {
        "The network changes"
    } else if t.kind == .device_connected {
        "A device is connected"
    } else if t.kind == .resource_changed {
        "A record changes"
    } else if t.kind == .app_event {
        "An app reports an event"
    } else if t.kind == .incoming_message {
        "A message comes in"
    } else if t.kind == .nearby_device {
        "A nearby device appears"
    } else if t.kind == .resource_time { "A time a record holds comes" } else { "A source changes" }
}

fn trigger_detail(t: Trigger) -> Text {
    if t.kind == .keyboard {
        t.chord
    } else if t.kind == .app_opened {
        t.app_id
    } else if t.kind == .schedule_day {
        "{t.hour.to_int()}:{t.minute.to_int()} {weekday_words(t.weekdays)}"
    } else if t.kind == .schedule_every {
        "Every {t.period_secs.to_int()} seconds"
    } else if t.kind == .once {
        if t.once_ms > 0 { when(t.once_ms) } else { "" }
    } else if t.kind == .workspace_joined {
        t.workspace
    } else if t.kind == .file_changed {
        t.folder
    } else if t.kind == .focus_mode {
        "{focus_word(t.focus_mode)} {transition_word(t.focus_transition)}s"
    } else if t.kind == .network_changed {
        if t.connected == Connected.any {
            t.network
        } else {
            "{t.network} {connected_word(t.connected)}"
        }
    } else if t.kind == .device_connected {
        t.device_class
    } else if t.kind == .resource_changed {
        t.record_kind
    } else if t.kind == .app_event {
        "{t.app_id} {t.event_kind}"
    } else if t.kind == .incoming_message {
        "{t.app_id} {t.from}"
    } else if t.kind == .nearby_device {
        t.device
    } else if t.kind == .resource_time { "{t.record_kind} {t.time_field}" } else { t.record_kind }
}

/// The words a person typed, one per comma.
fn commas(text: Text) -> List<Text> {
    text.split(",").map(one -> one.trim()).filter(one -> one != "")
}

fn weekday_words(days: List<Text>) -> Text {
    if days.is_empty() { "every day" } else { "on " + days.join(", ") }
}

fn problem_line(problem: Problem) -> Text {
    if problem.path == "" { problem.message } else { "Step {problem.path}: {problem.message}" }
}

fn heard_words(one: Heard) -> Text {
    let named = if one.label != "" { one.label } else { one.action_name }
    "{named} on {one.resource}"
}

fn connected_word(connected: Connected) -> Text {
    if connected == Connected.connected {
        "comes up"
    } else if connected == Connected.disconnected { "goes away" } else { "" }
}

fn problem_count(d: Draft) -> Int {
    d.problems.len()
}

fn problem_at(d: Draft, path: Text) -> List<Problem> {
    d.problems.filter(one -> one.path == path)
}

action toggle_fold(w: Workbench, d: Draft, path: Text) {
    d.folded = toggle_fold_text(d.folded, path)
}

action show_sheet(w: Workbench, sheet: Text) {
    w.sheet = sheet
}

action clear_new_input(w: Workbench) {
    w.new_input_name = ""
}

fn is_editing(w: Workbench) -> Bool {
    if let one = w.editing {
        return true
    }
    return false
}

action toggle_day(w: Workbench, day: Text) {
    if w.form.weekdays.contains(day) {
        w.form.weekdays = w.form.weekdays.filter(one -> one != day)
    } else {
        w.form.weekdays = [w.form.weekdays, [day]].flat_map(one -> one)
    }
}

action dismiss_notice(w: Workbench) (icon: .x, label: "Dismiss") {
    w.notice = ""
    w.failure = ""
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------
fn nav_key(showing: Text, open_draft: Option<Draft>, saved: Option<Saved>) -> Text {
    if open_draft != .none {
        if let d = open_draft { "draft:{d.id()}" } else { "overview" }
    } else if showing == "saved" {
        if let s = saved { "saved:{s.id}" } else { "overview" }
    } else { "overview" }
}

fn is_block(s: Step) -> Bool {
    s.head == .branch or s.head == .each
}

fn block_offer_label(head: Head) -> Text {
    if head == .branch {
        "If"
    } else if head == .each {
        "Repeat with each"
    } else if head == .set {
        "Set variable"
    } else if head == .ask {
        "Ask for a value"
    } else if head == .ai { "Ask the agent once" } else { "Start a task" }
}

fn block_offer_detail(head: Head) -> Text {
    if head == .branch {
        "Do some steps only when a value is set"
    } else if head == .each {
        "Do some steps for every item in a list"
    } else if head == .set {
        "Keep a value to use in later steps"
    } else if head == .ask {
        "Ask whoever runs the shortcut for something"
    } else if head == .ai {
        "Give the agent one instruction and wait for its answer"
    } else {
        "Let the agent work on an instruction with what the shortcut may use"
    }
}

fn offer_matches(query: Text, o: Offer) -> Bool {
    let asked = query.trim().lower()
    if asked == "" {
        return true
    }
    return o.label.lower().contains(asked)
        or o.app_label.lower().contains(asked)
        or o.resource.lower().contains(asked)
        or o.address.lower().contains(asked)
}

fn offer_detail(o: Offer) -> Text {
    let needs = o.params.filter(one -> one.required).map(one -> one.name)
    if needs.is_empty() {
        "{o.app_label}"
    } else {
        "{o.app_label} needs {needs.join(" and ")}"
    }
}

fn saved_steps(p: Program) -> List<Text> {
    let lines: List<Text> = []
    let at = 0
    for step in p.steps.filter(one -> one.parent == .none) {
        at = at + 1
        let named = if step.step_id == "" { "step" } else { step.step_id }
        lines = [lines, ["{at}. {named} ({head_word(step.head)})"]].flat_map(one -> one)
    }
    lines
}

default view window for Workbench

view window(w: Workbench) {
    state showing: Text = "overview"
    state draft: Option<Draft> = .none
    state saved: Option<Saved> = .none
    let open_draft: Option<Draft> = if let chosen = draft {
        .some(chosen)
    } else if w.opened != "" {
        w.drafts.find(one -> one.shortcut_id == w.opened)
    } else if showing == "draft" { w.drafts.last() } else { .none }
    ui.NavShell(
        content_kind: .page,
        title: if let d = open_draft {
            draft_label(d.name)
        } else if showing == "saved" {
            if let s = saved { s.name } else { "Shortcuts" }
        } else { "Overview" },
        sidebar_title: "Shortcuts",
        overview_title: "Overview",
        overview_count: w.saved.len(),
    ) {
        actions: {
            // New shortcut stands once: the window's own primary action draws it in the bar,
            // and its draft opens the window because the draft it made is the one it shows.
            ui.Button("Reload shortcuts", icon: .refresh, role: .plain) {
                on press: refresh(w)
            }
        }
        sidebar: {
            ui.NavList(selected: nav_key(showing, open_draft, saved)) {
                ui.NavItem("overview", "Overview", icon: .layout_dashboard) {
                    on select: set(showing, "overview").then(set(draft, .none)).then(close_draft(w))
                }
                if not w.drafts.is_empty() {
                    ui.NavSection("Drafts") {
                        for d in w.drafts {
                            ui.NavItem("draft:{d.id()}", draft_label(d.name), icon: .pencil) {
                                on select: set(showing, "draft").then(set(draft, .some(d)))
                            }
                        }
                    }
                }
                if not w.saved.is_empty() {
                    ui.NavSection("Saved shortcuts") {
                        for s in w.saved {
                            ui.NavItem("saved:{s.id}", s.name, icon: .play) {
                                on select: set(showing, "saved")
                                    .then(set(saved, .some(s)))
                                    .then(close_draft(w))
                            }
                        }
                    }
                }
            }
        }
        content: {
            ui.Scroll {
                page {
                    stack(gap: .group) {
                        if w.failure != "" {
                            ui.Banner(
                                "That could not be done",
                                message: w.failure,
                                kind: .error,
                                action: "Dismiss",
                            ) {
                                on act: dismiss_notice(w)
                                on dismiss: dismiss_notice(w)
                            }
                        }
                        if w.notice != "" {
                            ui.Banner(w.notice, kind: .success, action: "Dismiss") {
                                on act: dismiss_notice(w)
                                on dismiss: dismiss_notice(w)
                            }
                        }
                        if let d = open_draft {
                            editor(w, d)
                        } else if showing == "saved" {
                            if let s = saved {
                                saved_page(w, s)
                            } else {
                                ui.EmptyState("Nothing open", inline: true)
                            }
                        } else {
                            overview(w)
                        }
                    }
                }
            }
        }
    }
    if let d = open_draft {
        if w.sheet == "save" {
            save_sheet(w, d)
        }
        if let picking = w.picker_for {
            if picking == d {
                picker(w, d)
            }
        }
    }
}

view overview(w: Workbench) {
    if let noticed = repeated_work(w) {
        ui.Section(title: "Repeated work") {
            text(proposal_evidence(noticed), role: .label)
            text(proposal_name(noticed), role: .subheading)
            for heard_step in noticed.steps {
                text(heard_words(heard_step))
            }
            row(gap: .related, align: .center) {
                ui.Button("Open as a draft", icon: .plus, role: .primary) {
                    on press: draft_proposal(w, proposal_program(noticed))
                }
                ui.Button("Not this", role: .plain) {
                    on press: ignore_proposal(w, key_of(noticed.steps))
                }
            }
        }
    }
    ui.Section(title: "Describe a shortcut") {
        ui.TextInput(bind: w.description, placeholder: "Tidy my inbox every morning", lines: 3)
        ui.Button("Draft it", icon: .wand_sparkles, role: .primary) {
            on press: describe(w, w.description)
        }
    }
    if let made = w.suggested.first() {
        ui.Section(title: "The agent drafted “{made.name}”") {
            for change in made.changes {
                text(change)
            }
            for problem in made.problems {
                text(problem, role: .caption, secondary: true)
            }
            row(gap: .related, align: .center) {
                ui.Button("Open as a draft", icon: .arrow_right, role: .primary) {
                    on press: open_suggested(w, made)
                }
                ui.Button("Put it away", role: .plain) {
                    on press: dismiss_suggested(w, made)
                }
            }
        }
    }
    if w.saved.is_empty() and w.drafts.is_empty() {
        ui.EmptyState(
            "No shortcuts yet",
            message: "Describe one and the agent drafts it, or press New shortcut and add a step.",
            icon: .zap,
            inline: true,
        )
    }
    let runs = recent_runs()
    if let why = unreadable(query(invocations.Invocation)) {
        ui.Section(title: "Recent invocations") {
            std.unreadable_row("Recent invocations could not be read", why)
        }
    } else if not runs.is_empty() {
        ui.Section(title: "Recent invocations") {
            for call in runs {
                ui.ListRow(
                    title: call.title ?? "Shortcut run",
                    secondary: run_words(call),
                    meta: when(call.started_ms),
                )
            }
        }
    }
}

view editor(w: Workbench, d: Draft) {
    ui.TextInput(bind: d.name, placeholder: "Name", title: true)
    text(problems_line(d), role: .caption, secondary: true)
    row(gap: .related, align: .center) {
        ui.Toggle(bind: d.as_text, label: "Show program text")
        ui.Button("Add step", icon: .plus) {
            on press: open_picker(w, d, "")
        }
        ui.Button("Review and save…", icon: .check, role: .primary) {
            on press: review(d).then(show_sheet(w, "save"))
        }
        ui.Button("Duplicate", icon: .copy, role: .plain) {
            on press: duplicate_draft(w, d)
        }
        ui.Button("Discard", role: .plain) {
            on press: discard(w, d)
        }
    }
    if d.as_text {
        ui.Section(title: "Program text") {
            text(program_text(d.program), role: .code)
            text("Switch back to blocks to change the steps.", role: .caption, secondary: true)
        }
    } else {
        triggers_section(w, d)
        inputs_section(w, d)
        ui.Section(title: "Steps") {
            if reading(d.program).is_empty() {
                text(
                    "Add the first step: an action one of your apps offers, or a block.",
                    role: .caption,
                    secondary: true,
                )
            }
            block(w, d, .none, .top, "", 0)
        }
        output_section(w, d)
        grant_section(w, d)
    }
}

fn problems_line(d: Draft) -> Text {
    if d.problems.is_empty() {
        if reading(d.program).is_empty() {
            "Add a step to save this shortcut"
        } else { "Ready to save" }
    } else if let first = d.problems.first() {
        if first.path == "" { first.message } else { "{first.message}" }
    } else { "Ready to save" }
}

fn grant_line(g: Grant) -> Text {
    if g.kind == "" { g.label } else { "{g.label} ({g.kind})" }
}

view grant_section(w: Workbench, d: Draft) {
    ui.Section(title: "What saving would allow") {
        if d.grants.is_empty() {
            text(
                "It uses no app actions, so it can't read or change anything in your apps.",
                role: .caption,
                secondary: true,
            )
        }
        for g in d.grants {
            text(grant_line(g))
        }
        if not d.problems.is_empty() {
            for problem in d.problems {
                text(problem_line(problem), role: .caption, secondary: true)
            }
        }
    }
}

view inputs_section(w: Workbench, d: Draft) {
    ui.Section(title: "Inputs") {
        for i in d.program.inputs {
            row(gap: .related, align: .center) {
                ui.TextInput(bind: i.name, placeholder: "Name")
                ui.EnumField(bind: i.ty, label: "Type")
                ui.Toggle(bind: i.required, label: "Required")
                ui.Button(
                    "Remove",
                    icon: .x,
                    icon_only: true,
                    size: .small,
                    description: "Remove",
                ) {
                    on press: remove_input(w, d, i)
                }
            }
        }
        row(gap: .related, align: .center) {
            ui.TextInput(bind: w.new_input_name, placeholder: "Name")
            ui.EnumField(bind: w.new_input_type, label: "Type")
            ui.Button("Add input", icon: .plus) {
                on press: add_input(w, d, w.new_input_name, w.new_input_type)
                    .then(clear_new_input(w))
            }
        }
    }
}

view output_section(w: Workbench, d: Draft) {
    ui.Section(title: "Result") {
        row(gap: .related, align: .center) {
            text("What a finished run reports", role: .label)
            ui.MenuButton(
                label: if d.program.output.kind == .none {
                    "Not set"
                } else {
                    describe_value(d.program, d.program.output)
                },
                icon: .chevron_down,
            ) {
                ui.MenuItem("Not set", checked: d.program.output.kind == .none) {
                    on choose: clear_output(w, d)
                }
                for choice in output_choices(d.program) {
                    ui.MenuItem(choice.describes, checked: same_choice(d.program.output, choice)) {
                        on choose: set_output(w, d, choice)
                    }
                }
            }
        }
    }
}

view triggers_section(w: Workbench, d: Draft) {
    ui.Section(title: "Triggers") {
        if d.program.triggers.is_empty() {
            text(
                "Nothing starts it on its own yet. For now it runs when you press Run.",
                role: .caption,
                secondary: true,
            )
        }
        for t in d.program.triggers {
            ui.ListRow(title: trigger_words(t), secondary: trigger_detail(t), icon: .play) {
                trailing: {
                    row(gap: .related, align: .center) {
                        ui.Button(
                            "Edit",
                            icon: .pencil,
                            icon_only: true,
                            size: .small,
                            description: "Edit the trigger",
                        ) {
                            on press: edit_trigger(w, t)
                        }
                        ui.Button(
                            "Remove",
                            icon: .trash,
                            icon_only: true,
                            size: .small,
                            description: "Remove",
                        ) {
                            on press: remove_trigger(w, d, t)
                        }
                    }
                }
            }
        }
        ui.EnumField(bind: w.form.kind, label: "What starts it")
        if w.form.kind == .keyboard {
            ui.TextInput(bind: w.form.chord, placeholder: "Super+Shift+N")
        } else if w.form.kind == .app_opened {
            ui.TextInput(bind: w.form.app_id, placeholder: "so.tilo.notes")
        } else if w.form.kind == .schedule_day {
            row(gap: .related, align: .center) {
                ui.NumberInput(bind: w.form.hour, label: "Hour", min: 0.0, max: 23.0, precision: 0)
                ui.NumberInput(
                    bind: w.form.minute,
                    label: "Minute",
                    min: 0.0,
                    max: 59.0,
                    precision: 0,
                )
                ui.MenuButton(
                    label: if w.form.weekdays.is_empty() {
                        "Every day"
                    } else { w.form.weekdays.join(", ") },
                    icon: .calendar,
                ) {
                    for day in [
                        "monday",
                        "tuesday",
                        "wednesday",
                        "thursday",
                        "friday",
                        "saturday",
                        "sunday",
                    ] {
                        ui.MenuItem(day, checked: has_day(w.form.weekdays, day)) {
                            on choose: toggle_day(w, day)
                        }
                    }
                }
            }
            ui.Toggle(bind: w.form.catch_up, label: "Run once when a device was off at that time")
        } else if w.form.kind == .schedule_every {
            ui.NumberInput(
                bind: w.form.period_secs,
                label: "Seconds between runs",
                min: 1.0,
                precision: 0,
            )
            ui.Toggle(bind: w.form.catch_up, label: "Run once when a period was missed")
        } else if w.form.kind == .once {
            ui.TextInput(bind: w.form.once_text, placeholder: "2026-12-24T18:00")
        } else if w.form.kind == .workspace_joined {
            ui.TextInput(bind: w.form.workspace, placeholder: "Any workspace")
        } else if w.form.kind == .file_changed {
            ui.TextInput(bind: w.form.folder, placeholder: "The folder's capability")
        } else if w.form.kind == .focus_mode {
            ui.EnumField(bind: w.form.focus_mode, label: "Focus")
            ui.EnumField(bind: w.form.focus_transition, label: "Switches")
        } else if w.form.kind == .network_changed {
            ui.TextInput(bind: w.form.network, placeholder: "Any network")
            ui.EnumField(bind: w.form.connected, label: "Which change")
        } else if w.form.kind == .device_connected {
            ui.TextInput(bind: w.form.device_class, placeholder: "camera, display, storage")
        } else if w.form.kind == .resource_changed {
            ui.TextInput(bind: w.form.record_kind, placeholder: "chat.workspace")
            ui.TextInput(bind: w.form.actions_text, placeholder: "Actions, one per comma")
            ui.EnumField(bind: w.form.by, label: "Whose changes")
        } else if w.form.kind == .app_event {
            ui.TextInput(bind: w.form.app_id, placeholder: "so.tilo.mail")
            ui.TextInput(bind: w.form.event_kind, placeholder: "mail.message")
        } else if w.form.kind == .incoming_message {
            ui.TextInput(bind: w.form.app_id, placeholder: "Any app")
            ui.TextInput(bind: w.form.from, placeholder: "Anyone")
        } else if w.form.kind == .nearby_device {
            ui.TextInput(bind: w.form.device, placeholder: "Any device")
        } else if w.form.kind == .resource_time {
            ui.TextInput(bind: w.form.record_kind, placeholder: "tasks.issues")
            ui.TextInput(bind: w.form.time_field, placeholder: "due_ms")
            ui.NumberInput(bind: w.form.offset_secs, label: "Seconds from that time", precision: 0)
        } else {
            ui.TextInput(bind: w.form.record_kind, placeholder: "mail.account")
            ui.TextInput(bind: w.form.source_at, placeholder: "Which one, when it has a name")
            ui.NumberInput(
                bind: w.form.every_secs,
                label: "Look every this many seconds",
                min: 0.0,
                precision: 0,
            )
        }
        ui.TextInput(bind: w.form.filter, placeholder: "A filter, as JSON: field equals value")
        row(gap: .related, align: .center) {
            ui.Button(
                if is_editing(w) { "Apply" } else { "Add trigger" },
                icon: .check,
                role: .primary,
            ) {
                on press: apply_form(w, d)
            }
            if is_editing(w) {
                ui.Button("Cancel", role: .plain) {
                    on press: cancel_edit(w)
                }
            }
        }
    }
}

/// The steps of one block, each as its card, with the blocks inside them under it.
view block(w: Workbench, d: Draft, parent: Option<Step>, branch: Branch, title: Text, depth: Int) {
    if title != "" {
        text(title, role: .caption, secondary: true)
    }
    for s in siblings(d.program, parent, branch) {
        step_card(w, d, s)
        if not is_folded(d.folded, path_of(d.program, s)) {
            if let inner = opened_branch(s) {
                block(w, d, .some(s), inner, branch_title(inner), depth + 1)
                if s.head == .branch {
                    block(w, d, .some(s), .otherwise, "Otherwise", depth + 1)
                }
            }
        }
    }
}

view step_card(w: Workbench, d: Draft, s: Step) {
    let path = path_of(d.program, s)
    let number = number_of(d.program, s)
    let shut = is_folded(d.folded, path)
    ui.Card(title: "{number}. {step_words(d.program, s)}") {
        body: {
            row(gap: .related, align: .center) {
                if is_block(s) {
                    ui.Button(
                        "Show its steps",
                        icon: if shut { .chevron_right } else { .chevron_down },
                        icon_only: true,
                        size: .small,
                        description: "Fold",
                    ) {
                        on press: toggle_fold(w, d, path)
                    }
                }
                ui.Button(
                    "Move up",
                    icon: .chevron_up,
                    icon_only: true,
                    size: .small,
                    description: "Move up",
                ) {
                    on press: move_step(w, d, s, true)
                }
                ui.Button(
                    "Move down",
                    icon: .chevron_down,
                    icon_only: true,
                    size: .small,
                    description: "Move down",
                ) {
                    on press: move_step(w, d, s, false)
                }
                if let place = nest_place(d.program, s) {
                    ui.Button(
                        "Into the step above",
                        icon: .arrow_right,
                        icon_only: true,
                        size: .small,
                        description: "Move into the step above",
                    ) {
                        on press: nest_step(w, d, s)
                    }
                }
                if let place = lift_place(d.program, s) {
                    ui.Button(
                        "Out of its step",
                        icon: .arrow_left,
                        icon_only: true,
                        size: .small,
                        description: "Move out of its step",
                    ) {
                        on press: lift_step(w, d, s)
                    }
                }
                if is_block(s) {
                    ui.Button(
                        "Add inside",
                        icon: .plus,
                        icon_only: true,
                        size: .small,
                        description: "Add a step inside",
                    ) {
                        on press: open_picker(w, d, inside_address(d, s))
                    }
                }
                ui.Button(
                    "Remove",
                    icon: .trash,
                    icon_only: true,
                    size: .small,
                    description: "Remove",
                ) {
                    on press: remove_step(w, d, s)
                }
            }
            for problem in problem_at(d, path) {
                ui.Banner(problem.message, kind: .warning)
            }
            if not shut {
                step_fields(w, d, s)
            }
        }
    }
}

view step_fields(w: Workbench, d: Draft, s: Step) {
    if s.head == .branch {
        slot_row(w, d, s, "condition", "Condition", values_before(d.program, s))
    } else if s.head == .each {
        slot_row(w, d, s, "items", "Items", values_before(d.program, s))
        row(gap: .related, align: .center) {
            text("Each item is called", role: .label)
            ui.TextInput(bind: s.variable, placeholder: "item")
        }
    } else if s.head == .set {
        row(gap: .related, align: .center) {
            text("Variable name", role: .label)
            ui.TextInput(bind: s.name, placeholder: "value")
        }
        slot_row(w, d, s, "value", "Value", values_before(d.program, s))
    } else if s.head == .ask {
        row(gap: .related, align: .center) {
            text("Question", role: .label)
            ui.TextInput(bind: s.ask_description, placeholder: "Your answer")
            ui.EnumField(bind: s.ask_type, label: "Answer type")
        }
    } else if s.head == .ai {
        ui.TextInput(bind: s.prompt, placeholder: "What to do", lines: 2)
        slot_row(w, d, s, "ai_input", "What it reads", values_before(d.program, s))
    } else if s.head == .task {
        ui.TextInput(bind: s.instruction, placeholder: "What the task is to do", lines: 2)
    } else if s.head == .read {
        row(gap: .related, align: .center) {
            ui.TextInput(bind: s.read_app, placeholder: "App")
            ui.TextInput(bind: s.read_record, placeholder: "Record")
            ui.Toggle(bind: s.read_joined, label: "Every part")
        }
    } else {
        for param in s.params {
            argument_row(w, d, s, param)
        }
    }
    ui.EnumField(bind: s.policy, label: "When this step fails")
    if s.policy == .retry {
        row(gap: .related, align: .center) {
            ui.NumberInput(bind: s.attempts, label: "Tries", min: 1.0, precision: 0)
            ui.NumberInput(
                bind: s.backoff_ms,
                label: "Wait between tries, ms",
                min: 1.0,
                precision: 0,
            )
        }
    } else if s.policy == .carry_on {
        slot_row(w, d, s, "default", "Value to go on with", values_before(d.program, s))
    }
}

view argument_row(w: Workbench, d: Draft, s: Step, param: Param) {
    if let argument = s.args.find(one -> one.name == param.name) {
        row(gap: .related, align: .center) {
            text(if param.description != "" { param.description } else { param.name }, role: .label)
            ui.MenuButton(label: arg_text(d, argument), icon: .chevron_down) {
                ui.MenuItem("Not set", checked: argument.pick == Pick.none) {
                    on choose: clear_argument(w, d, s, argument)
                }
                for choice in argument.choices {
                    ui.MenuItem(
                        choice.describes,
                        checked: argument.pick == Pick.choice
                            and same_choice(argument.value, choice),
                    ) {
                        on choose: set_argument(w, d, s, argument, Pick.choice, choice)
                    }
                }
                ui.MenuItem("Type a value", checked: argument.pick == Pick.literal) {
                    on choose: type_argument(w, d, s, argument)
                }
            }
        }
        if argument.pick == Pick.literal {
            ui.TextInput(
                bind: argument.value.literal,
                placeholder: if param.description != "" { param.description } else { param.name },
            )
        }
    }
}

view slot_row(w: Workbench, d: Draft, s: Step, name: Text, label: Text, options: List<Choice>) {
    if let slot = s.slots.find(one -> one.name == name) {
        row(gap: .related, align: .center) {
            text(label, role: .label)
            ui.MenuButton(label: slot_text(d, slot), icon: .chevron_down) {
                ui.MenuItem("Not set", checked: slot.pick == Pick.none) {
                    on choose: clear_slot(w, d, s, name)
                }
                for choice in options {
                    ui.MenuItem(
                        choice.describes,
                        checked: slot.pick == Pick.choice and same_choice(slot.value, choice),
                    ) {
                        on choose: set_slot(w, d, s, name, Pick.choice, choice)
                    }
                }
                ui.MenuItem("Type a value", checked: slot.pick == Pick.literal) {
                    on choose: type_slot(w, d, s, name)
                }
            }
        }
        if slot.pick == Pick.literal {
            ui.TextInput(bind: slot.value.literal, placeholder: label)
        }
    }
}

/// The picker over the draft: a sheet, so what a press of Add step offers stands on screen and
/// reads whole, and it closes as the step is added or as the person dismisses it.
view picker(w: Workbench, d: Draft) {
    ui.ModalDialog(
        title: "Add a step",
        description: "An action one of your apps offers, or a block that holds steps.",
        open: true,
        primary: "Close",
    ) {
        on act_primary: close_picker(w)
        on dismiss: close_picker(w)
        body: {
            ui.TextInput(bind: w.picker_query, placeholder: "Narrow by action, app or resource")
            for head in [Head.branch, Head.each, Head.set, Head.ask, Head.ai, Head.task] {
                ui.ListRow(
                    title: block_offer_label(head),
                    secondary: block_offer_detail(head),
                    icon: .puzzle,
                ) {
                    on activate: add_block(w, d, head)
                }
            }
            if w.picker_loading {
                // The catalogue is read a moment after the sheet opens: say so, and read it then.
                ui.Timer(after: 0.01, restart: w.picker_block + d.shortcut_id) {
                    on tick: read_picker(w)
                }
                text("Reading the actions your apps offer…", role: .caption, secondary: true)
            } else if w.picker_failure != "" {
                text(
                    "The actions your apps offer could not be read",
                    role: .caption,
                    secondary: true,
                )
                text(w.picker_failure, role: .caption, secondary: true)
            } else if w.picker.is_empty() {
                text("No app offers actions here yet.", role: .caption, secondary: true)
            } else if w.picker.filter(offer -> offer_matches(w.picker_query, offer)).is_empty() {
                text("No actions match this filter", role: .caption, secondary: true)
            }
            for offer in w.picker {
                if offer_matches(w.picker_query, offer) {
                    ui.ListRow(
                        title: offer.label,
                        secondary: offer_detail(offer),
                        icon: if offer.reads { .eye } else { .zap },
                    ) {
                        on activate: add_offer(w, d, offer)
                    }
                }
            }
        }
    }
}

view saved_page(w: Workbench, s: Saved) {
    ui.ObjectHeader(
        title: s.name,
        caption: if s.paused { "Paused" } else { "Saved shortcut" },
        icon: .play,
    )
    row(gap: .related, align: .center) {
        ui.Button("Run", icon: .play, role: .primary) {
            on press: run(w, s)
        }
        if s.paused {
            ui.Button("Resume", icon: .play) {
                on press: resume_saved(w, s)
            }
        } else {
            ui.Button("Pause", icon: .pause) {
                on press: pause_saved(w, s)
            }
        }
        ui.Button("Stop the run", icon: .square) {
            on press: stop_saved(w, s)
        }
        ui.Button("Edit", icon: .pencil) {
            on press: open_shortcut(w, saved_json(s))
        }
        ui.Button("Delete", icon: .trash, role: .destructive) {
            on press: delete_saved(w, s)
        }
        ui.Button("Stop every run", role: .plain) {
            on press: stop_all(w)
        }
    }
    if let call = last_run(s) {
        ui.Section(title: "Last invocation") {
            text(run_words(call))
            text(when(call.started_ms), role: .caption, secondary: true)
        }
    }
    if not s.program.inputs.is_empty() {
        ui.Section(title: "Inputs") {
            for i in s.program.inputs {
                row(gap: .related, align: .center) {
                    text(i.name, role: .label)
                    ui.TextInput(bind: i.value, placeholder: i.description)
                }
            }
        }
    }
    let steps = saved_steps(s.program)
    if not steps.is_empty() {
        ui.Section(title: "Steps") {
            for line in steps {
                text(line)
            }
        }
    }
    ui.Section(title: "Refine it") {
        ui.TextInput(
            bind: w.comment,
            placeholder: "Say what the last run should do differently",
            lines: 2,
        )
        ui.Button("Refine it", icon: .wand_sparkles, role: .primary) {
            on press: refine(w, s, w.comment)
        }
    }
}

view save_sheet(w: Workbench, d: Draft) {
    ui.ModalDialog(
        title: "Save this shortcut?",
        description: "Saving lets this shortcut use the app actions listed here, and nothing else.",
        open: true,
        primary: if d.problems.is_empty() { "Save shortcut" } else { "Close" },
        secondary: "Cancel",
    ) {
        on act_primary: save(w, d).then(show_sheet(w, ""))
        on act_secondary: show_sheet(w, "")
        on dismiss: show_sheet(w, "")
        body: {
            if d.problems.is_empty() {
                if d.grants.is_empty() {
                    text(
                        "It uses no app actions, so it can't read or change anything in your apps.",
                        role: .caption,
                    )
                }
                for g in d.grants {
                    text(grant_line(g))
                }
            } else {
                ui.Banner("Fix these before saving", kind: .warning)
                for problem in d.problems {
                    text(problem_line(problem))
                }
            }
        }
    }
}
