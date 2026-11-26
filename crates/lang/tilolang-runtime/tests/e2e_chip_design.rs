//! The Chip Design app (`apps/tl/chip-design`) end to end, over the device engine a session
//! registers: imported engineering evidence checked and kept, the receipts that report integer
//! hardware, CPU operators, a research mapping, a superblock, a reported evidence flow with its
//! placements and an architecture study; and the structural operator design a person builds
//! from them, whose connections the device refuses or holds.

#[cfg(test)]
mod cases {
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::value::ResourceRef;
    use tilolang_runtime::{
        AmStore, EnumValue, InvokeOutcome, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = concat!(
        include_str!("../../../../apps/tl/chip-design/app.tl"),
        "\n",
        include_str!("../../../../apps/tl/chip-design/views.tl"),
        "\n",
        include_str!("../../../../apps/tl/chip-design/views_graph.tl"),
        "\n",
        include_str!("../../../../apps/tl/chip-design/scenarios.tl")
    );
    const REPORT: &str =
        include_str!("../../../engines/tilo-chip-design-engine/fixtures/storage-report.json");
    const CHECKPOINT: &str = include_str!(
        "../../../engines/tilo-chip-design-engine/fixtures/checkpoint-verification.json"
    );
    const MAPPING: &str =
        include_str!("../../../engines/tilo-chip-design-engine/fixtures/research-mapping.json");
    const SUPERBLOCK: &str =
        include_str!("../../../engines/tilo-chip-design-engine/fixtures/q4k-superblock.json");
    const RMS: &str =
        include_str!("../../../engines/tilo-chip-design-engine/fixtures/operators/rms-norm.json");
    const FLOW: &str =
        include_str!("../../../engines/tilo-chip-design-engine/fixtures/evidence-flow.json");

    /// The device's answer, from the engine a session registers for this app: the real checks,
    /// not a stand-in. Every ask it was sent is kept, and it can decline, as a device whose
    /// engine cannot answer does.
    #[derive(Clone, Default)]
    struct Device {
        declines: Rc<Cell<bool>>,
    }

    impl tilolang_tilo::engine::Provider for Device {
        fn call(&self, op: &str, input: &str) -> Result<String, String> {
            if self.declines.get() {
                return Err("the device declines this design".to_owned());
            }
            tilo_chip_design_engine::call(op, input)
        }
    }

    struct Chip {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        device: Device,
        engine: DeviceEngine,
    }

    impl Chip {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            let device = Device::default();
            let engine =
                DeviceEngine::new(device.clone(), &Files::default()).for_app("so.tilo.chip-design");
            rt.set_engine(engine.clone());
            let app = App::compile(SOURCE, &[]);
            app.load(&rt);
            let root = rt.create_root("Project").unwrap();
            Self {
                rt,
                app,
                root,
                device,
                engine,
            }
        }

        fn done(&self, action: &str, args: Vec<Value>) -> Value {
            let id = self.app.action(action);
            match self
                .rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
            {
                InvokeOutcome::Done(value) => value,
                other => panic!("{action}: {other:?}"),
            }
        }

        fn is_case(&self, r: &ResourceRef, field: &str, case: &str) -> bool {
            match self.rt.read(r, field).unwrap() {
                Value::Enum(value) => {
                    self.app.compiled.ids.get(case) == Some(value.variant.as_str())
                }
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn texts(&self, r: &ResourceRef, field: &str) -> Vec<String> {
            match self.rt.read(r, field).unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Text(text) => Some(text),
                        _ => None,
                    })
                    .collect(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn enum_arg(&self, ty: &str, variant: &str) -> Value {
            let ids = &self.app.compiled.ids;
            Value::Enum(EnumValue {
                ty: ids.get(ty).unwrap().to_owned(),
                variant: ids.get(variant).unwrap().to_owned(),
                fields: BTreeMap::default(),
            })
        }

        fn report(&self, json: &str) -> Vec<ResourceRef> {
            self.done(
                "import_report",
                vec![Value::resource(&self.root), Value::text(json)],
            );
            self.records(&self.root, "reports")
        }

        fn receipt(&self, json: &str) -> Vec<ResourceRef> {
            self.done(
                "import_verification",
                vec![Value::resource(&self.root), Value::text(json)],
            );
            self.records(&self.root, "verifications")
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("window"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap_or_else(|e| panic!("window: {e}"));
            window.set_lazy_rows(false).unwrap();
            window
        }

        /// One stage's own page, as a person opens it.
        fn stage_page(&self, stage: &ResourceRef) -> ViewInstance {
            let mut page = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("stage_page"),
                    vec![Value::resource(stage)],
                )
                .unwrap_or_else(|e| panic!("stage page: {e}"));
            page.set_lazy_rows(false).unwrap();
            page
        }

        fn add_operator(
            &self,
            node: &str,
            kind: &str,
            shape: Vec<i64>,
            width: i64,
            x: i64,
            y: i64,
        ) {
            self.done(
                "add_operator_at",
                vec![
                    Value::resource(&self.root),
                    Value::text(node),
                    self.enum_arg("OperatorKind", &format!("OperatorKind.{kind}")),
                    Value::List(shape.into_iter().map(Value::Int).collect()),
                    Value::Int(width),
                    Value::Int(x),
                    Value::Int(y),
                ],
            );
        }

        fn operator(&self, node: &str) -> ResourceRef {
            self.records(&self.root, "operators")
                .into_iter()
                .find(|op| self.text(op, "node") == node)
                .unwrap_or_else(|| panic!("no operator {node}"))
        }

        fn port_ids(&self, op: &ResourceRef, field: &str) -> Vec<String> {
            self.records(op, field)
                .iter()
                .map(|port| self.text(port, "key"))
                .collect()
        }

        fn connect(&self, from: &str, from_port: &str, to: &str, to_port: &str) {
            let from = self.operator(from);
            let to = self.operator(to);
            self.done(
                "connect",
                vec![
                    Value::resource(&self.root),
                    Value::resource(&from),
                    Value::text(from_port),
                    Value::resource(&to),
                    Value::text(to_port),
                ],
            );
        }

        fn operator_nodes(&self) -> Vec<String> {
            self.records(&self.root, "operators")
                .iter()
                .map(|op| self.text(op, "node"))
                .collect()
        }

        /// The identities of the items the project holds in `field`, in order.
        fn item_ids(&self, field: &str) -> Vec<String> {
            self.records(&self.root, field)
                .iter()
                .map(ResourceRef::id_string)
                .collect()
        }

        /// Every change the project's own document holds, oldest first: the design's operators,
        /// their ports, its connections and the project's fields are all in it.
        fn changes(&self) -> Vec<tilolang_runtime::ChangeNote> {
            self.rt.history(&self.root.doc).unwrap()
        }

        /// What the project's document recorded after `before`.
        fn made_since(
            &self,
            before: &[tilolang_runtime::ChangeNote],
        ) -> Vec<tilolang_runtime::ChangeNote> {
            self.changes()[before.len()..].to_vec()
        }
    }

    impl Reads for Chip {
        fn rt(&self) -> &Runtime {
            &self.rt
        }
    }

    /// Every word the window states: a control's label, a row's title, a property's name and
    /// value, an empty state's or a banner's title and message.
    fn labels(tree: &UiNode) -> Vec<String> {
        fn walk(node: &UiNode, found: &mut Vec<String>) {
            for prop in ["label", "title", "value", "message", "secondary"] {
                if let Some(Value::Text(text)) = node.props.get(prop) {
                    found.push(text.clone());
                }
            }
            for child in &node.children {
                walk(child, found);
            }
        }
        let mut found = Vec::new();
        walk(tree, &mut found);
        found
    }

    fn holds(tree: &UiNode, words: &str) -> bool {
        labels(tree).iter().any(|value| value.contains(words))
    }

    /// The properties a page states, each by its name and its value.
    fn properties(tree: &UiNode) -> Vec<(String, String)> {
        of_kind(tree, "ui.Property")
            .into_iter()
            .map(|node| (text_prop(node, "label"), text_prop(node, "value")))
            .collect()
    }

    /// The value a page states for one property, which it states once.
    fn fact(properties: &[(String, String)], name: &str) -> String {
        let found: Vec<&String> = properties
            .iter()
            .filter(|(label, _)| label == name)
            .map(|(_, value)| value)
            .collect();
        assert_eq!(found.len(), 1, "{name}: {properties:?}");
        found[0].clone()
    }

    fn flow_with_placement() -> String {
        let hash = "a".repeat(64);
        serde_json::json!({
            "schema_version": 1,
            "evidence": "evidence_flow",
            "status": "passed",
            "checks": {"reported_dependencies": "passed"},
            "declared_toolchain_image": null,
            "tools": {},
            "artifact_sha256": {
                "physical.json": hash,
                "db.h5": hash,
                "report.json": hash,
            },
            "excludes": [
                "artifact_authentication",
                "silicon_measurement",
                "manufacturing",
                "model_quality",
            ],
            "evidence_flow": {
                "title": "A reported flow with a placement",
                "input_identity_sha256": hash,
                "stages": [{
                    "id": "physical",
                    "title": "Physical",
                    "status": "passed",
                    "original_receipt_artifact": "physical.json",
                    "producer_sha256": hash,
                    "input_identity_sha256": hash,
                    "checks": {"extraction": "passed"},
                    "limitations": ["Reported only; no signoff."],
                    "observations": {
                        "scope": "library_physical",
                        "library": "research-library",
                        "clock_constraint_ns": 2.5,
                        "setup_slack_ns": 0.1,
                        "hold_slack_ns": 0.05,
                        "routed_signal_nets": 10,
                        "router_violations": 0,
                        "library_area_um2": 123.5,
                    },
                    "placement": {
                        "database_artifact": "db.h5",
                        "observation_artifact": "report.json",
                        "dbu_per_micron": 1000,
                        "die_dbu": [0, 0, 100_000, 100_000],
                        "total_instances": 1,
                        "omitted_instances": 0,
                        "macros": [{
                            "id": "m1",
                            "name": "SRAM0",
                            "master": "sram_1rw",
                            "orientation": "R0",
                            "status": "placed",
                            "bounds_dbu": [0, 0, 1000, 2000],
                        }],
                    },
                }],
                "edges": [],
                "subflows": [],
            },
        })
        .to_string()
    }

    /// The repository's evidence flow, with an observation of each kind, a completed and a
    /// refused RTL simulation, a stage that reports none, a placement, and one artifact digest
    /// of its own for each name the receipt holds.
    fn flow_with_facts() -> String {
        let hash = |byte: &str| byte.repeat(64);
        let mut receipt: serde_json::Value = serde_json::from_str(FLOW).unwrap();
        receipt["artifact_sha256"] = serde_json::json!({
            "cpu.json": hash("1"),
            "rtl.json": hash("2"),
            "rtl-refused.json": hash("3"),
            "synth.json": hash("4"),
            "physical.json": hash("5"),
            "db.h5": hash("6"),
            "report.json": hash("7"),
            "next.json": hash("8"),
            "prepared.json": hash("9"),
        });
        {
            let flow = &mut receipt["evidence_flow"];
            let stages = flow["stages"].as_array_mut().unwrap();
            stages.push(serde_json::json!({
                "id": "prepared",
                "title": "Synthetic prepared inputs",
                "status": "passed",
                "original_receipt_artifact": "prepared.json",
                "producer_sha256": hash("a"),
                "input_identity_sha256": hash("a"),
                "checks": {"synthetic_observation": "passed"},
                "limitations": ["Synthetic contract fixture; not an actual executed design"],
            }));
            stages.push(serde_json::json!({
                "id": "rtl-refused",
                "title": "Synthetic refused simulation",
                "status": "passed",
                "original_receipt_artifact": "rtl-refused.json",
                "producer_sha256": hash("a"),
                "input_identity_sha256": hash("a"),
                "checks": {"synthetic_observation": "passed"},
                "limitations": ["Synthetic contract fixture; not an actual executed design"],
                "observations": {
                    "scope": "rtl_simulation",
                    "outcome": "expected_refusal",
                    "refused_inputs": 3,
                    "cycles": 1200,
                    "accepted_inputs": 0,
                    "accepted_outputs": 0,
                    "output_sha256": hash("a"),
                },
            }));
            for stage in stages.iter_mut() {
                if stage["id"] == "physical" {
                    stage["placement"] = serde_json::json!({
                        "database_artifact": "db.h5",
                        "observation_artifact": "report.json",
                        "dbu_per_micron": 1000,
                        "die_dbu": [0, 0, 100_000, 100_000],
                        "total_instances": 1,
                        "omitted_instances": 0,
                        "macros": [{
                            "id": "m1",
                            "name": "SRAM0",
                            "master": "sram_1rw",
                            "orientation": "R0",
                            "status": "placed",
                            "bounds_dbu": [0, 0, 1000, 2000],
                        }],
                    });
                }
            }
        }
        receipt.to_string()
    }

    fn study() -> String {
        let hash = "b".repeat(64);
        serde_json::json!({
            "schema_version": 1,
            "evidence": "architecture_study",
            "status": "passed",
            "checks": {"linked_stages_match_inputs": "passed"},
            "declared_toolchain_image": null,
            "tools": {"window": {"version": "prepared", "executable_sha256": hash}},
            "artifact_sha256": {"prepared.json": hash},
            "excludes": [
                "full_model_execution",
                "coding_quality",
                "physical_ppa",
                "manufacturing",
                "payload_authentication",
                "synthesized_netlist_equivalence",
            ],
            "architecture_study": {
                "stage": "aggregate",
                "original_receipt_sha256": hash,
                "producer_sha256": hash,
                "input_identity_sha256": hash,
                "links": links(&hash),
                "expected_comparison_words": 64,
                "variants": [packed_variant(&hash), fp32_variant(&hash)],
            },
        })
        .to_string()
    }

    /// The receipt's four stage links, each naming `hash`'s receipt.
    fn links(hash: &str) -> serde_json::Value {
        let link = serde_json::json!({
            "receipt_sha256": hash,
            "producer_sha256": hash,
            "input_identity_sha256": hash,
        });
        serde_json::json!({
            "prepare": link,
            "numerical": link,
            "fault": link,
            "synthesis": link,
        })
    }

    /// The packed-4bit variant the study compares, its receipt naming `hash`.
    fn packed_variant(hash: &str) -> serde_json::Value {
        serde_json::json!({
            "identity": "packed-4bit",
            "numerical": {
                "cold_program_cycles": 1,
                "vector_load_cycles": 1,
                "warm_cycles": 1,
                "accepted_weight_bytes": 1,
                "accepted_descriptor_bytes": 1,
                "cache_reads": 1,
                "quantizer_packets": 1,
                "comparison_words": 64,
                "output_sha256": hash,
            },
            "synthesis": {
                "generic_logic_cells": 1,
                "activation_cache_bits": 1,
                "programmed_weight_bits": 1,
                "fixed_initialized_bits": 1,
                "licensed_memory_macro": false,
                "synthesized_netlist_equivalence": false,
                "memories": [{
                    "name": "weights",
                    "width_bits": 64,
                    "depth_words": 8192,
                    "bits": 524_288,
                    "read_ports": 1,
                    "clocked_read_ports": 1,
                    "write_ports": 1,
                    "fully_initialized": true,
                }],
            },
        })
    }

    /// The fp32 variant the study compares, its receipt naming `hash`.
    fn fp32_variant(hash: &str) -> serde_json::Value {
        serde_json::json!({
            "identity": "fp32",
            "numerical": {
                "cold_program_cycles": 1,
                "vector_load_cycles": 1,
                "warm_cycles": 1,
                "accepted_weight_bytes": 1,
                "accepted_descriptor_bytes": 1,
                "cache_reads": 1,
                "quantizer_packets": 1,
                "comparison_words": 64,
                "output_sha256": hash,
            },
            "synthesis": {
                "generic_logic_cells": 2,
                "activation_cache_bits": 1,
                "programmed_weight_bits": 1,
                "fixed_initialized_bits": 1,
                "licensed_memory_macro": false,
                "synthesized_netlist_equivalence": false,
                "memories": [],
            },
        })
    }

    tilo_test_env::scenario! {
        fn e2e_chip_design_1;

        phase a_first_run_starts_empty_and_opens_the_graph;
        {
            let c = Chip::new();
            assert_eq!(c.text(&c.root, "selected"), "all");
            assert!(c.records(&c.root, "reports").is_empty());
            assert!(c.records(&c.root, "operators").is_empty());
            let window = c.window();
            assert!(holds(window.tree(), "Nothing in this project yet"));
            c.done("show_graph", vec![Value::resource(&c.root)]);
            assert_eq!(c.text(&c.root, "selected"), "graph");
            let window = c.window();
            assert!(holds(window.tree(), "No operators yet"));
        }

        phase an_imported_report_keeps_its_provenance_and_is_removed_by_its_own_action;
        {
            let c = Chip::new();
            let reports = c.report(REPORT);
            assert_eq!(reports.len(), 1);
            let r = &reports[0];
            assert_eq!(c.text(r, "model"), "Synthetic/Contract-Example");
            assert_eq!(
                c.text(r, "revision"),
                "ad835591193cff5d2a231485cab35f8654c71287"
            );
            assert_eq!(c.number(r, "packed_weight_bytes"), 512);
            assert_eq!(c.text(r, "config_sha256").len(), 64);
            assert!(
                c.texts(r, "not_estimated")
                    .iter()
                    .any(|v| v == "coding_quality")
            );
            assert_eq!(c.text(&c.root, "selected"), "report");
            assert_eq!(c.text(&c.root, "subject"), c.text(r, "digest"));

            let window = c.window();
            assert!(holds(window.tree(), "Synthetic/Contract-Example"));
            assert!(holds(window.tree(), "Packed weight bytes"));
            assert!(holds(window.tree(), "Verification scope"));

            c.done(
                "remove_report",
                vec![Value::resource(&c.root), Value::resource(r)],
            );
            assert!(c.records(&c.root, "reports").is_empty());
            assert_eq!(c.text(&c.root, "selected"), "all");
        }

        phase the_same_imported_bytes_keep_one_record;
        {
            let c = Chip::new();
            c.report(REPORT);
            c.report(REPORT);
            assert_eq!(c.records(&c.root, "reports").len(), 1);
        }

        phase a_forged_report_is_refused_and_leaves_no_record;
        {
            let c = Chip::new();
            let mut report: serde_json::Value = serde_json::from_str(REPORT).unwrap();
            report["packed_weight_bytes"] = serde_json::json!(1);
            c.report(&report.to_string());
            assert!(c.records(&c.root, "reports").is_empty());
            assert!(!c.text(&c.root, "failure").is_empty());

            let mut moving: serde_json::Value = serde_json::from_str(REPORT).unwrap();
            moving["revision"] = serde_json::json!("main");
            c.report(&moving.to_string());
            assert!(c.records(&c.root, "reports").is_empty());
        }

        phase an_imported_receipt_keeps_its_checks_and_no_device_path;
        {
            let c = Chip::new();
            let receipts = c.receipt(CHECKPOINT);
            assert_eq!(receipts.len(), 1);
            let r = &receipts[0];
            assert_eq!(c.text(r, "evidence"), "checkpoint_tile_integer_contract");
            assert!(c.is_case(r, "status", "RunResult.passed"));
            let checks = c.records(r, "checks");
            assert_eq!(checks.len(), 3);
            assert!(
                checks
                    .iter()
                    .any(|check| c.text(check, "name") == "rtl_vs_integer_oracle")
            );
            assert_eq!(c.records(r, "tools").len(), 2);
            assert!(!c.text(r, "source").contains("/private/device"));
            let window = c.window();
            assert!(holds(window.tree(), "Reported passed"));
            c.done(
                "remove_verification",
                vec![Value::resource(&c.root), Value::resource(r)],
            );
            assert!(c.records(&c.root, "verifications").is_empty());
        }

        phase a_cpu_receipt_keeps_every_numerical_observation;
        {
            let c = Chip::new();
            c.receipt(RMS);
            let receipts = c.records(&c.root, "verifications");
            let r = &receipts[0];
            assert!(c.flag(r, "operator"));
            let observations = c.records(r, "observations");
            assert_eq!(observations.len(), 6);
            assert!(
                observations
                    .iter()
                    .all(|o| c.is_case(o, "kind", "ObservationKind.rms_norm"))
            );
            let window = c.window();
            assert!(holds(window.tree(), "Reported numerical observations"));
            assert!(holds(window.tree(), "FP32 words compared exactly"));
        }

        phase a_research_mapping_keeps_its_declared_scope;
        {
            let c = Chip::new();
            let receipts = c.receipt(MAPPING);
            let r = &receipts[0];
            assert_eq!(c.number(r, "mapped_cell_count"), 100);
            assert_eq!(c.text(r, "reported_library_area"), "123.5");
            assert!(c.flag(r, "has_manufacturable"));
            assert!(!c.flag(r, "manufacturable"));
            let window = c.window();
            assert!(holds(window.tree(), "Nonmanufacturable research library"));

            let mut forged: serde_json::Value = serde_json::from_str(MAPPING).unwrap();
            forged["manufacturable"] = serde_json::json!(true);
            c.receipt(&forged.to_string());
            assert_eq!(
                c.records(&c.root, "verifications").len(),
                1,
                "the forged claim is refused"
            );
        }

        phase a_superblock_receipt_keeps_its_cells_and_scalar_protocol;
        {
            let c = Chip::new();
            let receipts = c.receipt(SUPERBLOCK);
            let r = &receipts[0];
            assert_eq!(c.number(r, "generic_cell_count"), 9940);
            assert_eq!(c.number(r, "external_scalar_comparisons"), 32);
            assert!(!c.records(r, "generic_cells").is_empty());
            let window = c.window();
            assert!(holds(window.tree(), "SUPERBLOCK_PASS"));
            assert!(holds(window.tree(), "Reported generic cell count"));
        }

        phase an_evidence_flow_shows_its_stages_their_dependencies_and_the_placement;
        {
            let c = Chip::new();
            c.receipt(&flow_with_placement());
            let receipts = c.records(&c.root, "verifications");
            let r = &receipts[0];
            assert!(c.flag(r, "flow"));
            assert_eq!(c.records(r, "stages").len(), 1);
            let stages = c.records(r, "stages");
            let stage = stages.into_iter().next().unwrap();
            assert!(c.flag(&stage, "placed"));
            assert_eq!(c.records(&stage, "macros").len(), 1);
            assert_eq!(c.text(&stage, "title"), "Physical");

            // The window draws the flow and, with the stage opened, its placement.
            c.done(
                "inspect_stage",
                vec![
                    Value::resource(&c.root),
                    Value::resource(r),
                    Value::resource(&stage),
                ],
            );
            let window = c.window();
            assert!(holds(window.tree(), "Reported macro placement"));
            assert!(holds(window.tree(), "SRAM0"));
            assert!(holds(window.tree(), "Reported evidence flow"));

            let macros = c.records(&stage, "macros");
            let macro_ref = macros.into_iter().next().unwrap();
            c.done(
                "inspect_macro",
                vec![
                    Value::resource(&c.root),
                    Value::resource(r),
                    Value::resource(&macro_ref),
                ],
            );
            assert_eq!(c.text(&c.root, "macro"), c.text(&macro_ref, "slot"));
            let window = c.window();
            assert!(holds(window.tree(), "sram_1rw"));
        }

        phase a_flow_stage_shows_the_facts_it_reports_and_the_artifacts_it_names;
        {
            let c = Chip::new();
            let hash = |byte: &str| byte.repeat(64);
            c.receipt(&flow_with_facts());
            let receipts = c.records(&c.root, "verifications");
            let r = &receipts[0];
            assert!(c.flag(r, "flow"));
            let stages = c.records(r, "stages");
            let mut by_key = BTreeMap::new();
            for stage in &stages {
                by_key.insert(c.text(stage, "key"), stage);
            }

            let cpu = properties(c.stage_page(by_key["cpu"]).tree());
            assert_eq!(
                fact(&cpu, "Comparison contract"),
                "Backend characterization"
            );
            assert_eq!(fact(&cpu, "Compared words"), "8");
            assert_eq!(fact(&cpu, "Differing words"), "1");
            assert_eq!(fact(&cpu, "Reported accepted output SHA-256"), hash("a"));
            assert!(cpu.contains(&(
                "Original receipt artifact".to_owned(),
                "cpu.json".to_owned()
            )));
            assert!(cpu.contains(&("Declared artifact SHA-256".to_owned(), hash("1"))));

            let rtl = properties(c.stage_page(by_key["rtl"]).tree());
            assert_eq!(
                fact(&rtl, "Reported simulation outcome"),
                "Completed simulation"
            );
            assert_eq!(fact(&rtl, "Expected refusals observed"), "0");
            assert_eq!(fact(&rtl, "Observed RTL simulation cycles"), "1200");
            assert_eq!(fact(&rtl, "Accepted inputs"), "8");
            assert_eq!(fact(&rtl, "Accepted outputs"), "8");
            assert_eq!(fact(&rtl, "Reported accepted output SHA-256"), hash("a"));
            assert!(rtl.contains(&("Declared artifact SHA-256".to_owned(), hash("2"))));

            let refused = properties(c.stage_page(by_key["rtl-refused"]).tree());
            assert_eq!(
                fact(&refused, "Reported simulation outcome"),
                "Expected refusal; no completed result"
            );
            assert_eq!(fact(&refused, "Expected refusals observed"), "3");
            assert_eq!(fact(&refused, "Accepted outputs"), "0");
            assert!(refused.contains(&("Declared artifact SHA-256".to_owned(), hash("3"))));

            let synth = properties(c.stage_page(by_key["synth"]).tree());
            assert_eq!(fact(&synth, "Generic logic cells"), "80");
            assert_eq!(fact(&synth, "Logical memory bits"), "256");
            assert!(synth.contains(&("Declared artifact SHA-256".to_owned(), hash("4"))));

            let physical = c.stage_page(by_key["physical"]);
            let facts = properties(physical.tree());
            assert_eq!(
                fact(&facts, "Declared library"),
                "Synthetic research library"
            );
            assert_eq!(fact(&facts, "Declared clock constraint (ns)"), "6");
            assert_eq!(fact(&facts, "Estimated setup slack (ns)"), "0.9");
            assert_eq!(fact(&facts, "Estimated hold slack (ns)"), "0.04");
            assert_eq!(fact(&facts, "Routed signal nets"), "40");
            assert_eq!(fact(&facts, "Reported router violations"), "0");
            assert_eq!(fact(&facts, "Mapped library area estimate (µm²)"), "120");
            assert!(facts.contains(&("Declared artifact SHA-256".to_owned(), hash("5"))));
            assert!(facts.contains(&("Placement database artifact".to_owned(), "db.h5".to_owned())));
            assert!(facts.contains(&("Declared artifact SHA-256".to_owned(), hash("6"))));
            assert!(facts.contains(&(
                "Placement observation artifact".to_owned(),
                "report.json".to_owned()
            )));
            assert!(facts.contains(&("Declared artifact SHA-256".to_owned(), hash("7"))));
            assert!(holds(physical.tree(), "Reported numerical observation"));

            // A stage that reports no observation states none of the facts.
            let prepared = c.stage_page(by_key["prepared"]);
            let none = properties(prepared.tree());
            assert!(
                !holds(prepared.tree(), "Reported numerical observation"),
                "a stage that reports no observation shows no facts section"
            );
            for name in [
                "Comparison contract",
                "Reported simulation outcome",
                "Generic logic cells",
                "Declared library",
                "Routed signal nets",
                "Placement database artifact",
                "Placement observation artifact",
            ] {
                assert!(
                    !none.iter().any(|(label, _)| label == name),
                    "{name}: {none:?}"
                );
            }
            assert!(none.contains(&(
                "Original receipt artifact".to_owned(),
                "prepared.json".to_owned()
            )));
            assert!(none.contains(&("Declared artifact SHA-256".to_owned(), hash("9"))));
        }

        phase an_architecture_study_keeps_its_links_variants_and_memories;
        {
            let c = Chip::new();
            c.receipt(&study());
            let receipts = c.records(&c.root, "verifications");
            let r = &receipts[0];
            assert!(c.flag(r, "study"));
            assert!(c.is_case(r, "study_stage", "StageKind.aggregate"));
            assert_eq!(c.records(r, "variants").len(), 2);
            let variants = c.records(r, "variants");
            let variant = variants.into_iter().next().unwrap();
            assert!(c.flag(&variant, "synthesis"));
            assert_eq!(c.records(&variant, "memories").len(), 1);
            let window = c.window();
            assert!(holds(window.tree(), "Producer-reported stage checks"));
            assert!(holds(window.tree(), "Preparation"));
        }
    }

    tilo_test_env::scenario! {
        fn e2e_chip_design_2;

        phase an_operator_is_added_connected_reshaped_and_removed;
        {
            let c = Chip::new();
            c.add_operator("x", "input", vec![4, 8], 0, 0, 0);
            c.add_operator("rms", "rms_norm", vec![4, 8], 0, 280, 0);
            let x = c.operator("x");
            let rms = c.operator("rms");
            assert_eq!(c.number(&x, "normalization_width"), 8);
            assert_eq!(c.port_ids(&x, "outputs"), ["value"]);
            assert_eq!(c.port_ids(&rms, "inputs"), ["value"]);
            assert_eq!(c.port_ids(&rms, "outputs"), ["result"]);

            c.connect("x", "value", "rms", "value");
            let connections = c.records(&c.root, "connections");
            assert_eq!(connections.len(), 1);
            assert!(c.flag(&connections[0], "active"));
            assert!(!c.text(&connections[0], "digest").is_empty());

            // A shape the ports no longer match stops the connection holding.
            let rms = c.operator("rms");
            c.done(
                "set_shape",
                vec![
                    Value::resource(&c.root),
                    Value::resource(&rms),
                    Value::List(vec![Value::Int(4), Value::Int(16)]),
                ],
            );
            let connections = c.records(&c.root, "connections");
            assert!(!c.flag(&connections[0], "active"));
            assert_eq!(c.text(&connections[0], "reason"), "type_mismatch");

            let rms = c.operator("rms");
            c.done(
                "remove_operator",
                vec![Value::resource(&c.root), Value::resource(&rms)],
            );
            assert_eq!(c.records(&c.root, "operators").len(), 1);
            assert!(c.records(&c.root, "connections").is_empty());
        }

        phase removing_an_operator_asks_the_device_about_the_design_it_leaves_and_is_one_step;
        {
            let c = Chip::new();
            c.add_operator("x", "input", vec![4, 8], 0, 0, 0);
            c.add_operator("rms", "rms_norm", vec![4, 8], 0, 280, 0);
            c.add_operator("silu", "silu", vec![4, 8], 0, 560, 0);
            c.connect("x", "value", "rms", "value");
            c.connect("rms", "result", "silu", "value");
            let asked = c.engine.asked().len();
            let rms = c.operator("rms");
            c.done(
                "inspect_operator",
                vec![Value::resource(&c.root), Value::resource(&rms)],
            );
            let before = c.changes();

            let rms = c.operator("rms");
            c.done(
                "remove_operator",
                vec![Value::resource(&c.root), Value::resource(&rms)],
            );

            // The device is asked once, about the design the removal leaves: the operator and its
            // own connections are out of the ask already.
            let asks = c.engine.asked();
            assert_eq!(asks.len(), asked + 1, "the removal asks the device once");
            assert_eq!(asks[asked].0, "graph");
            let design: serde_json::Value = serde_json::from_str(&asks[asked].1).unwrap();
            assert_eq!(design["nodes"].as_array().unwrap().len(), 2);
            assert!(!asks[asked].1.contains("\"rms\""));
            assert!(design["connections"].as_array().unwrap().is_empty());

            assert_eq!(c.operator_nodes(), ["x", "silu"]);
            assert!(c.records(&c.root, "connections").is_empty());
            assert_eq!(c.text(&c.root, "node"), "", "the removed operator closes");
            assert_eq!(c.text(&c.root, "failure"), "");

            // Every change the removal made, in the project and in the items it held, is one
            // invocation: one step to undo.
            let made = c.made_since(&before);
            assert!(!made.is_empty(), "the removal changed the design");
            let invocation = &made[0].invocation;
            assert!(invocation.is_some(), "the removal is a run of its own");
            assert!(
                made.iter().all(|note| &note.invocation == invocation),
                "the removal is one step: {made:?}"
            );
        }

        phase a_declined_ask_leaves_the_design_as_it_was;
        {
            let c = Chip::new();
            c.add_operator("x", "input", vec![4, 8], 0, 0, 0);
            c.add_operator("rms", "rms_norm", vec![4, 8], 0, 280, 0);
            c.connect("x", "value", "rms", "value");
            let operators = c.item_ids("operators");
            let connections = c.item_ids("connections");
            c.device.declines.set(true);

            let rms = c.operator("rms");
            c.done(
                "remove_operator",
                vec![Value::resource(&c.root), Value::resource(&rms)],
            );

            assert_eq!(c.operator_nodes(), ["x", "rms"]);
            assert_eq!(c.item_ids("operators"), operators, "no operator left");
            assert_eq!(c.item_ids("connections"), connections, "no connection left");
            let connection = c.records(&c.root, "connections");
            assert!(c.flag(&connection[0], "active"));
            assert!(!c.text(&connection[0], "digest").is_empty());
            assert_eq!(
                c.text(&c.root, "failure"),
                "the device declines this design"
            );
        }

        phase disconnecting_an_input_asks_the_device_and_is_one_step;
        {
            let c = Chip::new();
            c.add_operator("x", "input", vec![4, 8], 0, 0, 0);
            c.add_operator("rms", "rms_norm", vec![4, 8], 0, 280, 0);
            c.connect("x", "value", "rms", "value");
            let before = c.changes();
            let asked = c.engine.asked().len();

            let rms = c.operator("rms");
            c.done(
                "disconnect",
                vec![
                    Value::resource(&c.root),
                    Value::resource(&rms),
                    Value::text("value"),
                ],
            );

            let asks = c.engine.asked();
            assert_eq!(asks.len(), asked + 1, "the disconnect asks the device once");
            assert_eq!(asks[asked].0, "graph");
            assert!(
                !asks[asked].1.contains("from_node"),
                "the ask holds no connection into that input"
            );
            assert_eq!(c.operator_nodes(), ["x", "rms"]);
            assert!(c.records(&c.root, "connections").is_empty());
            assert_eq!(c.text(&c.root, "failure"), "");

            let made = c.made_since(&before);
            assert!(!made.is_empty(), "the disconnect changed the design");
            let invocation = &made[0].invocation;
            assert!(invocation.is_some(), "the disconnect is a run of its own");
            assert!(
                made.iter().all(|note| &note.invocation == invocation),
                "the disconnect is one step: {made:?}"
            );
        }

        phase a_connection_the_types_refuse_is_refused_and_says_why;
        {
            let c = Chip::new();
            c.add_operator("w", "norm_weight_input", vec![8], 0, 0, 0);
            c.add_operator("rms", "rms_norm", vec![4, 8], 0, 280, 0);
            c.connect("w", "weights", "rms", "value");
            assert!(c.records(&c.root, "connections").is_empty());
            assert_eq!(c.text(&c.root, "failure"), "Port shapes differ");
        }

        phase choosing_an_output_names_what_it_can_feed;
        {
            let c = Chip::new();
            c.add_operator("x", "input", vec![4, 8], 0, 0, 0);
            c.add_operator("rms", "rms_norm", vec![4, 8], 0, 280, 0);
            let x = c.operator("x");
            c.done(
                "select_output",
                vec![
                    Value::resource(&c.root),
                    Value::resource(&x),
                    Value::text("value"),
                ],
            );
            assert_eq!(
                c.text(&c.root, "failure"),
                "",
                "choosing an output is refused"
            );
            assert_eq!(c.text(&c.root, "output_node"), "x");
            assert_eq!(c.text(&c.root, "output_port"), "value");
            c.done("show_graph", vec![Value::resource(&c.root)]);
            let window = c.window();
            assert!(holds(
                window.tree(),
                "Connecting from the Value output of Input"
            ));
        }

        phase the_graph_zooms_and_a_press_opens_an_operator;
        {
            let c = Chip::new();
            c.add_operator("x", "input", vec![4, 8], 0, 0, 0);
            c.add_operator("rms", "rms_norm", vec![4, 8], 0, 280, 0);
            c.done("show_graph", vec![Value::resource(&c.root)]);
            c.done(
                "zoom_graph",
                vec![Value::resource(&c.root), Value::text("in")],
            );
            assert_eq!(c.number(&c.root, "zoom"), 125);
            c.done(
                "zoom_graph",
                vec![Value::resource(&c.root), Value::text("fit")],
            );
            assert_eq!(c.number(&c.root, "zoom"), 100);

            // The first shape of the canvas is the first operator's box.
            c.done("tap_graph", vec![Value::resource(&c.root), Value::Int(0)]);
            assert_eq!(c.text(&c.root, "node"), "rms");
            let window = c.window();
            assert!(holds(window.tree(), "Node identity"));
            assert!(holds(window.tree(), "Apply shape"));
        }

        phase an_operator_the_design_cannot_hold_is_refused;
        {
            let c = Chip::new();
            c.add_operator("x", "input", vec![], 0, 0, 0);
            assert!(c.records(&c.root, "operators").is_empty());
            assert!(!c.text(&c.root, "failure").is_empty());
            c.add_operator("x", "input", vec![4, 8], 0, 0, 0);
            c.add_operator("x", "rms_norm", vec![4, 8], 0, 0, 0);
            assert_eq!(c.records(&c.root, "operators").len(), 1);
            assert_eq!(
                c.text(&c.root, "failure"),
                "An operator with that identity already exists"
            );
        }

        phase the_window_lists_everything_the_project_holds;
        {
            let c = Chip::new();
            c.report(REPORT);
            c.receipt(CHECKPOINT);
            let window = c.window();
            assert!(holds(window.tree(), "Synthetic/Contract-Example"));
            assert!(holds(window.tree(), "checkpoint_tile_integer_contract"));
            assert!(holds(window.tree(), "Reports"));
            assert!(holds(window.tree(), "Verifications"));
        }

        phase a_published_surface_is_the_one_the_app_declares;
        {
            let c = Chip::new();
            for action in [
                "import_report",
                "import_verification",
                "import_file",
                "remove_report",
                "remove_verification",
                "add_operator",
                "add_operator_at",
                "set_shape",
                "set_output_width",
                "move_operator",
                "remove_operator",
                "connect",
                "disconnect",
                "show_graph",
                "show_all",
                "inspect_report",
                "inspect_receipt",
                "inspect_stage",
                "inspect_macro",
                "open_subflow",
                "zoom_graph",
                "select_output",
                "inspect_operator",
            ] {
                assert!(
                    c.app.meta().action_named(action).is_some(),
                    "{action} is published"
                );
            }
        }
    }
}
