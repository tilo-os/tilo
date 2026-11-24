app so.tilo.chip-design "Chip Design" (
    icon: .cpu,
    category: .development,
    tagline: "Read reported chip evidence and sketch the operators behind it",
    keywords: ["chip", "chip design", "chip-design", "operator", "operator graph", "hardware", "verification", "report", "evidence", "rtl", "netlist", "silicon", "chip-entwurf", "halbleiter", "nachweis"],
    splash_budget: 250,
)

/// The structural operator kinds a design is built from.
// ---------------------------------------------------------------------------
// What the project holds
// ---------------------------------------------------------------------------
enum OperatorKind {
    input
    gate_input
    norm_weight_input
    rms_norm
    apply_norm_weight
    silu
    multiply
    projection_input
    matrix_weight_input
    linear_projection
    add
}

/// How a reported check ended.
enum CheckResult {
    passed
    failed
}

/// How a reported run ended.
enum RunResult {
    passed
    failed
    cancelled
}

/// Whether a storage report bounds its attention cache, or cannot say.
enum CacheKind {
    bound
    unsupported
}

/// What a reported evidence stage observed.
enum ObservationKind {
    none
    cpu_reference
    rtl_simulation
    generic_synthesis
    library_physical
    rms_norm
    silu
    gated_delta
    softmax
}

/// What a numerical comparison asserts.
enum Comparison {
    exact
    characterization
}

/// How a reported RTL simulation ended.
enum Outcome {
    completed
    expected_refusal
}

/// What a reported evidence dependency carries.
enum DependencyKind {
    data
    build
    verification
}

/// Which stage of an architecture comparison a receipt reports.
enum StageKind {
    prepare
    numerical
    fault
    synthesis
    aggregate
}

/// The reported macro placements' orientation.
enum Orientation {
    r0
    r90
    r180
    r270
    mx
    my
    mxr90
    myr90
}

/// One typed port of an operator.
type Port (title: key) {
    /// Its identity on its operator: `value`, `weights`, `result`, `left` or `right`.
    key: Text
    /// The tensor type it moves.
    data_type: Text
}

/// One operator of the structural design.
type Operator (title: node) {
    /// Its stable identity.
    node: Text
    kind: OperatorKind
    /// The F32 tensor shape; its last axis is the normalization width.
    shape: List<Int>
    normalization_width: Int
    /// The structural matrix output width, when the operator has a matrix.
    output_width: Int = 0
    has_output_width: Bool = false
    /// What a person typed for the shape, applied by `apply_shape`.
    shape_text: Text = ""
    /// What a person typed for the matrix output width, applied by `apply_width`.
    width_text: Text = ""
    x: Int
    y: Int
    inputs: Collection<Port>
    outputs: Collection<Port>

    search {
        hide inputs
        hide outputs
    }
}

/// One connection between two operator ports.
type Connection {
    from_node: Text
    from_port: Text
    to_node: Text
    to_port: Text
    /// The content identity of the connection.
    digest: Text
    /// Whether the stored design still connects it.
    active: Bool = true
    /// Why it does not, when it does not.
    reason: Text = ""
}

/// The storage assumptions a report states.
type Assumptions {
    logical_parameters: Int = 0
    weight_bits: Int = 0
    kv_bits: Int = 0
    context_tokens: Int = 0
}

/// One analytical storage report, as imported and as a person reads it.
type Report (title: model) {
    /// The content identity of the imported bytes.
    digest: Text
    /// The exact imported report, as the producer wrote it.
    source: Text
    /// The model repository the report is about.
    model: Text
    schema_version: Int
    revision: Text
    config_sha256: Text
    evidence: Text
    batch_size: Int
    assumptions: Assumptions
    packed_weight_bytes: Int
    cache: CacheKind
    cache_reason: Text = ""
    attention_layers: Int = 0
    kv_elements: Int = 0
    kv_bytes: Int = 0
    cache_excludes: List<Text>
    not_estimated: List<Text>
    limitations: List<Text>

    search {
        sort model a_z
    }
}

/// One reported check.
type Check (title: name) {
    name: Text
    result: CheckResult
}

/// One tool identity a receipt reports.
type Tool (title: name) {
    name: Text
    version: Text
    executable_sha256: Text
}

/// One artifact identity a receipt reports.
type Artifact (title: name) {
    name: Text
    sha256: Text
}

/// One named digest a receipt reports.
type Digest (title: name) {
    name: Text
    sha256: Text
}

/// One generic cell count a superblock receipt reports.
type CellCount (title: name) {
    name: Text
    count: Int
}

/// One numerical observation of a CPU operator receipt.
type Observation (title: label) {
    /// What the observation compared, in words.
    label: Text
    kind: ObservationKind
    /// What the comparison asserts.
    comparison: Comparison = .exact
    compared_words: Int = 0
    differing_words: Int = 0
    output_sha256: Text = ""
    outcome: Outcome = .completed
    refused_inputs: Int = 0
    cycles: Int = 0
    accepted_inputs: Int = 0
    accepted_outputs: Int = 0
    logic_cells: Int = 0
    logical_memory_bits: Int = 0
    library: Text = ""
    clock_constraint_ns: Text = ""
    setup_slack_ns: Text = ""
    hold_slack_ns: Text = ""
    routed_signal_nets: Int = 0
    router_violations: Int = 0
    library_area_um2: Text = ""
    /// The RMS profile this comparison ran over.
    profile: Text = ""
    fp32_comparisons: Int = 0
    /// SiLU's finite inputs and its exponential differences.
    total_inputs: Int = 0
    exp_differences: Int = 0
    max_exp_ulp: Int = 0
    /// Gated-delta shape and the words it compared.
    heads: Int = 0
    tokens: Int = 0
    width: Int = 0
    attention_words: Int = 0
    state_words: Int = 0
    /// Softmax's masked inputs and its bounded differences.
    name: Text = ""
    masked: Int = 0
    max_ulp: Int = 0
}

/// One logical memory a reported synthesis observed.
type Memory (title: name) {
    name: Text
    width_bits: Int
    depth_words: Int
    bits: Int
    read_ports: Int
    clocked_read_ports: Int
    write_ports: Int
    fully_initialized: Bool
}

/// One variant of a reported architecture comparison.
type Variant (title: identity) {
    identity: Text
    /// The numerical comparison, when the stage reports one.
    numerical: Bool = false
    cold_program_cycles: Int = 0
    vector_load_cycles: Int = 0
    warm_cycles: Int = 0
    accepted_weight_bytes: Int = 0
    accepted_descriptor_bytes: Int = 0
    cache_reads: Int = 0
    quantizer_packets: Int = 0
    comparison_words: Int = 0
    output_sha256: Text = ""
    /// The generic synthesis this variant observed, when the stage reports one.
    synthesis: Bool = false
    generic_logic_cells: Int = 0
    activation_cache_bits: Int = 0
    programmed_weight_bits: Int = 0
    fixed_initialized_bits: Int = 0
    memories: Collection<Memory>
}

/// One stage link of a reported architecture comparison.
type StageLink (title: name) {
    name: Text
    receipt_sha256: Text
    producer_sha256: Text
    input_identity_sha256: Text
}

