/// A project nobody has imported into yet.
// Captures: named situations a shot or a test stages, built from the app's own records. They
// never touch a person's data; a real first launch starts empty.
scenario "nothing in this project yet" {
}

/// A structural design: an RMS-norm block wired into a gated multiply.
scenario "the operator graph" {
    let p = root Project
    p.selected = "graph"
    p.node = "rms"
    let x = p.operators.insert(
        Operator(node: "input", kind: .input, shape: [4, 8], normalization_width: 8, x: 0, y: 0),
    )
    x.outputs.insert(Port(key: "value", data_type: "f32[4, 8]"))
    let weight = p.operators.insert(
        Operator(
            node: "weight",
            kind: .norm_weight_input,
            shape: [8],
            normalization_width: 8,
            x: 280,
            y: 320,
        ),
    )
    weight.outputs.insert(Port(key: "weights", data_type: "f32[8]"))
    let rms = p.operators.insert(
        Operator(node: "rms", kind: .rms_norm, shape: [4, 8], normalization_width: 8, x: 280, y: 0),
    )
    rms.inputs.insert(Port(key: "value", data_type: "f32[4, 8]"))
    rms.outputs.insert(Port(key: "result", data_type: "f32[4, 8]"))
    let weighted = p.operators.insert(
        Operator(
            node: "weighted",
            kind: .apply_norm_weight,
            shape: [4, 8],
            normalization_width: 8,
            x: 560,
            y: 0,
        ),
    )
    weighted.inputs.insert(Port(key: "value", data_type: "f32[4, 8]"))
    weighted.inputs.insert(Port(key: "weights", data_type: "f32[8]"))
    weighted.outputs.insert(Port(key: "result", data_type: "f32[4, 8]"))
    let gate = p.operators.insert(
        Operator(
            node: "gate",
            kind: .gate_input,
            shape: [4, 8],
            normalization_width: 8,
            x: 0,
            y: 320,
        ),
    )
    gate.outputs.insert(Port(key: "value", data_type: "f32[4, 8]"))
    let silu = p.operators.insert(
        Operator(node: "silu", kind: .silu, shape: [4, 8], normalization_width: 8, x: 560, y: 320),
    )
    silu.inputs.insert(Port(key: "value", data_type: "f32[4, 8]"))
    silu.outputs.insert(Port(key: "result", data_type: "f32[4, 8]"))
    let mul = p.operators.insert(
        Operator(node: "mul", kind: .multiply, shape: [4, 8], normalization_width: 8, x: 840, y: 0),
    )
    mul.inputs.insert(Port(key: "left", data_type: "f32[4, 8]"))
    mul.inputs.insert(Port(key: "right", data_type: "f32[4, 8]"))
    mul.outputs.insert(Port(key: "result", data_type: "f32[4, 8]"))
    let out = p.operators.insert(
        Operator(
            node: "output",
            kind: .projection_input,
            shape: [4, 8],
            normalization_width: 8,
            x: 1120,
            y: 0,
        ),
    )
    out.inputs.insert(Port(key: "value", data_type: "f32[4, 8]"))
    p.connections.insert(
        Connection(
            from_node: "input",
            from_port: "value",
            to_node: "rms",
            to_port: "value",
            digest: "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a",
        ),
    )
    p.connections.insert(
        Connection(
            from_node: "rms",
            from_port: "result",
            to_node: "weighted",
            to_port: "value",
            digest: "1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b",
        ),
    )
    p.connections.insert(
        Connection(
            from_node: "weight",
            from_port: "weights",
            to_node: "weighted",
            to_port: "weights",
            digest: "2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c",
        ),
    )
    p.connections.insert(
        Connection(
            from_node: "gate",
            from_port: "value",
            to_node: "silu",
            to_port: "value",
            digest: "3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d",
        ),
    )
    p.connections.insert(
        Connection(
            from_node: "weighted",
            from_port: "result",
            to_node: "mul",
            to_port: "left",
            digest: "4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e",
        ),
    )
    p.connections.insert(
        Connection(
            from_node: "silu",
            from_port: "result",
            to_node: "mul",
            to_port: "right",
            digest: "5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f",
        ),
    )
    p.connections.insert(
        Connection(
            from_node: "mul",
            from_port: "result",
            to_node: "output",
            to_port: "value",
            digest: "6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a",
        ),
    )
    // A connection the design no longer holds: the report says why.
    p.connections.insert(
        Connection(
            from_node: "gate",
            from_port: "value",
            to_node: "mul",
            to_port: "left",
            digest: "7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b",
            active: false,
            reason: "input_occupied",
        ),
    )
}

