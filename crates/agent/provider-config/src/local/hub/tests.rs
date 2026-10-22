use super::*;
use crate::local::{RuntimeExecution, RuntimeRelease};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn lfs(path: &str, size: u64, fill: char) -> serde_json::Value {
    serde_json::json!({
        "rfilename": path,
        "size": size,
        "blobId": "1".repeat(40),
        "lfs": {"sha256": fill.to_string().repeat(64), "size": size, "pointerSize": 134}
    })
}

fn small(path: &str, size: u64) -> serde_json::Value {
    serde_json::json!({"rfilename": path, "size": size, "blobId": "2".repeat(40)})
}

/// A repository as the Hub's `?blobs=true` answer describes it.
fn info() -> ApiModelInfo {
    serde_json::from_value(serde_json::json!({
        "id": "acme/Chat-7B-GGUF",
        "sha": COMMIT,
        "gated": false,
        "cardData": {"license": "apache-2.0"},
        "tags": ["gguf", "license:apache-2.0"],
        "gguf": {"context_length": 32768, "architecture": "llama"},
        "siblings": [
            small(".gitattributes", 1500),
            small("README.md", 4000),
            lfs("chat-7b.Q4_K_M.gguf", 4_000_000_000, 'a'),
            lfs("chat-7b.Q8_0.gguf", 7_000_000_000, 'b'),
            lfs("chat-7b-IQ3_XXS.gguf", 2_500_000_000, 'c'),
            lfs("mmproj-chat-7b-f16.gguf", 600_000_000, 'd'),
            // No published hash: never offered.
            {"rfilename": "chat-7b.Q2_K.gguf", "size": 2_000_000_000},
            lfs("BF16/chat-7b-BF16-00001-of-00002.gguf", 8_000_000_000, 'e'),
            lfs("BF16/chat-7b-BF16-00002-of-00002.gguf", 6_000_000_000, 'f'),
            // Missing its second shard: incomplete, never offered.
            lfs("F32/chat-7b-F32-00001-of-00002.gguf", 9_000_000_000, '9'),
            small("config.json", 700),
            small("tokenizer.json", 9_000_000),
            small("tokenizer_config.json", 3000),
            lfs("model-00001-of-00002.safetensors", 9_000_000_000, '1'),
            lfs("model-00002-of-00002.safetensors", 5_000_000_000, '2'),
            small("model.safetensors.index.json", 30_000),
            lfs("pytorch_model.bin", 14_000_000_000, '3'),
            small("modeling_chat.py", 9000),
            lfs("original/consolidated.safetensors", 14_000_000_000, '4'),
        ]
    }))
    .unwrap()
}

#[test]
fn a_person_names_a_model_by_id_or_any_hub_link() {
    let plain = HubRef::parse(" acme/Chat-7B ").unwrap();
    assert_eq!(
        plain,
        HubRef {
            repo: "acme/Chat-7B".into(),
            revision: None,
            file: None
        }
    );
    for link in [
        "https://huggingface.co/acme/Chat-7B",
        "https://huggingface.co/acme/Chat-7B/",
        "huggingface.co/acme/Chat-7B?library=gguf",
        "https://hf.co/acme/Chat-7B#files",
    ] {
        assert_eq!(HubRef::parse(link).unwrap(), plain, "{link}");
    }
    let file = HubRef::parse(
        "https://huggingface.co/acme/Chat-7B/blob/main/sub/chat-7b.Q4_K_M.gguf?download=true",
    )
    .unwrap();
    assert_eq!(file.revision.as_deref(), Some("main"));
    assert_eq!(file.file.as_deref(), Some("sub/chat-7b.Q4_K_M.gguf"));
    let pr = HubRef::parse("https://huggingface.co/acme/Chat-7B/tree/refs/pr/3").unwrap();
    assert_eq!(pr.revision.as_deref(), Some("refs/pr/3"));
    assert_eq!(pr.file, None);
    for refused in [
        "",
        "chat",
        "https://example.com/acme/Chat-7B",
        "https://huggingface.co/datasets/acme/data",
        "https://huggingface.co/spaces/acme/demo",
        "acme/../etc",
        "acme/Chat-7B/blob/main/../../secret",
        "acme/Chat 7B",
        "acme/Chat-7B/commits/main",
    ] {
        assert!(HubRef::parse(refused).is_err(), "{refused}");
    }
}