/// One stage of a reported evidence flow.
type Stage (title: title) {
    /// The producer's identity for the stage.
    key: Text
    title: Text
    status: RunResult
    original_receipt_artifact: Text
    producer_sha256: Text
    input_identity_sha256: Text
    limitations: List<Text>
    observed: ObservationKind = .none
    observation: Text = ""
    checks: Collection<Check>
    /// The reported macro placement, when the stage reports one.
    placed: Bool = false
    database_artifact: Text = ""
    observation_artifact: Text = ""
    dbu_per_micron: Int = 0
    die_left: Int = 0
    die_bottom: Int = 0
    die_right: Int = 0
    die_top: Int = 0
    total_instances: Int = 0
    omitted_instances: Int = 0
    macros: Collection<Macro>

    search {
        hide macros
        hide limitations
    }
}

/// One macro a reported placement holds.
type Macro (title: name) {
    /// The reported macro identity.
    slot: Text
    name: Text
    master: Text
    orientation: Orientation
    placement: Text
    left: Int
    bottom: Int
    right: Int
    top: Int
}

/// One reported evidence dependency.
type Dependency (title: artifact) {
    from: Text
    to: Text
    kind: DependencyKind
    artifact: Text
}

/// One reported subflow an evidence flow references.
type Subflow (title: title) {
    /// The producer's identity for the referenced subflow.
    key: Text
    title: Text
    summary_artifact: Text
}

/// One imported hardware verification receipt, as a person reads it.
type Receipt (title: evidence) {
    /// The identity of the sanitized receipt this project keeps.
    digest: Text
    /// The sanitized receipt, as this project keeps it.
    source: Text
    evidence: Text
    status: RunResult
    imported_bytes_sha256: Text = ""
    failure: Text = ""
    failure_stage: Text = ""
    input_validation: CheckResult = .passed
    has_input_validation: Bool = false
    lanes: Int = 0
    has_lanes: Bool = false
    group_size: Int = 0
    has_group_size: Bool = false
    declared_toolchain_image: Text = ""
    sample_manifest_sha256: Text = ""
    verifier_sha256: Text = ""
    artifact_validator_sha256: Text = ""
    mapper_sha256: Text = ""
    library_revision: Text = ""
    mapped_cell_count: Int = 0
    has_mapped_cell_count: Bool = false
    reported_library_area: Text = ""
    abc_delay_target_ps: Int = 0
    has_abc_target: Bool = false
    manufacturable: Bool = true
    has_manufacturable: Bool = false
    synthetic_coefficients: Bool = true
    has_synthetic_coefficients: Bool = false
    synthetic_activations: Bool = true
    has_synthetic_activations: Bool = false
    sample_sha256: Text = ""
    sample_receipt_sha256: Text = ""
    rtl_source_sha256: Text = ""
    real_protocol: Text = ""
    c_instrumentation: Text = ""
    external_scalar_comparisons: Int = 0
    has_external_scalar_comparisons: Bool = false
    generic_cell_count: Int = 0
    has_generic_cell_count: Bool = false
    excluded_metadata_cells: Int = 0
    has_excluded_metadata_cells: Bool = false
    compiler_flags: List<Text>
    checks: Collection<Check>
    tools: Collection<Tool>
    artifacts: Collection<Artifact>
    excludes: List<Text>
    input_digests: Collection<Digest>
    primary_sources: Collection<Digest>
    sources: Collection<Digest>
    generic_cells: Collection<CellCount>
    // The CPU operator summary, when the receipt is one.
    operator: Bool = false
    op_original_receipt_sha256: Text = ""
    op_primary_revision: Text = ""
    op_compiler_version: Text = ""
    op_compiler_executable_sha256: Text = ""
    op_compiler_flags: List<Text>
    op_primary_sources: Collection<Digest>
    op_local_sources: Collection<Digest>
    op_libraries: Collection<Digest>
    observations: Collection<Observation>
    // The architecture comparison, when the receipt is one.
    study: Bool = false
    study_stage: StageKind = .prepare
    study_original_receipt_sha256: Text = ""
    study_producer_sha256: Text = ""
    study_input_identity_sha256: Text = ""
    study_expected_comparison_words: Int = 0
    links: Collection<StageLink>
    variants: Collection<Variant>
    // The reported evidence flow, when the receipt is one.
    flow: Bool = false
    flow_title: Text = ""
    flow_input_identity_sha256: Text = ""
    stages: Collection<Stage>
    dependencies: Collection<Dependency>
    subflows: Collection<Subflow>

    search {
        filter status
        hide stages
        hide checks
        hide artifacts
        hide variants
    }
}

/// A workspace's chip design project: the evidence it imported and the structural operators a
/// person sketched from it.
type Project (untitled: "Chip Design") {
    reports: Collection<Report>
    verifications: Collection<Receipt>
    operators: Collection<Operator>
    connections: Collection<Connection>
    /// What the window shows: `all`, `graph`, `report` or `receipt`.
    selected: Text = "all"
    /// The digest of the report or receipt the window shows.
    subject: Text = ""
    /// The operator whose own facts the window shows.
    node: Text = ""
    /// The output a person chose to connect from.
    output_node: Text = ""
    output_port: Text = ""
    /// How far the graph is zoomed, in percent.
    zoom: Int = 100
    /// The evidence stage the shown receipt holds open.
    stage: Text = ""
    /// The placement macro the shown stage holds open.
    macro: Text = ""
    /// Why the last thing a person asked for was refused, in the engine's words.
    failure: Text = ""
    /// What last happened, said plainly.
    notice: Text = ""
}

root project: Project

default view window for Project

default view report_page for Report

default view receipt_page for Receipt

default view stage_page for Stage

default view macro_page for Macro

default view subflow_page for Subflow

default view operator_page for Operator

// ---------------------------------------------------------------------------
// The engine's vocabulary
// ---------------------------------------------------------------------------
type ImportAsk {
    json: Text
}

type NodeAsk {
    id: Text
    kind: OperatorKind
    shape: List<Int>
    /// The matrix output width; zero when the operator has no matrix.
    output_width: Int = 0
    x: Int
    y: Int
}

type EdgeAsk {
    from_node: Text
    from_port: Text
    to_node: Text
    to_port: Text
}

type GraphAsk {
    nodes: List<NodeAsk>
    connections: List<EdgeAsk>
}

/// Imports the engineering evidence a JSON file holds: an analytical storage report or a
/// hardware verification receipt. This is what a person's files and the file chooser hand the
/// app, and importing the same bytes twice keeps one record.
// ---------------------------------------------------------------------------
// Importing engineering evidence
// ---------------------------------------------------------------------------
action import_file(p: Project, file: File) (
    icon: .file_plus,
    label: "Import report",
    opens: ["application/json", .json],
) {
    require file.size <= 1048576 "The report exceeds the 1 MiB import limit"
    let document = match file.text() {
        .ok(text) => text,
        .err(why) => {
            p.failure = why
            return
        }
    }
    take_in(p, document)
}

/// Imports an analytical storage report from its JSON.
action import_report(p: Project, document: Text) (
    icon: .file_plus,
    label: "Import analytical report",
) {
    take_in(p, document)
}

/// Imports a hardware verification receipt from its JSON.
action import_verification(p: Project, document: Text) (
    icon: .shield_check,
    label: "Import verification receipt",
) {
    take_in(p, document)
}

