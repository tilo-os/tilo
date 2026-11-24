/// A chip design project's window: the structural operator graph, or the engineering evidence the
/// project imported.
view window(p: Project) {
    ui.NavShell(
        content_kind: if p.selected == "graph" { .canvas } else { .page },
        title: window_title(p),
        sidebar_title: "Chip Design",
    ) {
        sidebar_actions: {
            ui.Button("Operator graph", icon: .git_branch, icon_only: true) {
                on press: show_graph(p)
            }
            ui.Button("Import report", icon: .file_plus, icon_only: true) {
                on press: file.choose(import_file(p), accept: ["application/json"])
            }
        }
        sidebar: {
            places(p)
        }
        actions: {
            ui.Button("Operator graph", icon: .git_branch) {
                on press: show_graph(p)
            }
            ui.Button("All reports", icon: .list) {
                on press: show_all(p)
            }
            ui.Button("Import report", icon: .file_plus) {
                on press: file.choose(import_file(p), accept: ["application/json"])
            }
        }
        content: {
            stack(gap: .related) {
                if p.failure != "" {
                    ui.Banner(
                        "That could not be done",
                        message: p.failure,
                        kind: .warning,
                        action: "Dismiss",
                    ) {
                        on act: dismiss(p)
                    }
                }
                if p.notice != "" {
                    ui.Banner(p.notice, kind: .success, action: "Dismiss") {
                        on act: dismiss(p)
                    }
                }
                if p.selected == "graph" {
                    graph_body(p)
                } else if let r = shown_receipt(p) {
                    receipt_body(p, r)
                } else if let r = shown_report(p) {
                    report_body(r)
                } else {
                    evidence_list(p)
                }
            }
        }
        inspector: {
            if p.selected == "graph" {
                if let op = chosen_operator(p) {
                    operator_body(p, op)
                }
            }
        }
    }
}

/// The places down the side: the graph, everything imported, and each report and receipt.
view places(p: Project) {
    ui.NavList(selected: p.selected) {
        ui.NavItem("graph", "Operator graph", icon: .git_branch, count: p.operators.len()) {
            on select: show_graph(p)
        }
        ui.NavItem(
            "all",
            "All reports",
            icon: .list,
            count: p.reports.len() + p.verifications.len(),
        ) {
            on select: show_all(p)
        }
        if not p.reports.is_empty() {
            ui.NavSection("Reports", collapsible: true) {
                for r in p.reports.sort_by(r -> r.model) {
                    ui.NavItem("report:{r.digest}", r.model, icon: .file_text, item: r) {
                        on select: inspect_report(p, r)
                    }
                }
            }
        }
        if not p.verifications.is_empty() {
            ui.NavSection("Verifications", collapsible: true) {
                for v in p.verifications.sort_by(v -> receipt_name(v)) {
                    ui.NavItem(
                        "receipt:{v.digest}",
                        receipt_name(v),
                        icon: if v.status == .failed { .triangle_alert } else { .shield_check },
                        item: v,
                    ) {
                        on select: inspect_receipt(p, v)
                    }
                }
            }
        }
    }
}

fn window_title(p: Project) -> Text {
    if p.selected == "graph" {
        "Operator graph"
    } else if let r = shown_report(p) {
        r.model
    } else if let v = shown_receipt(p) { receipt_name(v) } else { "Chip Design" }
}

fn receipt_name(r: Receipt) -> Text {
    if r.flow and r.flow_title != "" { r.flow_title } else { r.evidence }
}

/// The report the window shows, by the digest it is published under.
fn shown_report(p: Project) -> Option<Report> {
    if p.subject == "" {
        .none
    } else {
        p.reports.find(r -> r.digest == p.subject)
    }
}

/// The verification receipt the window shows, by the digest it is published under.
fn shown_receipt(p: Project) -> Option<Receipt> {
    if p.subject == "" {
        .none
    } else {
        p.verifications.find(r -> r.digest == p.subject)
    }
}

fn chosen_operator(p: Project) -> Option<Operator> {
    if p.node == "" {
        .none
    } else {
        p.operators.find(op -> op.node == p.node)
    }
}