/// A structural design with the person connecting from the RMS output.
scenario "connecting two operators" {
    let p = root Project
    p.selected = "graph"
    p.output_node = "weighted"
    p.output_port = "result"
}

/// An imported analytical storage report, as a person reads it.
scenario "a storage report" {
    let p = root Project
    let report = p.reports.insert(
        Report(
            digest: "9c1f4a05d3b6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f70",
            source: "",
            schema_version: 1,
            model: "Synthetic/Contract-Example",
            revision: "ad835591193cff5d2a231485cab35f8654c71287",
            config_sha256: "d10c076a600d7f526026e4dcde3de46b616ff4cf26d1d265e1ecf96781ef8020",
            evidence: "analytical_storage_bound",
            batch_size: 1,
            packed_weight_bytes: 512,
            cache: .bound,
            attention_layers: 2,
            kv_elements: 2048,
            kv_bytes: 4096,
            cache_excludes: [
                "recurrent_state",
                "convolution_state",
                "scales",
                "padding",
                "workspace",
                "batching",
            ],
            not_estimated: [
                "silicon_area",
                "power",
                "tokens_per_second",
                "coding_quality",
                "total_runtime_memory",
            ],
            limitations: [
                "SYNTHETIC contract example. Model, revisions, digests and all values are authored fixtures; no experiment or checkpoint is represented.",
                "Weight bound assumes uniform precision; excludes scales, padding and duplicated storage.",
                "Logical parameter count is a user assumption, not inferred from packed tensor inventory.",
                "Open-weight access does not by itself establish hardware commercialization rights.",
                "Snapshot digest checks integrity, not publisher authentication.",
                "Precision is a storage assumption; quantization quality has not been validated.",
            ],
        ),
    )
    report.assumptions.logical_parameters = 1024
    report.assumptions.weight_bits = 4
    report.assumptions.kv_bits = 16
    report.assumptions.context_tokens = 64
    p.selected = "report"
    p.subject = report.digest
}

/// An imported checkpoint verification receipt, with the checks and identities it reports.
scenario "a checkpoint verification" {
    let p = root Project
    let r = p.verifications.insert(
        Receipt(
            digest: "5d41402abc4b2a76b9719d911017c5925d41402abc4b2a76b9719d911017c592",
            source: "",
            evidence: "checkpoint_tile_integer_contract",
            status: .passed,
            imported_bytes_sha256: "",
            declared_toolchain_image: "sha256:0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0",
            sample_manifest_sha256: "aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44",
            verifier_sha256: "bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55",
            artifact_validator_sha256: "cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66",
            synthetic_activations: true,
            has_synthetic_activations: true,
            compiler_flags: [],
            excludes: [
                "full_checkpoint_integrity",
                "group_rescaling",
                "activation_quantization",
                "full_model_execution",
                "coding_quality",
                "physical_ppa",
            ],
        ),
    )
    r.checks.insert(Check(name: "generic_synthesis", result: .passed))
    r.checks.insert(Check(name: "netlist_vs_integer_oracle", result: .passed))
    r.checks.insert(Check(name: "rtl_vs_integer_oracle", result: .passed))
    r.tools.insert(
        Tool(
            name: "verilator",
            version: "Verilator 5.028",
            executable_sha256: "dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11",
        ),
    )
    r.tools.insert(
        Tool(
            name: "yosys",
            version: "Yosys 0.45",
            executable_sha256: "ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22",
        ),
    )
    r.artifacts.insert(
        Artifact(
            name: "synthesized.v",
            sha256: "ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33",
        ),
    )
    p.selected = "receipt"
    p.subject = r.digest
}