/// Takes in one imported document, whichever it is: the device checks it first, and what it
/// holds reaches the project only once its own contract accepts it.
action take_in(p: Project, document: Text) {
    p.failure = ""
    p.notice = ""
    if document.len() > 1048576 {
        p.failure = "The report exceeds the 1 MiB import limit"
        return
    }
    let asked = json.encode(ImportAsk(json: document))
    match engine.call("import", asked) {
        .err(why) => {
            p.failure = why
        }
        .ok(answer) => {
            match json.parse(answer) {
                .err(why) => {
                    p.failure = why
                }
                .ok(value) => {
                    let kind = std.json_text(value, "kind")
                    let id = std.json_text(value, "id")
                    let stored = std.json_text(value, "stored")
                    if let facts = std.json_field(value, "facts") {
                        if kind == "report" {
                            if not p.reports.any(r -> r.digest == id) {
                                if p.reports.len() >= 128 {
                                    p.failure = "The project's report limit is reached"
                                    return
                                }
                                keep_report(p, id, stored, facts)
                            }
                            p.selected = "report"
                            p.subject = id
                            p.notice = "The report is imported."
                        } else {
                            if not p.verifications.any(r -> r.digest == id) {
                                if p.verifications.len() >= 128 {
                                    p.failure = "The project's verification limit is reached"
                                    return
                                }
                                keep_receipt(p, id, stored, facts)
                            }
                            p.selected = "receipt"
                            p.subject = id
                            p.stage = ""
                            p.macro = ""
                            p.notice = "The receipt is imported."
                        }
                    } else {
                        p.failure = "The device answered no report."
                    }
                }
            }
        }
    }
}

/// Keeps one analytical storage report and the assumptions and scope it states.
action keep_report(p: Project, digest: Text, source: Text, facts: json.Value) {
    let report = p.reports.insert(
        Report(
            digest: digest,
            source: source,
            schema_version: std.json_number(facts, "schema_version"),
            model: std.json_text(facts, "model"),
            revision: std.json_text(facts, "revision"),
            config_sha256: std.json_text(facts, "config_sha256"),
            evidence: std.json_text(facts, "evidence"),
            batch_size: std.json_number(facts, "batch_size"),
            packed_weight_bytes: std.json_number(facts, "packed_weight_bytes"),
            cache: if cache_status(facts) == "unsupported" { .unsupported } else { .bound },
            cache_reason: cache_reason_of(facts),
            attention_layers: if let n = std.json_field(facts, "full_attention_cache") {
                std.json_number(n, "full_attention_layers")
            } else { 0 },
            kv_elements: if let n = std.json_field(facts, "full_attention_cache") {
                std.json_number(n, "kv_elements")
            } else { 0 },
            kv_bytes: if let n = std.json_field(facts, "full_attention_cache") {
                std.json_number(n, "packed_bytes")
            } else { 0 },
            cache_excludes: if let n = std.json_field(facts, "full_attention_cache") {
                std.json_texts(n, "excludes")
            } else { [] },
            not_estimated: std.json_texts(facts, "not_estimated"),
            limitations: std.json_texts(facts, "limitations"),
        ),
    )
    if let stated = std.json_field(facts, "assumptions") {
        report.assumptions.logical_parameters = std.json_number(stated, "logical_parameters")
        report.assumptions.weight_bits = std.json_number(stated, "weight_bits")
        report.assumptions.kv_bits = std.json_number(stated, "kv_bits")
        report.assumptions.context_tokens = std.json_number(stated, "context_tokens")
    }
}

fn cache_status(facts: json.Value) -> Text {
    if let cache = std.json_field(facts, "full_attention_cache") {
        std.json_text(cache, "status")
    } else { "" }
}

fn cache_reason_of(facts: json.Value) -> Text {
    if let cache = std.json_field(facts, "full_attention_cache") {
        std.json_text(cache, "reason")
    } else { "" }
}