#[test]
fn api_urls_encode_what_a_person_typed() {
    assert!(
        search_url("llama 3 & more", HubFormat::Gguf)
            .starts_with("https://huggingface.co/api/models?search=llama+3+%26+more&filter=gguf&")
    );
    assert!(search_url("x", HubFormat::Safetensors).contains("expand%5B%5D=gated"));
    assert_eq!(
        model_info_url("acme/Chat", Some("refs/pr/3")),
        "https://huggingface.co/api/models/acme/Chat/revision/refs%2Fpr%2F3?blobs=true"
    );
    assert_eq!(
        file_url("acme/Chat", COMMIT, "BF16/a b.gguf"),
        format!("https://huggingface.co/acme/Chat/resolve/{COMMIT}/BF16/a+b.gguf")
    );
}

#[test]
fn quantizations_are_read_from_file_and_directory_names() {
    for (path, expected) in [
        ("chat-7b.Q4_K_M.gguf", Some("Q4_K_M")),
        ("Llama-3.2-1B-Instruct-IQ3_XXS.gguf", Some("IQ3_XXS")),
        ("qwen2.5-0.5b-instruct-fp16.gguf", Some("FP16")),
        ("model-UD-Q4_K_XL.gguf", Some("Q4_K_XL")),
        ("Q5_K_S/model-00001-of-00003.gguf", Some("Q5_K_S")),
        ("phi-3-mini-4k-instruct.gguf", None),
    ] {
        assert_eq!(quantization(path).as_deref(), expected, "{path}");
    }
}

#[test]
fn variants_offer_only_complete_hashed_data_files() {
    let variants = variants(&info());
    let names: Vec<(&str, ModelArtifactFormat)> = variants
        .iter()
        .map(|variant| (variant.name.as_str(), variant.format))
        .collect();
    assert_eq!(
        names,
        [
            ("IQ3_XXS", ModelArtifactFormat::LlamaCppGguf),
            ("Q4_K_M", ModelArtifactFormat::LlamaCppGguf),
            ("Q8_0", ModelArtifactFormat::LlamaCppGguf),
            ("BF16", ModelArtifactFormat::LlamaCppGgufShards),
            ("safetensors", ModelArtifactFormat::HfSnapshotFiles),
        ]
    );
    let shards = &variants[3];
    assert_eq!(shards.download_bytes(), 14_000_000_000);
    assert!(shards.files[0].path.ends_with("00001-of-00002.gguf"));
    let snapshot = &variants[4];
    let mut paths: Vec<&str> = snapshot
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    paths.sort_unstable();
    // Weights, config and tokenizer: never pickles, code or subfolders.
    assert_eq!(
        paths,
        [
            "config.json",
            "model-00001-of-00002.safetensors",
            "model-00002-of-00002.safetensors",
            "model.safetensors.index.json",
            "tokenizer.json",
            "tokenizer_config.json",
        ]
    );
    assert_eq!(
        snapshot
            .files
            .iter()
            .find(|file| file.path == "config.json")
            .unwrap()
            .digest,
        HubDigest::GitBlob {
            oid: "2".repeat(40)
        }
    );
}

#[test]
fn a_snapshot_without_tokenizer_or_hashes_is_not_offered() {
    let mut info = info();
    info.siblings
        .retain(|file| !file.rfilename.starts_with("tokenizer"));
    assert!(
        variants(&info)
            .iter()
            .all(|variant| variant.format != ModelArtifactFormat::HfSnapshotFiles)
    );
    let mut info = super::tests::info();
    for file in &mut info.siblings {
        if file.rfilename == "config.json" {
            file.blob_id = None;
        }
    }
    assert!(
        variants(&info)
            .iter()
            .all(|variant| variant.format != ModelArtifactFormat::HfSnapshotFiles)
    );
}

