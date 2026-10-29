//! What `--engine` adds, to compare the engines that run apps' wasm (docs/tilolang-engines.md):
//!
//! * `compile`: compiling the package's modules in a process that has not compiled them, and
//!   `compile_peak`, how far compiling them raises the peak memory of a process of its own;
//! * `start`: opening the package's main view to its first tree in a process that has not
//!   compiled its modules, compiling included: what a launch costs;
//! * with `--compute`, `compute_sweep` and `compute_loops`: the guest code of
//!   `bench/compute`, a tilolang package doing integer arithmetic over about a million
//!   indices, once in guest calls alone and once in loops that read a list through the host;
//! * with `--library PATH`, `library_blur`: a wasm library compiled from Rust
//!   (`bench/imagefilter`) blurring a 512 x 512 image, loaded and called through the engine as
//!   an app's library would be; and `compile.library`, `compile_peak.library` for it.
//!
//! Each workload's result is in its record's meta, so the engines can be checked to agree.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use serde_json::{Value as Json, json};
use tilolang::{Compiled, IdsLock, SeededEntropy};
use tilolang_runtime::engine::{Engine, Host, HostFail, Wasm};
use tilolang_runtime::{
    AmStore, EngineConfig, EngineKind, EngineLimits, InvokeOutcome, Principal, Runtime,
    SimpleAuthority, Translation, Value,
};

use super::{BenchConfig, Record, Sized, ms};

/// The engine fields of every record.
pub(super) fn engine_meta(engine: EngineConfig) -> Json {
    let translation = match engine.translation {
        Translation::Lazy => "lazy",
        Translation::Eager => "eager",
    };
    json!({"engine": engine.kind.name(), "translation": translation})
}

fn record(
    config: &BenchConfig,
    kpi: String,
    unit: &'static str,
    samples: Vec<f64>,
    extra: Json,
) -> Record {
    let mut meta = engine_meta(config.engine);
    if let (Some(m), Json::Object(extra)) = (meta.as_object_mut(), extra) {
        m.extend(extra);
    }
    Record {
        kpi,
        unit,
        samples,
        meta,
    }
}

fn status_kib(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        // optional-decode: a benchmark probe of /proc: a kernel without the field measures nothing
        .parse()
        .ok()
}

/// What a compile child compiles, with which engine, and how it reports.
const COMPILE_MODULES: &str = "TILOLANG_BENCH_COMPILE_MODULES";
const COMPILE_ENGINE: &str = "TILOLANG_BENCH_COMPILE_ENGINE";
const COMPILE_TRANSLATION: &str = "TILOLANG_BENCH_COMPILE_TRANSLATION";
const COMPILE_LINE: &str = "tilolang-bench-compile-peak-kib=";