/// Keeps one hardware verification receipt: its scope, its checks, the identities it reports,
/// and whichever of the three reported protocols it speaks.
action keep_receipt(p: Project, digest: Text, source: Text, facts: json.Value) {
    let receipt = if let held = std.json_field(facts, "receipt") { held } else { facts }
    let record = p.verifications.insert(
        Receipt(
            digest: digest,
            source: source,
            evidence: std.json_text(receipt, "evidence"),
            status: run_result_of(std.json_text(receipt, "status")),
            imported_bytes_sha256: std.json_text(facts, "imported_bytes_sha256"),
            failure: std.json_text(receipt, "failure"),
            failure_stage: std.json_text(receipt, "failure_stage"),
            lanes: std.json_number(receipt, "lanes"),
            has_lanes: std.json_field(receipt, "lanes") != .none,
            group_size: std.json_number(receipt, "group_size"),
            has_group_size: std.json_field(receipt, "group_size") != .none,
            declared_toolchain_image: std.json_text(receipt, "declared_toolchain_image"),
            sample_manifest_sha256: std.json_text(receipt, "sample_manifest_sha256"),
            verifier_sha256: std.json_text(receipt, "verifier_sha256"),
            artifact_validator_sha256: std.json_text(receipt, "artifact_validator_sha256"),
            mapper_sha256: std.json_text(receipt, "mapper_sha256"),
            library_revision: std.json_text(receipt, "library_revision"),
            mapped_cell_count: std.json_number(receipt, "mapped_cell_count"),
            has_mapped_cell_count: std.json_field(receipt, "mapped_cell_count") != .none,
            reported_library_area: number_said(std.json_field(receipt, "reported_library_area")),
            abc_delay_target_ps: std.json_number(receipt, "abc_delay_target_ps"),
            has_abc_target: std.json_field(receipt, "abc_delay_target_ps") != .none,
            manufacturable: std.json_flag(receipt, "manufacturable"),
            has_manufacturable: std.json_field(receipt, "manufacturable") != .none,
            synthetic_coefficients: std.json_flag(receipt, "synthetic_coefficients"),
            has_synthetic_coefficients: std.json_field(receipt, "synthetic_coefficients") != .none,
            synthetic_activations: std.json_flag(receipt, "synthetic_activations"),
            has_synthetic_activations: std.json_field(receipt, "synthetic_activations") != .none,
            sample_sha256: std.json_text(receipt, "sample_sha256"),
            sample_receipt_sha256: std.json_text(receipt, "sample_receipt_sha256"),
            rtl_source_sha256: std.json_text(receipt, "rtl_source_sha256"),
            real_protocol: std.json_text(receipt, "real_protocol"),
            c_instrumentation: std.json_text(receipt, "c_instrumentation"),
            external_scalar_comparisons: std.json_number(receipt, "external_scalar_C_comparisons"),
            has_external_scalar_comparisons: std.json_field(
                receipt,
                "external_scalar_C_comparisons",
            )
                != .none,
            generic_cell_count: std.json_number(receipt, "generic_cell_count"),
            has_generic_cell_count: std.json_field(receipt, "generic_cell_count") != .none,
            excluded_metadata_cells: std.json_number(receipt, "excluded_metadata_cells"),
            has_excluded_metadata_cells: std.json_field(receipt, "excluded_metadata_cells")
                != .none,
            compiler_flags: std.json_texts(receipt, "compiler_flags"),
            excludes: std.json_texts(receipt, "excludes"),
        ),
    )
    if let validation = std.json_field(receipt, "input_validation") {
        record.input_validation = check_result_of(std.json_text_plain(validation))
        record.has_input_validation = true
    }
    for check in std.json_list(receipt, "checks") {
        record.checks.insert(
            Check(
                name: std.json_text(check, "name"),
                result: check_result_of(std.json_text(check, "result")),
            ),
        )
    }
    for tool in std.json_list(receipt, "tools") {
        record.tools.insert(
            Tool(
                name: std.json_text(tool, "name"),
                version: std.json_text(tool, "version"),
                executable_sha256: std.json_text(tool, "digest"),
            ),
        )
    }
    for artifact in std.json_list(receipt, "artifact_sha256") {
        record.artifacts.insert(
            Artifact(
                name: std.json_text(artifact, "name"),
                sha256: std.json_text(artifact, "digest"),
            ),
        )
    }
    for digest in std.json_list(receipt, "input_sha256") {
        record.input_digests.insert(
            Digest(name: std.json_text(digest, "name"), sha256: std.json_text(digest, "digest")),
        )
    }
    for digest in std.json_list(receipt, "primary_source_sha256") {
        record.primary_sources.insert(
            Digest(name: std.json_text(digest, "name"), sha256: std.json_text(digest, "digest")),
        )
    }
    for digest in std.json_list(receipt, "source_sha256") {
        record.sources.insert(
            Digest(name: std.json_text(digest, "name"), sha256: std.json_text(digest, "digest")),
        )
    }
    for cell in std.json_list(receipt, "generic_cells") {
        record.generic_cells.insert(
            CellCount(name: std.json_text(cell, "name"), count: std.json_number(cell, "count")),
        )
    }
    if let operator = std.json_field(receipt, "operator_summary") {
        record.operator = true
        record.op_original_receipt_sha256 = std.json_text(operator, "original_receipt_sha256")
        record.op_primary_revision = std.json_text(operator, "primary_revision")
        record.op_compiler_version = std.json_text(operator, "compiler_version")
        record.op_compiler_executable_sha256 = std.json_text(operator, "compiler_executable_sha256")
        record.op_compiler_flags = std.json_texts(operator, "compiler_flags")
        for digest in std.json_list(operator, "primary_source_sha256") {
            record.op_primary_sources.insert(
                Digest(
                    name: std.json_text(digest, "name"),
                    sha256: std.json_text(digest, "digest"),
                ),
            )
        }
        for digest in std.json_list(operator, "local_source_sha256") {
            record.op_local_sources.insert(
                Digest(
                    name: std.json_text(digest, "name"),
                    sha256: std.json_text(digest, "digest"),
                ),
            )
        }
        for digest in std.json_list(operator, "linked_library_sha256") {
            record.op_libraries.insert(
                Digest(
                    name: std.json_text(digest, "name"),
                    sha256: std.json_text(digest, "digest"),
                ),
            )
        }
        for case in std.json_list(operator, "observations") {
            record.observations.insert(observation_of(case))
        }
    }
    if let study = std.json_field(receipt, "architecture_study") {
        record.study = true
        record.study_stage = stage_kind_of(std.json_text(study, "stage"))
        record.study_original_receipt_sha256 = std.json_text(study, "original_receipt_sha256")
        record.study_producer_sha256 = std.json_text(study, "producer_sha256")
        record.study_input_identity_sha256 = std.json_text(study, "input_identity_sha256")
        record.study_expected_comparison_words = std.json_number(study, "expected_comparison_words")
        for link in std.json_list(study, "links") {
            record.links.insert(
                StageLink(
                    name: std.json_text(link, "name"),
                    receipt_sha256: std.json_text(link, "receipt_sha256"),
                    producer_sha256: std.json_text(link, "producer_sha256"),
                    input_identity_sha256: std.json_text(link, "input_identity_sha256"),
                ),
            )
        }
        for variant in std.json_list(study, "variants") {
            let held = record.variants.insert(Variant(identity: std.json_text(variant, "identity")))
            if let numerical = std.json_field(variant, "numerical") {
                held.numerical = true
                held.cold_program_cycles = std.json_number(numerical, "cold_program_cycles")
                held.vector_load_cycles = std.json_number(numerical, "vector_load_cycles")
                held.warm_cycles = std.json_number(numerical, "warm_cycles")
                held.accepted_weight_bytes = std.json_number(numerical, "accepted_weight_bytes")
                held.accepted_descriptor_bytes = std.json_number(
                    numerical,
                    "accepted_descriptor_bytes",
                )
                held.cache_reads = std.json_number(numerical, "cache_reads")
                held.quantizer_packets = std.json_number(numerical, "quantizer_packets")
                held.comparison_words = std.json_number(numerical, "comparison_words")
                held.output_sha256 = std.json_text(numerical, "output_sha256")
            }
            if let synthesis = std.json_field(variant, "synthesis") {
                held.synthesis = true
                held.generic_logic_cells = std.json_number(synthesis, "generic_logic_cells")
                held.activation_cache_bits = std.json_number(synthesis, "activation_cache_bits")
                held.programmed_weight_bits = std.json_number(synthesis, "programmed_weight_bits")
                held.fixed_initialized_bits = std.json_number(synthesis, "fixed_initialized_bits")
                for memory in std.json_list(synthesis, "memories") {
                    held.memories.insert(
                        Memory(
                            name: std.json_text(memory, "name"),
                            width_bits: std.json_number(memory, "width_bits"),
                            depth_words: std.json_number(memory, "depth_words"),
                            bits: std.json_number(memory, "bits"),
                            read_ports: std.json_number(memory, "read_ports"),
                            clocked_read_ports: std.json_number(memory, "clocked_read_ports"),
                            write_ports: std.json_number(memory, "write_ports"),
                            fully_initialized: std.json_flag(memory, "fully_initialized"),
                        ),
                    )
                }
            }
        }
    }
    if let flow = std.json_field(receipt, "evidence_flow") {
        record.flow = true
        record.flow_title = std.json_text(flow, "title")
        record.flow_input_identity_sha256 = std.json_text(flow, "input_identity_sha256")
        for stage in std.json_list(flow, "stages") {
            let held = record.stages.insert(
                Stage(
                    key: std.json_text(stage, "id"),
                    title: std.json_text(stage, "title"),
                    status: run_result_of(std.json_text(stage, "status")),
                    original_receipt_artifact: std.json_text(stage, "original_receipt_artifact"),
                    producer_sha256: std.json_text(stage, "producer_sha256"),
                    input_identity_sha256: std.json_text(stage, "input_identity_sha256"),
                    limitations: std.json_texts(stage, "limitations"),
                ),
            )
            for check in std.json_list(stage, "checks") {
                held.checks.insert(
                    Check(
                        name: std.json_text(check, "name"),
                        result: check_result_of(std.json_text(check, "result")),
                    ),
                )
            }
            if let observation = std.json_field(stage, "observations") {
                held.observed = observation_kind_of(std.json_text(observation, "scope"))
                held.observation = observation_said(observed_of(observation))
            }
            if let placement = std.json_field(stage, "placement") {
                held.placed = true
                held.database_artifact = std.json_text(placement, "database_artifact")
                held.observation_artifact = std.json_text(placement, "observation_artifact")
                held.dbu_per_micron = std.json_number(placement, "dbu_per_micron")
                let die = std.json_numbers(placement, "die_dbu")
                held.die_left = at(die, 0)
                held.die_bottom = at(die, 1)
                held.die_right = at(die, 2)
                held.die_top = at(die, 3)
                held.total_instances = std.json_number(placement, "total_instances")
                held.omitted_instances = std.json_number(placement, "omitted_instances")
                for item in std.json_list(placement, "macros") {
                    let bounds = std.json_numbers(item, "bounds_dbu")
                    held.macros.insert(
                        Macro(
                            slot: std.json_text(item, "id"),
                            name: std.json_text(item, "name"),
                            master: std.json_text(item, "master"),
                            orientation: orientation_of(std.json_text(item, "orientation")),
                            placement: std.json_text(item, "status"),
                            left: at(bounds, 0),
                            bottom: at(bounds, 1),
                            right: at(bounds, 2),
                            top: at(bounds, 3),
                        ),
                    )
                }
            }
        }
        for edge in std.json_list(flow, "edges") {
            record.dependencies.insert(
                Dependency(
                    from: std.json_text(edge, "from"),
                    to: std.json_text(edge, "to"),
                    kind: dependency_kind_of(std.json_text(edge, "kind")),
                    artifact: std.json_text(edge, "artifact"),
                ),
            )
        }
        for link in std.json_list(flow, "subflows") {
            record.subflows.insert(
                Subflow(
                    key: std.json_text(link, "id"),
                    title: std.json_text(link, "title"),
                    summary_artifact: std.json_text(link, "summary_artifact"),
                ),
            )
        }
    }
}