#[test]
fn one_quantization_in_two_files_is_told_apart_by_name() {
    let info: ApiModelInfo = serde_json::from_value(serde_json::json!({
        "id": "acme/x", "sha": COMMIT,
        "siblings": [lfs("a/x.Q4_0.gguf", 10, 'a'), lfs("b/x.Q4_0.gguf", 20, 'b')]
    }))
    .unwrap();
    let names: Vec<String> = variants(&info)
        .into_iter()
        .map(|variant| variant.name)
        .collect();
    assert_eq!(names, ["a/x.Q4_0", "b/x.Q4_0"]);
}

#[test]
fn the_licence_is_the_cards_with_its_document_at_the_commit() {
    let license = HubLicense::of(&info());
    assert_eq!(license.id, "apache-2.0");
    assert_eq!(
        license.url,
        format!("https://huggingface.co/acme/Chat-7B-GGUF/blob/{COMMIT}/README.md")
    );
    let mut other = info();
    other.card_data = Some(ApiCardData {
        license: Some(serde_json::json!("other")),
        license_name: Some("llama3.1".into()),
        license_link: None,
    });
    other.siblings.push(ApiSibling {
        rfilename: "LICENSE".into(),
        size: Some(10),
        blob_id: Some("3".repeat(40)),
        lfs: None,
    });
    let license = HubLicense::of(&other);
    assert_eq!(license.id, "llama3.1");
    assert!(license.url.ends_with("/LICENSE"));
    let mut none = info();
    none.card_data = None;
    none.tags.clear();
    assert_eq!(HubLicense::of(&none).id, "unknown");
}

#[test]
fn gating_reads_the_hubs_three_answers() {
    for (value, gated) in [
        (serde_json::json!(false), Gated::Open),
        (serde_json::json!("auto"), Gated::Auto),
        (serde_json::json!("manual"), Gated::Manual),
    ] {
        assert_eq!(serde_json::from_value::<Gated>(value).unwrap(), gated);
        let ours = serde_json::to_value(gated).unwrap();
        assert_eq!(serde_json::from_value::<Gated>(ours).unwrap(), gated);
    }
    let hit: ApiSearchHit = serde_json::from_value(serde_json::json!({
        "id": "acme/Chat", "downloads": 12, "likes": 3, "gated": "manual",
        "pipeline_tag": "text-generation", "tags": ["gguf", "license:mit", "safetensors"]
    }))
    .unwrap();
    let result = HubSearchResult::of(hit).unwrap();
    assert!(result.gated);
    assert_eq!(result.license.as_deref(), Some("mit"));
    assert_eq!(result.formats, [HubFormat::Gguf, HubFormat::Safetensors]);
    let bad: ApiSearchHit = serde_json::from_value(serde_json::json!({"id": "../x"})).unwrap();
    assert!(HubSearchResult::of(bad).is_none());
}

fn machine(memory_gib: u64, vram_gib: Option<u64>) -> HardwareSnapshot {
    HardwareSnapshot {
        cpu_threads: 8,
        memory_mib: memory_gib.saturating_mul(1024),
        free_storage_mib: 500 * 1024,
        accelerators: vram_gib
            .map(|gib| Accelerator::Nvidia {
                vram_mib: gib.saturating_mul(1024),
                driver: "570.1".into(),
            })
            .into_iter()
            .collect(),
    }
}