/// The stage of `r` the window holds open, when the person opened one.
fn chosen_stage(p: Project, r: Receipt) -> Option<Stage> {
    if p.stage == "" { .none } else { r.stages.find(s -> s.key == p.stage) }
}

// ---------------------------------------------------------------------------
// Everything the project holds
// ---------------------------------------------------------------------------
view evidence_list(p: Project) {
    if p.reports.is_empty() and p.verifications.is_empty() and p.operators.is_empty() {
        ui.EmptyState(
            "Nothing in this project yet",
            message: "Sketch the operators in the operator graph, or import an engineering report (JSON) to read the evidence it records.",
            icon: .cpu,
            action: "Open the operator graph",
        ) {
            on act: show_graph(p)
        }
    } else {
        ui.StatGroup {
            ui.StatTile("Reports", "{p.reports.len()}")
            ui.StatTile("Verifications", "{p.verifications.len()}")
            ui.StatTile("Operators", "{p.operators.len()}")
        }
        if not p.reports.is_empty() {
            ui.Section(title: "Reports") {
                for r in p.reports.sort_by(r -> r.model) {
                    ui.ListRow(
                        title: r.model,
                        secondary: r.revision,
                        note: r.evidence,
                        icon: .file_text,
                        item: r,
                    ) {
                        on activate: inspect_report(p, r)
                    }
                }
            }
        }
        if not p.verifications.is_empty() {
            ui.Section(title: "Verifications") {
                for v in p.verifications.sort_by(v -> receipt_name(v)) {
                    ui.ListRow(
                        title: receipt_name(v),
                        secondary: v.evidence,
                        meta: status_words(v.status),
                        meta_tone: if v.status == .failed { .danger } else { .success },
                        icon: if v.status == .failed { .triangle_alert } else { .shield_check },
                        item: v,
                    ) {
                        on activate: inspect_receipt(p, v)
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// One analytical storage report
// ---------------------------------------------------------------------------
view report_page(r: Report) {
    std.document(r) {
        report_body(r)
    }
}

template view report_body(r: Report) {
    ui.ObjectHeader(title: r.model, caption: r.revision, icon: .file_text)
    ui.Text(
        "Imported analytical storage report. Source authenticity and quantization quality are unverified.",
        role: .caption,
        secondary: true,
    )
    ui.Section(title: "Reported provenance") {
        ui.PropertyList {
            ui.Property("Model repository", r.model)
            ui.Property("Reported model revision", r.revision)
            ui.Property("Reported config SHA-256", r.config_sha256)
            ui.Property("Imported bytes BLAKE3", r.digest)
        }
    }
    ui.Section(title: "Storage assumptions") {
        ui.PropertyList {
            ui.Property("Assumed logical parameters", "{r.assumptions.logical_parameters}")
            ui.Property("Assumed weight bits", "{r.assumptions.weight_bits}")
            ui.Property("Assumed cache bits", "{r.assumptions.kv_bits}")
            ui.Property("Assumed context tokens", "{r.assumptions.context_tokens}")
            ui.Property("Batch size", "{r.batch_size}")
        }
    }
    ui.Section(title: "Analytical storage") {
        ui.PropertyList {
            ui.Property("Packed weight bytes", "{r.packed_weight_bytes}")
            if r.cache == .unsupported {
                ui.Property(
                    "Cache storage unverified",
                    if r.cache_reason == "" { "Not reported" } else { r.cache_reason },
                )
            } else {
                ui.Property("Reported full-attention layers", "{r.attention_layers}")
                ui.Property("Reported cache elements", "{r.kv_elements}")
                ui.Property("Partial cache bytes", "{r.kv_bytes}")
                for excluded in r.cache_excludes {
                    ui.Property("Excluded from cache", excluded)
                }
            }
        }
    }
    ui.Section(title: "Exclusions and limitations") {
        ui.PropertyList {
            for item in r.not_estimated {
                ui.Property("Unestimated", item)
            }
            for item in r.limitations {
                ui.Property("Report limitation", item)
            }
            ui.Property(
                "Verification scope",
                "Internal arithmetic only. The pinned config is not reconstructed; hardware performance and coding quality are not measured.",
            )
        }
    }
    ui.Button("Remove report", icon: .trash_2, role: .destructive) {
        on press: remove_report(project, r)
    }
}

// ---------------------------------------------------------------------------
// One verification receipt
// ---------------------------------------------------------------------------
view receipt_page(r: Receipt) {
    std.document(r) {
        receipt_body(project, r)
    }
}

template view receipt_body(p: Project, r: Receipt) {
    ui.ObjectHeader(title: receipt_name(r), caption: r.evidence, icon: .shield_check)
    ui.Text(receipt_disclaimer(r), role: .caption, secondary: true)
    receipt_scope(r)
    ui.Section(title: checks_label(r)) {
        if r.checks.is_empty() {
            ui.Text("Nothing in this project yet", role: .caption, secondary: true)
        } else {
            ui.PropertyList {
                for check in r.checks.sort_by(c -> c.name) {
                    ui.Property(check.name, check_words(check.result))
                }
            }
        }
    }
    receipt_provenance(r)
    ui.Section(title: "Reported output artifact identities") {
        if r.artifacts.is_empty() {
            ui.Text("Not reported", role: .caption, secondary: true)
        } else {
            ui.PropertyList {
                for artifact in r.artifacts.sort_by(a -> a.name) {
                    ui.Property(artifact.name, artifact.sha256)
                }
            }
        }
    }
    ui.Section(title: "Exclusions and limitations") {
        ui.PropertyList {
            for exclusion in r.excludes {
                ui.Property("Verification exclusion", exclusion)
            }
        }
    }
    if r.operator {
        ui.Section(title: "Reported numerical observations") {
            for observation in r.observations {
                observation_row(observation)
            }
        }
    }
    if r.study {
        study_page(r)
    }
    if r.flow {
        flow_body(p, r)
    }
    ui.Button("Remove verification receipt", icon: .trash_2, role: .destructive) {
        on press: remove_verification(p, r)
    }
}

fn checks_label(r: Receipt) -> Text {
    if r.study {
        "Producer-reported stage checks"
    } else if r.operator { "Reported numerical checks" } else { "Reported integer checks" }
}

view receipt_scope(r: Receipt) {
    ui.Section(title: "Reported verification scope") {
        ui.PropertyList {
            ui.Property("Reported run result", status_words(r.status))
            ui.Property("Evidence class", r.evidence)
            if r.failure != "" {
                ui.Property("Failure explanation", r.failure)
            }
            if r.failure_stage != "" {
                ui.Property("Failure stage", r.failure_stage)
            }
            if r.has_input_validation {
                ui.Property("Reported input validation", check_words(r.input_validation))
            }
            if r.has_lanes {
                ui.Property("Output lanes", "{r.lanes}")
            }
            if r.has_group_size {
                ui.Property("Activation group size", "{r.group_size}")
            }
            if r.has_mapped_cell_count {
                ui.Property("Reported mapped cell count", "{r.mapped_cell_count}")
            }
            if r.reported_library_area != "" {
                ui.Property("Sum of research library cell areas", r.reported_library_area)
            }
            if r.has_abc_target {
                ui.Property(
                    "ABC optimization target (ps), not measured timing",
                    "{r.abc_delay_target_ps}",
                )
            }
            if r.has_manufacturable and not r.manufacturable {
                ui.Property("Process scope", "Nonmanufacturable research library")
            }
            if r.has_external_scalar_comparisons {
                ui.Property("External scalar comparisons", "{r.external_scalar_comparisons}")
            }
            if r.real_protocol != "" {
                ui.Property("Protocol observation", r.real_protocol)
            }
            if r.c_instrumentation != "" {
                ui.Property("C reference instrumentation", r.c_instrumentation)
            }
            if not r.compiler_flags.is_empty() {
                ui.Property("Scalar compiler flags", r.compiler_flags.join(" "))
            }
            if r.has_generic_cell_count {
                ui.Property("Reported generic cell count", "{r.generic_cell_count}")
            }
            if r.has_excluded_metadata_cells {
                ui.Property("Excluded metadata cells", "{r.excluded_metadata_cells}")
            }
            if r.study {
                ui.Property("Reported experiment stage", stage_words(r.study_stage))
                ui.Property(
                    "Declared complete output words per variant",
                    "{r.study_expected_comparison_words}",
                )
            }
            if r.operator {
                for observation in r.observations {
                    ui.Property("Reported numerical observation", observation_said(observation))
                }
            }
        }
    }
}

view receipt_provenance(r: Receipt) {
    ui.Section(title: "Reported provenance") {
        ui.PropertyList {
            ui.Property("Original imported bytes BLAKE3", r.digest)
            ui.Property(
                "Imported bytes SHA-256",
                if r.imported_bytes_sha256 == "" {
                    "Not reported"
                } else { r.imported_bytes_sha256 },
            )
            ui.Property(
                "Declared immutable toolchain image",
                if r.declared_toolchain_image == "" {
                    "Not reported"
                } else { r.declared_toolchain_image },
            )
            if r.sample_manifest_sha256 != "" {
                ui.Property("Reported sample manifest SHA-256", r.sample_manifest_sha256)
            }
            if r.verifier_sha256 != "" {
                ui.Property("Reported verifier SHA-256", r.verifier_sha256)
            }
            if r.artifact_validator_sha256 != "" {
                ui.Property("Reported artifact validator SHA-256", r.artifact_validator_sha256)
            }
            if r.mapper_sha256 != "" {
                ui.Property("Reported mapper SHA-256", r.mapper_sha256)
            }
            if r.library_revision != "" {
                ui.Property("Reported library source revision", r.library_revision)
            }
            if r.sample_sha256 != "" {
                ui.Property("Reported sample digest", r.sample_sha256)
            }
            if r.sample_receipt_sha256 != "" {
                ui.Property("Reported sample receipt digest", r.sample_receipt_sha256)
            }
            if r.rtl_source_sha256 != "" {
                ui.Property("Reported RTL source digest", r.rtl_source_sha256)
            }
            for entry in r.input_digests.sort_by(d -> d.name) {
                ui.Property(entry.name, entry.sha256)
            }
            for entry in r.primary_sources.sort_by(d -> d.name) {
                ui.Property(entry.name, entry.sha256)
            }
            for entry in r.sources.sort_by(d -> d.name) {
                ui.Property(entry.name, entry.sha256)
            }
            for cell in r.generic_cells.sort_by(c -> c.name) {
                ui.Property(cell.name, "{cell.count}")
            }
            if r.operator {
                ui.Property("Original receipt SHA-256", r.op_original_receipt_sha256)
                ui.Property("Reported primary source revision", r.op_primary_revision)
                ui.Property("Reported compiler version", r.op_compiler_version)
                ui.Property("Reported tool executable SHA-256", r.op_compiler_executable_sha256)
                ui.Property("Reported compiler flags", r.op_compiler_flags.join(" "))
                for entry in r.op_primary_sources.sort_by(d -> d.name) {
                    ui.Property(entry.name, entry.sha256)
                }
                for entry in r.op_local_sources.sort_by(d -> d.name) {
                    ui.Property(entry.name, entry.sha256)
                }
                for entry in r.op_libraries.sort_by(d -> d.name) {
                    ui.Property(entry.name, entry.sha256)
                }
            }
            if r.study {
                ui.Property("Reported experiment stage", stage_words(r.study_stage))
                ui.Property("Original producer receipt SHA-256", r.study_original_receipt_sha256)
                ui.Property("Executed producer SHA-256", r.study_producer_sha256)
                ui.Property("Shared input identity SHA-256", r.study_input_identity_sha256)
            }
            for tool in r.tools.sort_by(t -> t.name) {
                ui.Property(tool.name, tool.version)
                ui.Property("Reported tool executable SHA-256", tool.executable_sha256)
            }
            if r.tools.is_empty() and not r.operator {
                ui.Property("Tool identities", "Not reported")
            }
        }
    }
}

/// One numerical observation, written out as the report states it.
view observation_row(o: Observation) {
    if o.kind == .rms_norm {
        ui.Property(
            "Reported numerical observation",
            "{o.profile}: {o.fp32_comparisons} FP32 words compared exactly",
        )
    } else if o.kind == .silu {
        ui.Property(
            "Reported numerical observation",
            "{o.total_inputs} inputs: SiLU compared exactly; exp has {o.exp_differences} differences, maximum {o.max_exp_ulp} ULP",
        )
    } else if o.kind == .gated_delta {
        ui.Property(
            "Reported numerical observation",
            "{o.heads} heads, {o.tokens} tokens, width {o.width}: {o.attention_words} attention and {o.state_words} state words compared exactly",
        )
    } else if o.kind == .softmax {
        ui.Property(
            "Reported numerical observation",
            "{o.name}: {o.width} words, {o.masked} masked; {o.differing_words} differences, maximum {o.max_ulp} ULP",
        )
    }
}

// ---------------------------------------------------------------------------
// A reported architecture comparison
// ---------------------------------------------------------------------------
view study_page(r: Receipt) {
    ui.Section(title: "Stage links") {
        ui.PropertyList {
            ui.Property("Original producer receipt SHA-256", r.study_original_receipt_sha256)
            ui.Property("Executed producer SHA-256", r.study_producer_sha256)
            ui.Property("Shared input identity SHA-256", r.study_input_identity_sha256)
            for link in r.links.sort_by(l -> l.name) {
                ui.Property(stage_words(stage_of(link.name)), link.receipt_sha256)
                ui.Property("Executed producer SHA-256", link.producer_sha256)
            }
        }
    }
    for variant in r.variants.sort_by(v -> v.identity) {
        ui.Section(title: variant.identity) {
            ui.PropertyList {
                if variant.numerical {
                    ui.Property("Cold weight programming cycles", "{variant.cold_program_cycles}")
                    ui.Property("Activation load cycles", "{variant.vector_load_cycles}")
                    ui.Property("Warm execution cycles", "{variant.warm_cycles}")
                    ui.Property("Accepted packed weight bytes", "{variant.accepted_weight_bytes}")
                    ui.Property(
                        "Accepted block descriptor bytes",
                        "{variant.accepted_descriptor_bytes}",
                    )
                    ui.Property("Activation cache reads", "{variant.cache_reads}")
                    ui.Property("Quantizer packets", "{variant.quantizer_packets}")
                    ui.Property("Compared output words", "{variant.comparison_words}")
                    ui.Property("Reported output SHA-256", variant.output_sha256)
                }
                if variant.synthesis {
                    ui.Property("Generic logic cells", "{variant.generic_logic_cells}")
                    ui.Property("Activation cache bits", "{variant.activation_cache_bits}")
                    ui.Property(
                        "Programmed weight storage bits",
                        "{variant.programmed_weight_bits}",
                    )
                    ui.Property(
                        "Initialized fixed storage bits",
                        "{variant.fixed_initialized_bits}",
                    )
                }
            }
            for memory in variant.memories.sort_by(m -> m.name) {
                ui.Section(title: "{memory.name} of {variant.identity}") {
                    ui.PropertyList {
                        ui.Property("Word width (bits)", "{memory.width_bits}")
                        ui.Property("Depth (words)", "{memory.depth_words}")
                        ui.Property("Logical storage (bits)", "{memory.bits}")
                        ui.Property("Read ports", "{memory.read_ports}")
                        ui.Property("Clocked read ports", "{memory.clocked_read_ports}")
                        ui.Property("Write ports", "{memory.write_ports}")
                        ui.Property(
                            "Fully initialized",
                            if memory.fully_initialized { "Yes" } else { "No" },
                        )
                    }
                }
            }
        }
    }
}

fn stage_of(name: Text) -> StageKind {
    stage_kind_of(name)
}

// ---------------------------------------------------------------------------
// The reported evidence flow and its placements
// ---------------------------------------------------------------------------
view flow_body(p: Project, r: Receipt) {
    ui.Section(title: "Reported evidence flow") {
        ui.ObjectHeader(title: r.flow_title, caption: "Reported evidence only", icon: .git_branch)
        ui.PropertyList {
            ui.Property("Declared input identity SHA-256", r.flow_input_identity_sha256)
            for tool in r.tools.sort_by(t -> t.name) {
                ui.Property(
                    tool.name,
                    "Version {tool.version}, executable {tool.executable_sha256}",
                )
            }
        }
        ui.GraphCanvas(
            title: .some(r.flow_title),
            columns: .some(4),
            column_width: .some(280.0),
            connections: evidence_connections(r),
        ) {
            for stage in r.stages.sort_by(s -> s.key) {
                ui.GraphNode(stage.key, width: .some(260.0)) {
                    evidence_stage_card(p, r, stage)
                }
            }
            for link in r.subflows.sort_by(s -> s.key) {
                ui.GraphNode(link.key, width: .some(260.0)) {
                    evidence_subflow_card(p, r, link)
                }
            }
        }
        ui.Text(
            "Reported evidence only. Artifacts are not authenticated or executed. Library estimates are not silicon measurements, signoff, manufacturing approval, or model quality.",
            role: .caption,
            secondary: true,
        )
    }
    if let stage = chosen_stage(p, r) {
        stage_body(p, .some(r), stage)
    }
    ui.Section(title: "Original artifact references") {
        for artifact in r.artifacts.sort_by(a -> a.name) {
            ui.ListRow(title: artifact.name, secondary: artifact.sha256, dense: true)
        }
    }
}

view evidence_stage_card(p: Project, r: Receipt, s: Stage) {
    ui.ListRow(
        title: s.title,
        secondary: observation_words(s.observed),
        meta: status_words(s.status),
        meta_tone: if s.status == .failed { .danger } else { .success },
        selected: p.stage == s.key,
    ) {
        on activate: inspect_stage(p, r, s)
    }
}

view evidence_subflow_card(p: Project, r: Receipt, s: Subflow) {
    ui.ListRow(title: s.title, secondary: "Subflow reference", icon: .git_branch) {
        on activate: open_subflow(p, r, s)
    }
}

fn evidence_connections(r: Receipt) -> List<ui.Connection> {
    r.dependencies.map(d -> ui.Connection.connection(from: d.from, to: d.to))
}

// ---------------------------------------------------------------------------
// One stage of a reported flow
// ---------------------------------------------------------------------------
view stage_page(s: Stage) {
    std.document(s) {
        stage_body(project, receipt_of_stage(project, s), s)
    }
}

/// The receipt that holds a stage, when the project still holds it.
fn receipt_of_stage(p: Project, s: Stage) -> Option<Receipt> {
    p.verifications.find(r -> r.stages.any(held -> held.key == s.key))
}

template view stage_body(p: Project, r: Option<Receipt>, s: Stage) {
    ui.ObjectHeader(title: s.title, caption: s.key, icon: .git_branch)
    ui.PropertyList {
        ui.Property("Reported stage result", status_words(s.status))
        if s.observed != .none {
            ui.Property("Observation", observation_words(s.observed))
        }
        ui.Property("Original receipt artifact", s.original_receipt_artifact)
        ui.Property("Declared producer SHA-256", s.producer_sha256)
        ui.Property("Declared input identity SHA-256", s.input_identity_sha256)
        for check in s.checks.sort_by(c -> c.name) {
            ui.Property(check.name, check_words(check.result))
        }
        for limitation in s.limitations {
            ui.Property("Scope limitation", limitation)
        }
        if let held = r {
            for edge in held.dependencies.filter(d -> d.from == s.key or d.to == s.key) {
                ui.Property(
                    "Declared dependency",
                    "{edge.from} feeds {edge.to}, {dependency_words(edge.kind)}, artifact {edge.artifact}",
                )
            }
        }
    }
    if s.placed {
        placement_body(p, r, s)
    }
}

// ---------------------------------------------------------------------------
// A reported macro placement
// ---------------------------------------------------------------------------
view macro_page(m: Macro) {
    std.document(m) {
        ui.ObjectHeader(title: m.name, caption: m.master, icon: .square)
        macro_properties(m, units_of(project, m))
    }
}

/// The units a macro's bounds are read in: those of the placement stage that holds it.
fn units_of(p: Project, m: Macro) -> Int {
    for receipt in p.verifications {
        for stage in receipt.stages {
            if stage.macros.any(held -> held.slot == m.slot) {
                return stage.dbu_per_micron
            }
        }
    }
    1
}

template view placement_body(p: Project, r: Option<Receipt>, s: Stage) {
    ui.Section(title: "Reported macro placement") {
        ui.Text(
            "Macro footprints only; other instances are omitted. Reported geometry does not establish layout authentication or signoff. Coordinates are in micrometers, with the physical origin at the lower left.",
            role: .caption,
            secondary: true,
        )
        ui.PropertyList {
            ui.Property("Total reported instances", "{s.total_instances}")
            ui.Property("Instances omitted from this view", "{s.omitted_instances}")
            ui.Property(
                "Die bounds (µm)",
                "{microns(s.die_left, s.dbu_per_micron)}, {microns(s.die_bottom, s.dbu_per_micron)}, {microns(s.die_right, s.dbu_per_micron)}, {microns(s.die_top, s.dbu_per_micron)}",
            )
        }
        ui.Drawing(
            placement_shapes(s, p.macro),
            meaning: .chart,
            label: .some("Reported macro placement"),
            height: .some(320.0),
            motion: .calm,
        ) {
            on press(i): tap_macro(p, s, i)
        }
        for item in s.macros.sort_by(m -> m.name) {
            ui.ListRow(
                title: item.name,
                secondary: item.master,
                meta: item.placement,
                selected: p.macro == item.slot,
            ) {
                on activate: inspect_seen_macro(p, r, s, item)
            }
            if p.macro == item.slot {
                macro_properties(item, s.dbu_per_micron)
            }
        }
    }
}

/// Shows one macro the person picked from the placement's own list.
action inspect_seen_macro(p: Project, r: Option<Receipt>, s: Stage, m: Macro) {
    if let held = r { inspect_macro(p, held, m) }
}

template view macro_properties(m: Macro, per_micron: Int) {
    ui.PropertyList {
        ui.Property("Cell master", m.master)
        ui.Property(
            "Macro bounds (µm)",
            "{microns(m.left, per_micron)}, {microns(m.bottom, per_micron)}, {microns(m.right, per_micron)}, {microns(m.top, per_micron)}",
        )
        ui.Property("Orientation", orientation_words(m.orientation))
        ui.Property("Placement status", m.placement)
    }
}

/// Micrometers a database unit count stands for, read as a layout reads it.
fn microns(dbu: Int, per_micron: Int) -> Text {
    if per_micron <= 0 {
        "{dbu}"
    } else {
        (dbu.to_float() / per_micron.to_float()).to_text()
    }
}

fn orientation_words(value: Orientation) -> Text {
    if value == .r90 {
        "R90"
    } else if value == .r180 {
        "R180"
    } else if value == .r270 {
        "R270"
    } else if value == .mx {
        "MX"
    } else if value == .my {
        "MY"
    } else if value == .mxr90 { "MXR90" } else if value == .myr90 { "MYR90" } else { "R0" }
}

/// The die and its reported macros as fractions of one box, top down as a layout is drawn.
/// Shape zero is the die; `index` counts the macros from one.
fn placement_shapes(s: Stage, selected: Text) -> List<ui.Shape> {
    let width = (s.die_right - s.die_left).to_float()
    let height = (s.die_top - s.die_bottom).to_float()
    let die = [
        ui.Shape.rect(x: 0.0, y: 0.0, w: 1.0, h: 1.0, stroke: .some(.track), weight: .hairline),
    ]
    let placed = s.macros
        .sort_by(m -> m.name)
        .map(
            m -> ui.Shape.rect(
                x: span(m.left - s.die_left, width),
                y: span(s.die_top - m.top, height),
                w: span(m.right - m.left, width),
                h: span(m.top - m.bottom, height),
                fill: .some(if selected == m.slot { .accent } else { .track }),
                stroke: .some(.muted),
                weight: .hairline,
            ),
        )
    die + placed
}

/// Where a database unit distance stands across a die, as a fraction of its box.
fn span(dbu: Int, extent: Float) -> Float {
    if extent <= 0.0 {
        0.0
    } else {
        (dbu.to_float() / extent).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// One reported subflow reference
// ---------------------------------------------------------------------------
view subflow_page(s: Subflow) {
    std.document(s) {
        subflow_body(project, receipt_of_subflow(project, s), s)
    }
}

fn receipt_of_subflow(p: Project, s: Subflow) -> Option<Receipt> {
    p.verifications.find(r -> r.subflows.any(held -> held.key == s.key))
}

template view subflow_body(p: Project, r: Option<Receipt>, s: Subflow) {
    ui.ObjectHeader(title: s.title, caption: "Subflow reference", icon: .git_branch)
    if let held = r {
        ui.PropertyList {
            ui.Property("Relative artifact reference", s.summary_artifact)
            ui.Property(
                "Declared artifact SHA-256",
                if let digest = artifact_digest(held, s.summary_artifact) {
                    digest
                } else { "Not reported" },
            )
        }
        if let digest = artifact_digest(held, s.summary_artifact) {
            if p.verifications.any(other -> other.flow and other.imported_bytes_sha256 == digest) {
                ui.Button("Open imported subflow", icon: .git_branch, role: .primary) {
                    on press: open_subflow(p, held, s)
                }
            } else {
                ui.Text(
                    "Referenced summary has not been imported. No execution claim is made for this reference.",
                    role: .caption,
                    secondary: true,
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// One operator's own facts
// ---------------------------------------------------------------------------
view operator_page(op: Operator) {
    std.document(op) {
        operator_body(project, op)
    }
}

template view operator_body(p: Project, op: Operator) {
    ui.ObjectHeader(title: kind_words(op.kind), caption: op.node, icon: .cpu)
    ui.PropertyList {
        ui.Property("Node identity", op.node)
        ui.Property("FP32 shape", shape_said(op.shape))
        ui.Property(
            if op.has_output_width { "Input width" } else { "Normalization width" },
            "{op.normalization_width}",
        )
        ui.Property("Position", "{op.x}, {op.y}")
        for port in op.inputs {
            ui.Property("In", "{port_words(port.key)}: {port.data_type}")
        }
        for port in op.outputs {
            ui.Property("Out", "{port_words(port.key)}: {port.data_type}")
        }
    }
    row(gap: .related, align: .end) {
        ui.TextInput(bind: op.shape_text, placeholder: shape_said(op.shape), fill)
        ui.Button("Apply shape", icon: .check) {
            on press: apply_shape(p, op)
        }
    }
    if op.has_output_width {
        row(gap: .related, align: .end) {
            ui.TextInput(bind: op.width_text, placeholder: "{op.output_width}", fill)
            ui.Button("Apply output width", icon: .check) {
                on press: apply_width(p, op)
            }
        }
    }
    row(gap: .tight) {
        ui.Button("Move left", icon: .chevron_left) {
            on press: move_operator(p, op, op.x - 32, op.y)
        }
        ui.Button("Move right", icon: .chevron_right) {
            on press: move_operator(p, op, op.x + 32, op.y)
        }
        ui.Button("Move up", icon: .chevron_up) {
            on press: move_operator(p, op, op.x, op.y - 32)
        }
        ui.Button("Move down", icon: .chevron_down) {
            on press: move_operator(p, op, op.x, op.y + 32)
        }
    }
    for connection in p.connections.filter(c -> c.to_node == op.node and c.active) {
        ui.Button("Disconnect {port_words(connection.to_port)}", icon: .unlink) {
            on press: disconnect(p, op, connection.to_port)
        }
    }
    ui.Button("Remove operator", icon: .trash_2, role: .destructive) {
        on press: remove_operator(p, op)
    }
}
// ---------------------------------------------------------------------------
// Captures
// ---------------------------------------------------------------------------