/// The observation one evidence stage reported, as the view's own record.
fn observed_of(observation: json.Value) -> Observation {
    let kind = observation_kind_of(std.json_text(observation, "scope"))
    Observation(
        label: observation_words(kind),
        kind: kind,
        comparison: if std.json_text(observation, "comparison") == "characterization" {
            .characterization
        } else { .exact },
        compared_words: std.json_number(observation, "compared_words"),
        differing_words: std.json_number(observation, "differing_words"),
        output_sha256: std.json_text(observation, "output_sha256"),
        outcome: if std.json_text(observation, "outcome") == "expected_refusal" {
            .expected_refusal
        } else { .completed },
        refused_inputs: std.json_number(observation, "refused_inputs"),
        cycles: std.json_number(observation, "cycles"),
        accepted_inputs: std.json_number(observation, "accepted_inputs"),
        accepted_outputs: std.json_number(observation, "accepted_outputs"),
        logic_cells: std.json_number(observation, "logic_cells"),
        logical_memory_bits: std.json_number(observation, "logical_memory_bits"),
        library: std.json_text(observation, "library"),
        clock_constraint_ns: number_said(std.json_field(observation, "clock_constraint_ns")),
        setup_slack_ns: number_said(std.json_field(observation, "setup_slack_ns")),
        hold_slack_ns: number_said(std.json_field(observation, "hold_slack_ns")),
        routed_signal_nets: std.json_number(observation, "routed_signal_nets"),
        router_violations: std.json_number(observation, "router_violations"),
        library_area_um2: number_said(std.json_field(observation, "library_area_um2")),
    )
}

/// One numerical observation of a CPU operator receipt, from its own words.
fn observation_of(case: json.Value) -> Observation {
    let kind = observation_kind_of(std.json_text(case, "kind"))
    Observation(
        label: observation_label(kind),
        kind: kind,
        compared_words: std.json_number(case, "fp32_comparisons"),
        profile: std.json_text(case, "profile"),
        fp32_comparisons: std.json_number(case, "fp32_comparisons"),
        total_inputs: std.json_number(case, "total_inputs"),
        exp_differences: std.json_number(case, "exp_differences"),
        max_exp_ulp: std.json_number(case, "max_exp_ulp"),
        heads: std.json_number(case, "heads"),
        tokens: std.json_number(case, "tokens"),
        width: std.json_number(case, "width"),
        attention_words: std.json_number(case, "attention_words"),
        state_words: std.json_number(case, "state_words"),
        name: std.json_text(case, "name"),
        masked: std.json_number(case, "masked"),
        differing_words: std.json_number(case, "differing_words"),
        max_ulp: std.json_number(case, "max_ulp"),
    )
}

/// A reported decimal, as the engine wrote it; empty when the report holds none.
fn number_said(value: Option<json.Value>) -> Text {
    match value {
        .none => "",
        .some(held) => match held {
            .string(text) => text,
            .number(number) => "",
            .object(fields) => "",
            .array(items) => "",
            .bool(flag) => "",
            .null => "",
        },
    }
}

/// The `index`th number of a list, and zero where it does not reach.
fn at(numbers: List<Int>, index: Int) -> Int {
    numbers.at(index) ?? 0
}

fn check_result_of(value: Text) -> CheckResult {
    if value == "failed" { .failed } else { .passed }
}

fn run_result_of(value: Text) -> RunResult {
    if value == "failed" { .failed } else if value == "cancelled" { .cancelled } else { .passed }
}

fn observation_kind_of(value: Text) -> ObservationKind {
    if value == "cpu_reference" {
        .cpu_reference
    } else if value == "rtl_simulation" {
        .rtl_simulation
    } else if value == "generic_synthesis" {
        .generic_synthesis
    } else if value == "library_physical" {
        .library_physical
    } else if value == "rms_norm" {
        .rms_norm
    } else if value == "silu" {
        .silu
    } else if value == "gated_delta" {
        .gated_delta
    } else if value == "softmax" { .softmax } else { .none }
}

/// What one observation compared, in words, for its record's name and list row.
fn observation_label(kind: ObservationKind) -> Text {
    if kind == .rms_norm {
        "RMS normalization comparison"
    } else if kind == .silu {
        "SiLU comparison"
    } else if kind == .gated_delta {
        "Gated-delta comparison"
    } else if kind == .softmax { "Softmax comparison" } else { observation_words(kind) }
}

fn dependency_kind_of(value: Text) -> DependencyKind {
    if value == "build" { .build } else if value == "verification" { .verification } else { .data }
}

fn stage_kind_of(value: Text) -> StageKind {
    if value == "numerical" {
        .numerical
    } else if value == "fault" {
        .fault
    } else if value == "synthesis" {
        .synthesis
    } else if value == "aggregate" { .aggregate } else { .prepare }
}

fn orientation_of(value: Text) -> Orientation {
    if value == "R90" {
        .r90
    } else if value == "R180" {
        .r180
    } else if value == "R270" {
        .r270
    } else if value == "MX" {
        .mx
    } else if value == "MY" {
        .my
    } else if value == "MXR90" { .mxr90 } else if value == "MYR90" { .myr90 } else { .r0 }
}

/// Removes a report and everything it stated.
action remove_report(p: Project, r: Report) (icon: .trash_2, label: "Remove report") {
    for held in p.reports {
        if held.digest == r.digest { p.reports.remove(held) }
    }
    p.selected = "all"
    p.subject = ""
    p.notice = "The report is removed."
}

/// Removes a verification receipt and everything it stated.
action remove_verification(p: Project, r: Receipt) (
    icon: .trash_2,
    label: "Remove verification receipt",
) {
    for held in p.verifications {
        if held.digest == r.digest { p.verifications.remove(held) }
    }
    p.selected = "all"
    p.subject = ""
    p.stage = ""
    p.macro = ""
    p.notice = "The receipt is removed."
}