#[test]
fn fit_follows_accelerator_memory_then_system_memory() {
    let gguf = ModelArtifactFormat::LlamaCppGguf;
    let snapshot = ModelArtifactFormat::HfSnapshotFiles;
    let gib = 1024 * MIB;
    let rate = |hardware: &HardwareSnapshot, format, bytes| {
        let fit = rate_fit(hardware, format, bytes);
        (fit.fit, fit.reason)
    };
    let gpu = machine(32, Some(16));
    assert_eq!(
        rate(&gpu, gguf, 8 * gib),
        (HardwareFit::Excellent, HubFitReason::Accelerator)
    );
    assert_eq!(
        rate(&gpu, gguf, 20 * gib),
        (HardwareFit::Good, HubFitReason::AcceleratorAndMemory)
    );
    assert_eq!(
        rate(&gpu, gguf, 38 * gib),
        (HardwareFit::Tight, HubFitReason::AcceleratorAndMemory)
    );
    assert_eq!(
        rate(&gpu, snapshot, 20 * gib),
        (HardwareFit::Unsupported, HubFitReason::TooLarge)
    );
    let cpu = machine(16, None);
    assert_eq!(
        rate(&cpu, gguf, 4 * gib),
        (HardwareFit::Good, HubFitReason::Memory)
    );
    assert_eq!(
        rate(&cpu, gguf, 9 * gib),
        (HardwareFit::Tight, HubFitReason::Memory)
    );
    assert_eq!(
        rate(&cpu, snapshot, 4 * gib),
        (HardwareFit::Tight, HubFitReason::Memory)
    );
    assert_eq!(
        rate(&cpu, gguf, 20 * gib),
        (HardwareFit::Unsupported, HubFitReason::TooLarge)
    );
    assert_eq!(
        rate(&machine(4, None), gguf, MIB),
        (HardwareFit::Unsupported, HubFitReason::ProcessorTooSmall)
    );
    let mut full = machine(64, Some(24));
    full.free_storage_mib = 1024;
    assert_eq!(
        rate(&full, gguf, 2 * gib),
        (HardwareFit::Unsupported, HubFitReason::NoStorage)
    );
    // An accelerator whose driver is inactive does not count.
    let mut inactive = machine(16, None);
    inactive.accelerators.push(Accelerator::Nvidia {
        vram_mib: 24 * 1024,
        driver: String::new(),
    });
    assert_eq!(rate(&inactive, gguf, 4 * gib).1, HubFitReason::Memory);
    // A board's GPU sharing memory carries a GGUF past the processor floor;
    // a Radeon's own memory counts for GGUF through Vulkan, not for vLLM.
    let mut board = machine(8, None);
    board.cpu_threads = 2;
    board.accelerators.push(Accelerator::Vulkan {
        vram_mib: 0,
        driver: "panfrost".into(),
    });
    assert_eq!(
        rate(&board, gguf, gib / 2),
        (HardwareFit::Good, HubFitReason::Memory)
    );
    board.accelerators.clear();
    assert_eq!(
        rate(&board, gguf, gib / 2).1,
        HubFitReason::ProcessorTooSmall
    );
    let mut radeon = machine(16, None);
    radeon.accelerators.push(Accelerator::Amd {
        vram_mib: 16 * 1024,
        rocm: None,
    });
    assert_eq!(rate(&radeon, gguf, 8 * gib).1, HubFitReason::Accelerator);
    assert_eq!(rate(&radeon, snapshot, 4 * gib).1, HubFitReason::Memory);
}

fn llama_cpp() -> RuntimeRelease {
    RuntimeRelease {
        id: "llama-cpp-cpu".into(),
        kind: RuntimeKind::Cpu,
        version: "1".into(),
        artifact: PinnedArtifact {
            url: "https://example.invalid/runtime".into(),
            sha256: "a".repeat(64),
            size_bytes: 1,
        },
        oci_image: format!("example.invalid/llama@sha256:{}", "a".repeat(64)),
        execution: Some(RuntimeExecution::LlamaCpp {
            max_context_tokens: 8192,
            gpu_layers: 0,
            parallel_slots: None,
        }),
    }
}