/// A reported research-library mapping, with its declared scope and observations.
scenario "a research mapping" {
    let p = root Project
    let r = p.verifications.insert(
        Receipt(
            digest: "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
            source: "",
            evidence: "research_library_checkpoint_tile",
            status: .passed,
            mapper_sha256: "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
            library_revision: "0123456789abcdef0123456789abcdef01234567",
            mapped_cell_count: 100,
            has_mapped_cell_count: true,
            reported_library_area: "123.5",
            abc_delay_target_ps: 2500,
            has_abc_target: true,
            manufacturable: false,
            has_manufacturable: true,
            compiler_flags: [],
            excludes: [
                "timing_closure",
                "physical_layout",
                "physical_power",
                "manufacturability",
                "full_model_area",
                "group_rescaling",
                "coding_quality",
                "offline_checkpoint_payload_authentication",
            ],
        ),
    )
    r.checks.insert(Check(name: "input_validation", result: .passed))
    r.checks.insert(Check(name: "library_mapping", result: .passed))
    r.checks.insert(Check(name: "mapped_netlist_vs_integer_oracle", result: .passed))
    r.tools.insert(
        Tool(
            name: "verilator",
            version: "Verilator 5.028",
            executable_sha256: "dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11",
        ),
    )
    r.tools.insert(
        Tool(
            name: "yosys",
            version: "Yosys 0.45",
            executable_sha256: "ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22",
        ),
    )
    r.artifacts.insert(
        Artifact(
            name: "mapped.v",
            sha256: "ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33",
        ),
    )
    p.selected = "receipt"
    p.subject = r.digest
}

/// A reported Q4_K superblock receipt, with the generic cells it observed.
scenario "a q4k superblock" {
    let p = root Project
    let r = p.verifications.insert(
        Receipt(
            digest: "fedcba0987654321fedcba0987654321fedcba0987654321fedcba0987654321",
            source: "",
            evidence: "q4k_superblock_residue_rtl",
            status: .passed,
            declared_toolchain_image: "sha256:0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0",
            sample_sha256: "aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44",
            sample_receipt_sha256: "bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55",
            rtl_source_sha256: "cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66",
            real_protocol: "SUPERBLOCK_PASS groups=8 inputs=2048 stalls=7 overlap=7",
            c_instrumentation: "write aux32[8] and sumi immediately before original FP32 scaling; no numerical changes",
            compiler_flags: [
                "-std=c11",
                "-O0",
                "-fno-fast-math",
                "-ffp-contract=off",
                "oracle.c",
                "-lm",
                "-o",
                "oracle",
            ],
            external_scalar_comparisons: 32,
            has_external_scalar_comparisons: true,
            generic_cell_count: 9940,
            has_generic_cell_count: true,
            excluded_metadata_cells: 12,
            has_excluded_metadata_cells: true,
            excludes: [
                "FP32_rescaler_RTL",
                "actual_activation_capture",
                "optimized_backend_equivalence",
                "complete_layer",
                "BF16_conversion_lineage",
                "model_quality",
                "PPA",
                "manufacturability",
            ],
        ),
    )
    for name in [
        "immutable_real_superblocks",
        "pinned_primary_source",
        "real_integer_banks_protocol_exact_and_primary_C",
        "full_A8_extremes_and_lane_endpoints",
        "fault_sign_rejected",
        "fault_scale_rejected",
        "fault_minimum_rejected",
        "generic_synthesis_and_check",
    ] {
        r.checks.insert(Check(name: name, result: .passed))
    }
    r.tools.insert(
        Tool(
            name: "gcc",
            version: "gcc 14.2.1",
            executable_sha256: "1f28c67c201081aaf051bdf1efdfa559f98b4ec4ddcb6f6f236ac5cd6c1f7db6",
        ),
    )
    r.generic_cells.insert(CellCount(name: "$_ANDNOT_", count: 2367))
    r.generic_cells.insert(CellCount(name: "$_XOR_", count: 4312))
    r.generic_cells.insert(CellCount(name: "$_MUX_", count: 3261))
    p.selected = "receipt"
    p.subject = r.digest
}