/// Opens the operator graph.
// ---------------------------------------------------------------------------
// What the window shows
// ---------------------------------------------------------------------------
action show_graph(p: Project) (icon: .git_branch, label: "Operator graph") {
    p.selected = "graph"
    p.subject = ""
    p.stage = ""
    p.macro = ""
}

/// Lists every report and receipt the project holds.
action show_all(p: Project) (icon: .list, label: "All reports") {
    p.selected = "all"
    p.subject = ""
    p.stage = ""
    p.macro = ""
}

/// Shows one report.
action inspect_report(p: Project, r: Report) (icon: .file_text, label: "Inspect report") {
    p.selected = "report"
    p.subject = r.digest
    p.stage = ""
    p.macro = ""
    p.notice = ""
}

/// Shows one verification receipt.
action inspect_receipt(p: Project, r: Receipt) (
    icon: .shield_check,
    label: "Inspect verification receipt",
) {
    p.selected = "receipt"
    p.subject = r.digest
    p.stage = ""
    p.macro = ""
    p.notice = ""
}

/// Shows one stage of the receipt the window holds open.
action inspect_stage(p: Project, r: Receipt, s: Stage) (
    icon: .file_search,
    label: "Inspect evidence",
) {
    p.selected = "receipt"
    p.subject = r.digest
    p.stage = s.key
    p.macro = ""
}

/// Shows one macro of the placement the window holds open.
action inspect_macro(p: Project, r: Receipt, m: Macro) (icon: .square, label: "Inspect placement") {
    p.selected = "receipt"
    p.subject = r.digest
    p.macro = m.slot
}

/// Opens the receipt an evidence flow's subflow reference names, when its summary is imported.
action open_subflow(p: Project, r: Receipt, s: Subflow) {
    if let digest = artifact_digest(r, s.summary_artifact) {
        if let target = p.verifications.find(
                other -> other.flow and other.imported_bytes_sha256 == digest,
            ) {
            inspect_receipt(p, target)
            return
        }
    }
    p.failure = "The referenced summary has not been imported. No execution claim is made for this reference."
}

/// The digest a receipt reports for one artifact.
fn artifact_digest(r: Receipt, name: Text) -> Option<Text> {
    if let found = r.artifacts.find(a -> a.name == name) { .some(found.sha256) } else { .none }
}

/// Shrugs off the line about the last refusal or success.
action dismiss(p: Project) {
    p.failure = ""
    p.notice = ""
}

/// Zooms the operator graph in, out, or back to its whole extent.
action zoom_graph(p: Project, step: Text) (icon: .search, label: "Zoom the graph") {
    if step == "in" {
        p.zoom = if p.zoom < 400 { p.zoom + 25 } else { 400 }
    } else if step == "out" {
        p.zoom = if p.zoom > 25 { p.zoom - 25 } else { 25 }
    } else if step == "fit" {
        p.zoom = 100
    } else {
        p.failure = "There is no zoom step by that name"
    }
}

/// Chooses the output a person connects from, naming the inputs it can feed.
action select_output(p: Project, op: Operator, port: Text) (
    icon: .git_branch,
    label: "Choose output",
) {
    if not op.outputs.any(out -> out.key == port) {
        p.failure = "This output port no longer exists"
        return
    }
    p.output_node = op.node
    p.output_port = port
}

/// Leaves the graph without a chosen output.
action clear_output(p: Project) {
    p.output_node = ""
    p.output_port = ""
}

/// Shows one operator's own facts, or closes them.
action inspect_operator(p: Project, op: Operator) (icon: .cpu, label: "Inspect operator") {
    p.node = op.node
}

action close_operator(p: Project) {
    p.node = ""
}

/// Adds an operator of `kind` after the ones already in the graph, on a four-wide grid.
// ---------------------------------------------------------------------------
// Building the structural design
// ---------------------------------------------------------------------------
action add_operator(p: Project, name: Text) uses random (icon: .plus, label: "Add operator") {
    let kind = kind_named(name)
    let count = p.operators.len()
    let shape = if kind == .norm_weight_input or kind == .matrix_weight_input {
        [8]
    } else { [4, 8] }
    let width = if kind == .matrix_weight_input or kind == .linear_projection { 8 } else { 0 }
    add_operator_at(p, random.id(), kind, shape, width, count % 4 * 280, count / 4 * 320)
}

/// Adds one operator with exactly the shape, matrix width and place a caller gives it. The
/// device checks the whole design before any of it is kept.
action add_operator_at(
    p: Project,
    node: Text,
    kind: OperatorKind,
    shape: List<Int>,
    output_width: Int,
    x: Int,
    y: Int,
) (icon: .plus, label: "Add operator with a shape") {
    p.failure = ""
    if p.operators.len() >= 64 {
        p.failure = "The operator limit of this design is reached"
        return
    }
    if p.operators.any(op -> op.node == node) {
        let held = p.operators.find(op -> op.node == node)
        if let existing = held {
            if same_operator(existing, kind, shape, output_width, x, y) {
                return
            }
        }
        p.failure = "An operator with that identity already exists"
        return
    }
    let candidate = NodeAsk(
        id: node,
        kind: kind,
        shape: shape,
        output_width: output_width,
        x: x,
        y: y,
    )
    let asked = graph_ask(p, [candidate], [])
    match engine.call("graph", asked) {
        .err(why) => {
            p.failure = why
        }
        .ok(answer) => {
            p.operators.insert(
                Operator(
                    node: node,
                    kind: kind,
                    shape: shape,
                    normalization_width: at(shape, shape.len() - 1),
                    output_width: output_width,
                    has_output_width: output_width > 0,
                    x: x,
                    y: y,
                ),
            )
            apply_graph(p, answer)
        }
    }
}

fn same_operator(
    op: Operator,
    kind: OperatorKind,
    shape: List<Int>,
    output_width: Int,
    x: Int,
    y: Int,
) -> Bool {
    op.kind == kind
        and op.shape == shape
        and op.output_width == output_width
        and op.x == x
        and op.y == y
}

/// Reshapes one operator. Its last axis sets the normalization width, and every connection the
/// new shape cannot hold stops holding.
action set_shape(p: Project, op: Operator, shape: List<Int>) (
    icon: .maximize_2,
    label: "Set operator input shape",
) {
    p.failure = ""
    if shape.is_empty() {
        p.failure = "An operator needs a shape"
        return
    }
    let asked = graph_ask_without(p, op, shape, 0)
    match engine.call("graph", asked) {
        .err(why) => {
            p.failure = why
        }
        .ok(answer) => {
            op.shape = shape
            op.normalization_width = at(shape, shape.len() - 1)
            apply_graph(p, answer)
        }
    }
}

/// Applies the shape a person typed for one operator, when it names one.
action apply_shape(p: Project, op: Operator) (icon: .check, label: "Apply shape") {
    let shape = parsed_shape(op.shape_text)
    if shape.is_empty() {
        p.failure = "A shape is up to four positive whole numbers, like [4, 8]"
        return
    }
    op.shape_text = ""
    set_shape(p, op, shape)
}

/// Applies the matrix output width a person typed for one operator.
action apply_width(p: Project, op: Operator) (icon: .check, label: "Apply output width") {
    let width = op.width_text.trim().to_int() ?? 0
    if width <= 0 {
        p.failure = "A matrix width is a whole number of at least 1"
        return
    }
    op.width_text = ""
    set_output_width(p, op, width)
}