/// The most memory compiling `modules` adds to a process: a process of its own (the bench's
/// own has held rooms far larger than any compile) reads them, then compiles them, and reports
/// how far its peak resident memory rose. `None` without a way to start one.
fn compile_peak(config: &BenchConfig, modules: &[&[u8]]) -> Result<Option<f64>, String> {
    let Some(isolation) = &config.memory else {
        return Ok(None);
    };
    let dir = std::env::temp_dir();
    let paths: Vec<PathBuf> = (0..modules.len())
        .map(|i| dir.join(format!("tilolang-bench-{}-{i}.wasm", std::process::id())))
        .collect();
    for (path, bytes) in paths.iter().zip(modules) {
        std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    }
    let joined = std::env::join_paths(&paths).map_err(|e| e.to_string())?;
    let translation = match config.engine.translation {
        Translation::Lazy => "lazy",
        Translation::Eager => "eager",
    };
    let output = std::process::Command::new(&isolation.program)
        .args(&isolation.args)
        .env(COMPILE_MODULES, joined)
        .env(COMPILE_ENGINE, config.engine.kind.name())
        .env(COMPILE_TRANSLATION, translation)
        .output();
    for path in &paths {
        let _ = std::fs::remove_file(path);
    }
    let output = output.map_err(|e| format!("the compile process: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix(COMPILE_LINE))
        // optional-decode: a line that is not the measurement is reported below as nothing measured
        .and_then(|v| v.parse::<f64>().ok())
        .map(Some)
        .ok_or_else(|| {
            format!(
                "the compile process reported nothing: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
}

/// In a process [`compile_peak`] started: compile, print how far the peak rose, and say so.
/// `None` in any other process.
#[must_use]
pub(super) fn compile_child() -> Option<Result<(), String>> {
    let modules = std::env::var_os(COMPILE_MODULES)?;
    let measured = (|| {
        let name = std::env::var(COMPILE_ENGINE).map_err(|e| e.to_string())?;
        let kind = EngineKind::from_name(&name).ok_or_else(|| format!("no engine {name}"))?;
        let translation = match std::env::var(COMPILE_TRANSLATION).as_deref() {
            Ok("eager") => Translation::Eager,
            _ => Translation::Lazy,
        };
        tilolang_runtime::configure(EngineConfig { kind, translation })?;
        let bytes = std::env::split_paths(&modules)
            .map(|path| std::fs::read(path).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let engine = Engine::new(EngineLimits::default())?;
        let before = status_kib("VmHWM:").ok_or("the process's memory is not readable")?;
        for module in &bytes {
            engine.compile(module).map_err(|e| e.to_string())?;
        }
        let after = status_kib("VmHWM:").ok_or("the process's memory is not readable")?;
        println!("{COMPILE_LINE}{}", after.saturating_sub(before));
        Ok(())
    })();
    Some(measured)
}

/// The records the workloads asked for add before the package's scenarios: `compile` and
/// `compile_peak`, `compute_*`, `library_blur`.
pub(super) fn workloads(
    config: &BenchConfig,
    packages: &[Compiled],
    short: &str,
) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    if config.workloads.cold {
        let modules: Vec<&[u8]> = packages.iter().map(|p| p.wasm.as_slice()).collect();
        records.extend(compile_records(config, short, &modules)?);
    }
    if config.workloads.compute {
        records.extend(compute(config)?);
    }
    if let Some(path) = &config.workloads.library {
        let bytes = read_library(path)?;
        if config.workloads.cold {
            records.extend(compile_records(config, "library", &[&bytes])?);
        }
        records.push(library(config, &bytes)?);
    }
    Ok(records)
}

/// `compile` and `compile_peak`: each sample compiles the modules anew.
fn compile_records(
    config: &BenchConfig,
    name: &str,
    modules: &[&[u8]],
) -> Result<[Record; 2], String> {
    let mut times = Vec::new();
    let mut peaks = Vec::new();
    for _ in 0..config.samples {
        tilolang_runtime::engine::forget_compiled();
        let engine = Engine::new(EngineLimits::default())?;
        engine.profile(true);
        for module in modules {
            engine.compile(module).map_err(|e| e.to_string())?;
        }
        times.push(ms(engine.profiled().unwrap_or_default().compiling));
        peaks.extend(compile_peak(config, modules)?);
    }
    let bytes: usize = modules.iter().map(|m| m.len()).sum();
    let extra = json!({"modules": modules.len(), "bytes": bytes});
    Ok([
        record(
            config,
            format!("tilolang.compile.{name}"),
            "ms",
            times,
            extra.clone(),
        ),
        record(
            config,
            format!("tilolang.compile_peak.{name}"),
            "KiB",
            peaks,
            extra,
        ),
    ])
}

/// `start`: each sample opens the room in a process that has forgotten the modules it compiled.
pub(super) fn start(sized: &Sized<'_>) -> Result<Record, String> {
    let mut samples = Vec::new();
    for _ in 0..sized.config.samples {
        tilolang_runtime::engine::forget_compiled();
        let started = Instant::now();
        let room = sized.open()?;
        samples.push(ms(started.elapsed()));
        drop(room);
    }
    Ok(sized.record("start", "ms", samples, Json::Null))
}

/// The tilolang package of the compute workload, compiled into the bench so it runs from
/// anywhere.
const COMPUTE: &str = include_str!("../../bench/compute/app.tl");

/// The compute workload's package (`bench/compute`), compiled.
///
/// # Errors
/// It does not compile.
pub fn compute_package() -> Result<Compiled, String> {
    tilolang::compile_package_with(COMPUTE, &IdsLock::new(), &[], &mut SeededEntropy(1))
        .map_err(|diagnostics| format!("bench/compute: {diagnostics:?}"))
}

/// Indices `compute_sweep` runs `mix` over; `compute_loops` runs it over 32^4, the same.
const SWEEP: i64 = 1 << 20;

/// `compute_sweep` and `compute_loops`.
fn compute(config: &BenchConfig) -> Result<[Record; 2], String> {
    let package = compute_package()?;
    let action = |name: &str| {
        package
            .meta
            .actions
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.id.clone())
            .ok_or_else(|| format!("bench/compute has no action {name}"))
    };
    let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).map_err(|e| e.to_string())?;
    rt.load_wasm(&package.wasm).map_err(|e| e.to_string())?;
    let lab = rt.create("Lab").map_err(|e| e.to_string())?;
    let who = Principal::new("bench");
    let timed = |id: &str, args: Vec<Value>| -> Result<(Vec<f64>, Json), String> {
        let mut samples = Vec::new();
        let mut result = Json::Null;
        for _ in 0..config.samples {
            let started = Instant::now();
            let out = rt
                .invoke_action(&who, (id, args.clone()))
                .map_err(|e| e.to_string())?;
            samples.push(ms(started.elapsed()));
            if out != InvokeOutcome::Done(Value::Unit) {
                return Err(format!("{id}: {out:?}"));
            }
            result = rt.project(&rt.read(&lab, "result").map_err(|e| e.to_string())?);
        }
        Ok((samples, result))
    };
    let (sweep, sweep_result) = timed(
        &action("sweep_range")?,
        vec![Value::resource(&lab), Value::Int(SWEEP)],
    )?;
    let (loops, loops_result) = timed(&action("nested_loops")?, vec![Value::resource(&lab)])?;
    Ok([
        record(
            config,
            "tilolang.compute_sweep".into(),
            "ms",
            sweep,
            json!({"indices": SWEEP, "result": sweep_result}),
        ),
        record(
            config,
            "tilolang.compute_loops".into(),
            "ms",
            loops,
            json!({"indices": SWEEP, "result": loops_result}),
        ),
    ])
}

/// A library imports nothing from the host.
struct NoImports;

impl Host for NoImports {
    fn import(&self, id: usize, _: &[u8], _: &[Wasm]) -> Result<Option<Wasm>, HostFail> {
        Err(HostFail::Trap(format!("the library called import {id}")))
    }
}

/// The image `library_blur` blurs, and how often.
const IMAGE: (i32, i32) = (512, 512);
const PASSES: i32 = 4;

/// `library_blur`: each sample makes the image anew (not timed) and blurs it.
fn read_library(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| {
        format!(
            "{}: {e}; build it with crates/platform/tilolang-tilo/bench/imagefilter/build.sh",
            path.display()
        )
    })
}

fn library(config: &BenchConfig, bytes: &[u8]) -> Result<Record, String> {
    let engine = Engine::new(EngineLimits::default())?;
    let module = engine.compile(bytes).map_err(|e| e.to_string())?;
    let host: Rc<dyn Host> = Rc::new(NoImports);
    let call = |export: &str, args: &[Wasm]| {
        engine
            .call(&module, export, args, Rc::clone(&host))
            .map_err(|e| format!("{export}: {e}"))
    };
    let mut samples = Vec::new();
    let mut checksum = None;
    for _ in 0..config.samples {
        if call(
            "fill",
            &[Wasm::I32(IMAGE.0), Wasm::I32(IMAGE.1), Wasm::I32(7)],
        )? != Some(Wasm::I32(0))
        {
            return Err("fill refused the image".into());
        }
        let started = Instant::now();
        let out = call("blur", &[Wasm::I32(PASSES)])?;
        samples.push(ms(started.elapsed()));
        checksum = match out {
            Some(Wasm::I64(sum)) => Some(sum),
            other => return Err(format!("blur returned {other:?}")),
        };
    }
    Ok(record(
        config,
        "tilolang.library_blur".into(),
        "ms",
        samples,
        json!({
            "width": IMAGE.0,
            "height": IMAGE.1,
            "passes": PASSES,
            "bytes": bytes.len(),
            "checksum": checksum,
        }),
    ))
}