/// A reported evidence flow with a placed physical stage.
scenario "an evidence flow" {
    let p = root Project
    let r = p.verifications.insert(
        Receipt(
            digest: "0f0e0d0c0b0a090807060504030201000f0e0d0c0b0a09080706050403020100",
            source: "",
            evidence: "evidence_flow",
            status: .passed,
            imported_bytes_sha256: "1111222233334444555566667777888899990000aaaabbbbccccddddeeeeffff",
            flow: true,
            flow_title: "A reported integer tile flow",
            flow_input_identity_sha256: "aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999",
            compiler_flags: [],
            excludes: [
                "artifact_authentication",
                "silicon_measurement",
                "manufacturing",
                "model_quality",
            ],
        ),
    )
    r.tools.insert(
        Tool(
            name: "verilator",
            version: "Verilator 5.028",
            executable_sha256: "dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11",
        ),
    )
    let cpu = r.stages.insert(
        Stage(
            key: "cpu",
            title: "CPU reference",
            status: .passed,
            original_receipt_artifact: "cpu.json",
            producer_sha256: "1111111111111111111111111111111111111111111111111111111111111111",
            input_identity_sha256: "aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999",
            limitations: ["Reported only; no signoff."],
        ),
    )
    cpu.observed = .cpu_reference
    cpu.observation = "2048 words compared, 0 differing"
    let rtl = r.stages.insert(
        Stage(
            key: "rtl",
            title: "RTL simulation",
            status: .passed,
            original_receipt_artifact: "rtl.json",
            producer_sha256: "2222222222222222222222222222222222222222222222222222222222222222",
            input_identity_sha256: "aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999",
            limitations: ["Reported only; no signoff."],
        ),
    )
    rtl.observed = .rtl_simulation
    rtl.observation = "64 accepted inputs, 64 outputs, 8192 cycles"
    let synth = r.stages.insert(
        Stage(
            key: "synth",
            title: "Generic synthesis",
            status: .passed,
            original_receipt_artifact: "synth.json",
            producer_sha256: "3333333333333333333333333333333333333333333333333333333333333333",
            input_identity_sha256: "aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999",
            limitations: ["Reported only; no signoff."],
        ),
    )
    synth.observed = .generic_synthesis
    synth.observation = "9940 logic cells, 0 logical memory bits"
    let physical = r.stages.insert(
        Stage(
            key: "physical",
            title: "Extracted library estimate",
            status: .passed,
            original_receipt_artifact: "physical.json",
            producer_sha256: "4444444444444444444444444444444444444444444444444444444444444444",
            input_identity_sha256: "aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999",
            limitations: ["Placement footprints only; no layout authentication."],
        ),
    )
    physical.observed = .library_physical
    physical.observation = "research-library: 1840 routed nets, 0 router violations"
    physical.placed = true
    physical.database_artifact = "db.h5"
    physical.observation_artifact = "report.json"
    physical.dbu_per_micron = 1000
    physical.die_left = 0
    physical.die_bottom = 0
    physical.die_right = 100000
    physical.die_top = 80000
    physical.total_instances = 1842
    physical.omitted_instances = 1838
    physical.macros.insert(
        Macro(
            slot: "sram-0",
            name: "SRAM0",
            master: "sram_1rw",
            orientation: .r0,
            placement: "placed",
            left: 8000,
            bottom: 12000,
            right: 26000,
            top: 32000,
        ),
    )
    physical.macros.insert(
        Macro(
            slot: "sram-1",
            name: "SRAM1",
            master: "sram_1rw",
            orientation: .mx,
            placement: "locked",
            left: 62000,
            bottom: 40000,
            right: 80000,
            top: 62000,
        ),
    )
    r.dependencies.insert(
        Dependency(from: "cpu", to: "rtl", kind: .verification, artifact: "rtl.json"),
    )
    r.dependencies.insert(
        Dependency(from: "rtl", to: "synth", kind: .build, artifact: "synth.json"),
    )
    r.dependencies.insert(
        Dependency(from: "synth", to: "physical", kind: .verification, artifact: "physical.json"),
    )
    r.subflows.insert(
        Subflow(key: "layer-group", title: "Additional layer group", summary_artifact: "next.json"),
    )
    r.artifacts.insert(
        Artifact(
            name: "cpu.json",
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    )
    r.artifacts.insert(
        Artifact(
            name: "rtl.json",
            sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
    )
    r.artifacts.insert(
        Artifact(
            name: "synth.json",
            sha256: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        ),
    )
    r.artifacts.insert(
        Artifact(
            name: "physical.json",
            sha256: "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        ),
    )
    r.artifacts.insert(
        Artifact(
            name: "next.json",
            sha256: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        ),
    )
    p.selected = "receipt"
    p.subject = r.digest
    p.stage = "physical"
    p.macro = "sram-0"
}

/// A reported architecture comparison across two variants.
scenario "an architecture study" {
    let p = root Project
    let r = p.verifications.insert(
        Receipt(
            digest: "9999888877776666555544443333222299998888777766665555444433332222",
            source: "",
            evidence: "architecture_study",
            status: .passed,
            study: true,
            study_stage: .aggregate,
            study_original_receipt_sha256: "5555555555555555555555555555555555555555555555555555555555555555",
            study_producer_sha256: "6666666666666666666666666666666666666666666666666666666666666666",
            study_input_identity_sha256: "7777777777777777777777777777777777777777777777777777777777777777",
            study_expected_comparison_words: 2048,
            compiler_flags: [],
            excludes: [
                "full_model_execution",
                "coding_quality",
                "physical_ppa",
                "manufacturing",
                "payload_authentication",
                "synthesized_netlist_equivalence",
            ],
        ),
    )
    r.checks.insert(Check(name: "input_identity_recorded", result: .passed))
    r.checks.insert(Check(name: "all_variants_complete_outputs", result: .passed))
    r.checks.insert(Check(name: "protocol_controls", result: .passed))
    r.checks.insert(Check(name: "wrong_coefficient_rejected", result: .passed))
    r.checks.insert(Check(name: "wrong_row_rejected", result: .passed))
    r.checks.insert(Check(name: "generic_structure_observed", result: .passed))
    r.checks.insert(Check(name: "linked_stages_match_inputs", result: .passed))
    r.tools.insert(
        Tool(
            name: "window",
            version: "prepared",
            executable_sha256: "8888888888888888888888888888888888888888888888888888888888888888",
        ),
    )
    r.links.insert(
        StageLink(
            name: "prepare",
            receipt_sha256: "9999999999999999999999999999999999999999999999999999999999999999",
            producer_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            input_identity_sha256: "7777777777777777777777777777777777777777777777777777777777777777",
        ),
    )
    r.links.insert(
        StageLink(
            name: "numerical",
            receipt_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            producer_sha256: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            input_identity_sha256: "7777777777777777777777777777777777777777777777777777777777777777",
        ),
    )
    let fast = r.variants.insert(
        Variant(
            identity: "packed-4bit",
            numerical: true,
            cold_program_cycles: 5120,
            vector_load_cycles: 4096,
            warm_cycles: 2048,
            accepted_weight_bytes: 524288,
            accepted_descriptor_bytes: 8192,
            cache_reads: 1024,
            quantizer_packets: 64,
            comparison_words: 2048,
            output_sha256: "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            synthesis: true,
            generic_logic_cells: 9940,
            activation_cache_bits: 65536,
            programmed_weight_bits: 4194304,
            fixed_initialized_bits: 1024,
        ),
    )
    fast.memories.insert(
        Memory(
            name: "weights",
            width_bits: 64,
            depth_words: 8192,
            bits: 524288,
            read_ports: 1,
            clocked_read_ports: 1,
            write_ports: 1,
            fully_initialized: true,
        ),
    )
    r.variants.insert(
        Variant(
            identity: "fp32",
            numerical: true,
            cold_program_cycles: 2048,
            vector_load_cycles: 4096,
            warm_cycles: 2048,
            accepted_weight_bytes: 4194304,
            accepted_descriptor_bytes: 8192,
            cache_reads: 1024,
            quantizer_packets: 0,
            comparison_words: 2048,
            output_sha256: "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            synthesis: true,
            generic_logic_cells: 12040,
            activation_cache_bits: 65536,
            programmed_weight_bits: 33554432,
            fixed_initialized_bits: 1024,
        ),
    )
    p.selected = "receipt"
    p.subject = r.digest
}