/// A shape written as `[4, 8]` or `4, 8`, and nothing when the words name no shape.
fn parsed_shape(text: Text) -> List<Int> {
    let parts = text.replace("[", "").replace("]", "").split(",")
    let numbers: List<Int> = parts.map(part -> part.trim().to_int() ?? 0)
    if numbers.len() > 4 or numbers.any(n -> n <= 0) { [] } else { numbers }
}

/// Shows one placement macro of the stage the window holds open.
action tap_macro(p: Project, s: Stage, index: Int) {
    if index <= 0 {
        return
    }
    let placed = s.macros.sort_by(m -> m.name).at(index - 1)
    if let m = placed {
        if let r = p.verifications.find(held -> held.stages.any(stage -> stage.key == s.key)) {
            inspect_macro(p, r, m)
        }
    }
}

/// Sets the matrix output width of one operator.
action set_output_width(p: Project, op: Operator, width: Int) (
    icon: .maximize_2,
    label: "Set matrix output width",
) {
    p.failure = ""
    if width <= 0 or width > 1048576 {
        p.failure = "A matrix width is between 1 and 1048576"
        return
    }
    let asked = graph_ask_without(p, op, [], width)
    match engine.call("graph", asked) {
        .err(why) => {
            p.failure = why
        }
        .ok(answer) => {
            op.output_width = width
            op.has_output_width = true
            apply_graph(p, answer)
        }
    }
}

/// Moves one operator to a place on the canvas.
action move_operator(p: Project, op: Operator, x: Int, y: Int) (
    icon: .move,
    label: "Move operator",
) {
    op.x = x.clamp(0, 8192)
    op.y = y.clamp(0, 8192)
}

/// Removes one operator, and every connection it held. Every other connection holds as it did:
/// removing an operator takes only its own edges, and no other operator's ports change.
action remove_operator(p: Project, op: Operator) (icon: .trash_2, label: "Remove operator") {
    p.failure = ""
    // Its own fields are read before it goes: a removed record answers nothing.
    let node = op.node
    for held in p.connections {
        if held.from_node == node or held.to_node == node { p.connections.remove(held) }
    }
    for held in p.operators {
        if held.node == node { p.operators.remove(held) }
    }
    if p.node == node {
        p.node = ""
    }
}

/// Connects one output to one input. The device refuses a connection whose types differ, whose
/// input another connection holds, or that would close a cycle.
action connect(p: Project, from: Operator, from_port: Text, to: Operator, to_port: Text) (
    icon: .git_branch,
    label: "Connect operators",
) {
    p.failure = ""
    if p.connections.len() >= 256 {
        p.failure = "The operator edge limit of this design is reached"
        return
    }
    let candidate = EdgeAsk(
        from_node: from.node,
        from_port: from_port,
        to_node: to.node,
        to_port: to_port,
    )
    if p.connections.any(c -> same_edge(c, candidate)) {
        return
    }
    let asked = graph_ask(p, [], [candidate])
    match engine.call("graph", asked) {
        .err(why) => {
            p.failure = why
        }
        .ok(answer) => {
            match outcome_of(answer, candidate) {
                .none => {
                    p.failure = "The device answered no connection for that ask"
                }
                .some(outcome) => {
                    if not outcome.active {
                        p.failure = refusal_words(outcome.reason)
                        return
                    }
                    p.connections.insert(
                        Connection(
                            from_node: from.node,
                            from_port: from_port,
                            to_node: to.node,
                            to_port: to_port,
                            digest: outcome.digest,
                        ),
                    )
                    apply_graph(p, answer)
                }
            }
        }
    }
}

/// Disconnects every connection into one input.
action disconnect(p: Project, op: Operator, port: Text) (icon: .unlink, label: "Disconnect input") {
    p.failure = ""
    if not op.inputs.any(input -> input.key == port) {
        p.failure = "This input port no longer exists"
        return
    }
    for held in p.connections {
        if held.to_node == op.node and held.to_port == port { p.connections.remove(held) }
    }
}

/// Resolves the whole design again, so what the window shows is what the stored design makes.
action resolve_graph(p: Project) {
    match engine.call("graph", graph_ask(p, [], [])) {
        .err(why) => {
            p.failure = why
        }
        .ok(answer) => apply_graph(p, answer),
    }
}

// ---------------------------------------------------------------------------
// The engine's answers, read into the project
// ---------------------------------------------------------------------------
type OutcomeOf {
    digest: Text
    active: Bool
    reason: Text
}

/// The engine's ask for the design `p` holds, with `extra` connections under consideration.
fn graph_ask(p: Project, extra_nodes: List<NodeAsk>, extra_edges: List<EdgeAsk>) -> Text {
    json.encode(
        GraphAsk(
            nodes: p.operators.map(op -> node_ask(op)) + extra_nodes,
            connections: p.connections.map(c -> edge_ask(c)) + extra_edges,
        ),
    )
}

/// The engine's ask for the design `p` holds, with one operator's shape or width changed.
fn graph_ask_without(p: Project, op: Operator, shape: List<Int>, width: Int) -> Text {
    json.encode(
        GraphAsk(
            nodes: p.operators.map(
                other -> if other.node == op.node {
                    NodeAsk(
                        id: other.node,
                        kind: other.kind,
                        shape: if shape.is_empty() { other.shape } else { shape },
                        output_width: if width > 0 {
                            width
                        } else if other.has_output_width { other.output_width } else { 0 },
                        x: other.x,
                        y: other.y,
                    )
                } else { node_ask(other) },
            ),
            connections: p.connections.map(c -> edge_ask(c)),
        ),
    )
}

fn node_ask(op: Operator) -> NodeAsk {
    NodeAsk(
        id: op.node,
        kind: op.kind,
        shape: op.shape,
        output_width: if op.has_output_width { op.output_width } else { 0 },
        x: op.x,
        y: op.y,
    )
}

fn edge_ask(c: Connection) -> EdgeAsk {
    EdgeAsk(from_node: c.from_node, from_port: c.from_port, to_node: c.to_node, to_port: c.to_port)
}

fn same_edge(c: Connection, e: EdgeAsk) -> Bool {
    c.from_node == e.from_node
        and c.from_port == e.from_port
        and c.to_node == e.to_node
        and c.to_port == e.to_port
}

/// What the engine answered for one candidate connection.
fn outcome_of(answer: Text, candidate: EdgeAsk) -> Option<OutcomeOf> {
    match json.parse(answer) {
        .err(why) => .none,
        .ok(value) => {
            for row in std.json_list(value, "active") {
                if row_matches(row, candidate) {
                    return .some(
                        OutcomeOf(digest: std.json_text(row, "id"), active: true, reason: ""),
                    )
                }
            }
            for row in std.json_list(value, "rejected") {
                if row_matches(row, candidate) {
                    return .some(
                        OutcomeOf(
                            digest: std.json_text(row, "id"),
                            active: false,
                            reason: std.json_text(row, "reason"),
                        ),
                    )
                }
            }
            .none
        }
    }
}