#[test]
fn a_picked_variant_becomes_a_release_the_signed_runtime_serves() {
    let runtime = llama_cpp();
    let offer = HubModelOffer::of(
        &info(),
        &machine(32, None),
        |format| runtime.supports_format(format),
        false,
    )
    .unwrap();
    assert_eq!(offer.revision, COMMIT);
    assert_eq!(offer.context_window, 32768);
    assert!(offer.variant("Q4_K_M").unwrap().runtime_available);
    assert!(offer.variant("BF16").unwrap().runtime_available);
    assert!(!offer.variant("safetensors").unwrap().runtime_available);

    let model = UserModel::from_offer(&offer, "Q4_K_M").unwrap();
    assert_eq!(model.id, "hf/acme/chat-7b-gguf/q4_k_m");
    assert_eq!(
        model.file_url(&model.files[0]).unwrap(),
        format!("https://huggingface.co/acme/Chat-7B-GGUF/resolve/{COMMIT}/chat-7b.Q4_K_M.gguf")
    );
    let release = model.release(&"a".repeat(64));
    release.validate_shape().unwrap();
    assert!(runtime.supports_format(release.artifact_format));
    assert!(release.requires_acceptance);
    assert_eq!(release.version, COMMIT);
    assert_eq!(release.license_id, "apache-2.0");
    assert!(!release.vision && !release.face_recognition);
    assert!(UserModel::from_offer(&offer, "Q3_NOPE").is_err());

    let shards = UserModel::from_offer(&offer, "BF16").unwrap();
    assert_eq!(shards.format, ModelArtifactFormat::LlamaCppGgufShards);
    shards.release(&"b".repeat(64)).validate_shape().unwrap();
}

#[test]
fn a_signed_catalog_cannot_claim_a_per_file_layout() {
    let offer = HubModelOffer::of(&info(), &machine(32, None), |_| true, false).unwrap();
    let release = UserModel::from_offer(&offer, "safetensors")
        .unwrap()
        .release(&"c".repeat(64));
    let catalog = crate::local::LocalModelCatalog {
        runtimes: vec![],
        models: vec![release],
    };
    assert!(catalog.validate().is_err());
}

#[test]
fn records_are_checked_when_read_back() {
    let offer = HubModelOffer::of(&info(), &machine(32, None), |_| true, false).unwrap();
    let model = UserModel::from_offer(&offer, "Q8_0").unwrap();
    let mut wrong = model.clone();
    wrong.id = "HF/Upper".into();
    assert!(wrong.validate().is_err());
    let mut wrong = model.clone();
    wrong.files[0].path = "../../.ssh/id_ed25519".into();
    assert!(wrong.validate().is_err());
    let mut wrong = model.clone();
    wrong.source = UserModelSource::Hub {
        repo: "acme/x".into(),
        revision: "main".into(),
    };
    assert!(wrong.validate().is_err());
    let mut wrong = model;
    wrong.files.push(wrong.files[0].clone());
    assert!(wrong.validate().is_err());
}

#[test]
fn a_local_gguf_file_is_a_model_with_an_unknown_licence() {
    let model = UserModel::from_file("My Tune.Q4_K_M.gguf", 10, &"d".repeat(64)).unwrap();
    assert_eq!(model.id, "file/my-tune.q4_k_m");
    assert_eq!(model.source_label(), "My Tune.Q4_K_M.gguf");
    assert_eq!(model.revision(), "local");
    assert!(model.file_url(&model.files[0]).is_none());
    let release = model.release(&"d".repeat(64));
    release.validate_shape().unwrap();
    assert!(!release.requires_acceptance);
    // A name, never a path the service would open.
    assert!(UserModel::from_file("/home/me/model.gguf", 10, "").is_err());
    assert!(UserModel::from_file("model.safetensors", 10, "").is_err());
    assert!(UserModel::from_file("model.gguf", 0, "").is_err());
    // A record from before files were handed over keeps its file name.
    let legacy: UserModelSource =
        serde_json::from_str(r#"{"kind":"file","path":"old.gguf"}"#).unwrap();
    assert_eq!(
        legacy,
        UserModelSource::File {
            name: "old.gguf".into()
        }
    );
}

#[test]
fn ids_stay_in_the_runtime_alias_alphabet() {
    let id = user_model_id(
        &format!("{}/{}", "O".repeat(96), "N+ä".repeat(40)),
        "Q4_K_M",
    );
    assert!(id.len() <= 128, "{id}");
    assert!(
        id.bytes().all(|byte| byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || b"-_./".contains(&byte))
    );
}