fn row_matches(row: json.Value, candidate: EdgeAsk) -> Bool {
    std.json_text(row, "from_node") == candidate.from_node
        and std.json_text(row, "from_port") == candidate.from_port
        and std.json_text(row, "to_node") == candidate.to_node
        and std.json_text(row, "to_port") == candidate.to_port
}

/// Keeps the ports and connection outcomes the device answered, so the window draws and
/// refuses by the design's own resolution.
action apply_graph(p: Project, answer: Text) {
    match json.parse(answer) {
        .err(why) => {
            p.failure = why
        }
        .ok(value) => {
            let nodes = std.json_list(value, "nodes")
            for op in p.operators {
                for held in op.inputs {
                    op.inputs.remove(held)
                }
                for held in op.outputs {
                    op.outputs.remove(held)
                }
                for node in nodes.filter(n -> std.json_text(n, "id") == op.node) {
                    for port in std.json_list(node, "inputs") {
                        op.inputs.insert(
                            Port(
                                key: std.json_text(port, "id"),
                                data_type: std.json_text(port, "data_type"),
                            ),
                        )
                    }
                    for port in std.json_list(node, "outputs") {
                        op.outputs.insert(
                            Port(
                                key: std.json_text(port, "id"),
                                data_type: std.json_text(port, "data_type"),
                            ),
                        )
                    }
                }
            }
            let rejected = std.json_list(value, "rejected")
            for connection in p.connections {
                connection.active = true
                connection.reason = ""
                for row in rejected {
                    if connection_matches(connection, row) {
                        connection.active = false
                        connection.reason = std.json_text(row, "reason")
                    }
                }
            }
        }
    }
}

fn connection_matches(c: Connection, row: json.Value) -> Bool {
    c.from_node == std.json_text(row, "from_node")
        and c.from_port == std.json_text(row, "from_port")
        and c.to_node == std.json_text(row, "to_node")
        and c.to_port == std.json_text(row, "to_port")
}

/// What a refused connection says, in a person's words.
// ---------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------
fn refusal_words(reason: Text) -> Text {
    if reason == "unknown_node" {
        "An endpoint no longer exists"
    } else if reason == "unknown_port" {
        "A port no longer exists"
    } else if reason == "type_mismatch" {
        "Port shapes differ"
    } else if reason == "input_occupied" {
        "Another connection occupies this input"
    } else if reason == "cycle" {
        "This connection would create a cycle"
    } else { "The device refused this connection" }
}

fn kind_words(kind: OperatorKind) -> Text {
    if kind == .input {
        "Input"
    } else if kind == .gate_input {
        "Gate input"
    } else if kind == .norm_weight_input {
        "Normalization weights"
    } else if kind == .rms_norm {
        "RMS normalization"
    } else if kind == .apply_norm_weight {
        "Apply normalization weights"
    } else if kind == .silu {
        "SiLU activation"
    } else if kind == .multiply {
        "Multiply"
    } else if kind == .add {
        "Sum"
    } else if kind == .projection_input {
        "Projection input"
    } else if kind == .matrix_weight_input { "Matrix weights" } else { "Linear projection" }
}

/// The operator kind a menu label names, and `input` for a label no operator has.
fn kind_named(label: Text) -> OperatorKind {
    let kinds = [
        OperatorKind.input,
        OperatorKind.gate_input,
        OperatorKind.norm_weight_input,
        OperatorKind.rms_norm,
        OperatorKind.apply_norm_weight,
        OperatorKind.silu,
        OperatorKind.multiply,
        OperatorKind.add,
        OperatorKind.projection_input,
        OperatorKind.matrix_weight_input,
        OperatorKind.linear_projection,
    ]
    if let found = kinds.find(kind -> kind_words(kind) == label) { found } else { .input }
}

fn kind_names() -> List<Text> {
    [
        OperatorKind.input,
        OperatorKind.gate_input,
        OperatorKind.norm_weight_input,
        OperatorKind.rms_norm,
        OperatorKind.apply_norm_weight,
        OperatorKind.silu,
        OperatorKind.multiply,
        OperatorKind.add,
        OperatorKind.projection_input,
        OperatorKind.matrix_weight_input,
        OperatorKind.linear_projection,
    ]
        .map(kind -> kind_words(kind))
}

fn port_words(port: Text) -> Text {
    if port == "value" {
        "Value"
    } else if port == "weights" {
        "Weights"
    } else if port == "result" {
        "Result"
    } else if port == "left" {
        "Left operand"
    } else if port == "right" { "Right operand" } else { port }
}

fn status_words(status: RunResult) -> Text {
    if status == .failed {
        "Reported failed"
    } else if status == .cancelled { "Reported cancelled" } else { "Reported passed" }
}

fn check_words(result: CheckResult) -> Text {
    if result == .failed { "Reported failed" } else { "Reported passed" }
}

fn stage_words(kind: StageKind) -> Text {
    if kind == .numerical {
        "Numerical verification"
    } else if kind == .fault {
        "Fault controls"
    } else if kind == .synthesis {
        "Generic synthesis"
    } else if kind == .aggregate { "Linked comparison" } else { "Preparation" }
}

fn dependency_words(kind: DependencyKind) -> Text {
    if kind == .build {
        "Build dependency"
    } else if kind == .verification { "Verification dependency" } else { "Data dependency" }
}

fn observation_words(kind: ObservationKind) -> Text {
    if kind == .cpu_reference {
        "CPU reference"
    } else if kind == .rtl_simulation {
        "RTL simulation"
    } else if kind == .generic_synthesis {
        "Generic synthesis"
    } else if kind == .library_physical {
        "Extracted library estimate"
    } else { "No numerical observations reported" }
}

fn shape_said(shape: List<Int>) -> Text {
    "[{shape.map(n -> "{n}").join(", ")}]"
}

/// The evidence class's own disclaimer, so a person never reads a claim as a measurement.
fn receipt_disclaimer(r: Receipt) -> Text {
    if r.flow {
        "Reported evidence only. Artifacts are not authenticated or executed. Library estimates are not silicon measurements, signoff, manufacturing approval, or model quality."
    } else if r.study {
        "Producer-reported architecture comparison. Stage links and artifact identities are unverified here. Cycles and generic cells establish no clock speed, physical area, energy or coding quality."
    } else if r.operator {
        "Imported reported CPU numerical observations. Artifact identity and authenticity are unverified here; this establishes no full-model execution, model quality, RTL or physical PPA."
    } else if r.evidence == "research_library_checkpoint_tile" {
        "Imported reported research-library mapping evidence. Artifact identity and authenticity are unverified here; this establishes no manufacturable layout, timing closure or model quality."
    } else {
        "Imported reported integer hardware evidence. Artifact identity and authenticity are unverified here; this establishes no full-chip PPA or model quality."
    }
}

/// The one-line reading of a numerical observation.
fn observation_said(o: Observation) -> Text {
    if o.kind == .cpu_reference {
        "{o.compared_words} words compared, {o.differing_words} differing"
    } else if o.kind == .rtl_simulation {
        "{o.accepted_inputs} accepted inputs, {o.accepted_outputs} outputs, {o.cycles} cycles"
    } else if o.kind == .generic_synthesis {
        "{o.logic_cells} logic cells, {o.logical_memory_bits} logical memory bits"
    } else if o.kind == .library_physical {
        "{o.library}: {o.routed_signal_nets} routed nets, {o.router_violations} router violations"
    } else { "" }
}
