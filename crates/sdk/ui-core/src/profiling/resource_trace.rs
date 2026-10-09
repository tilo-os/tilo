//! Opt-in resource-refresh diagnostics (`TILO_PROFILE_RESOURCES=1`).
//!
//! Each `tilo-resource-stage ` stderr line contains one JSON object. Durations
//! are CPU wall time for disjoint stages, excluding serialization/logging and
//! child stages. Cache lookup rows report the actual hit/miss; work stages
//! report `not_applicable`. Bound discovery owns an internal cache whose result
//! is not exposed here, so its result is explicitly `not_observed`.
//! Resource and permission generations belong to the App, not a frame counter.
//! The environment switch is read once per process. Disabled spans read no
//! clock and serialize nothing. Diagnostic I/O can perturb subsequent timing.

use crate::time::Instant;
use std::sync::OnceLock;

use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stage {
    ResourceCache,
    DeclarationGeneration,
    SemanticConstruction,
    SurfaceValidation,
    LocalCache,
    LocalPermissionProjection,
    BoundDiscovery,
    BoundControlProjection,
    ControlCache,
    ControlRefresh,
    /// Deciding whether an input kept the published tree: `hit` kept it.
    #[cfg(feature = "native")]
    InputCarry,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CacheResult {
    Hit,
    Miss,
    NotApplicable,
    NotObserved,
}

#[derive(Debug, Serialize)]
struct Row {
    stage: Stage,
    duration_us: u64,
    cache_result: CacheResult,
    generation: u64,
    permission_generation: u64,
}

pub(crate) struct Span {
    start: Option<Instant>,
    stage: Stage,
    generation: u64,
    permission_generation: u64,
}

impl Span {
    pub(crate) fn start(stage: Stage, generation: u64, permission_generation: u64) -> Self {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        let enabled = *ENABLED.get_or_init(|| {
            std::env::var("TILO_PROFILE_RESOURCES").is_ok_and(|value| value == "1")
        });
        Self::with_enabled(stage, generation, permission_generation, enabled)
    }

    fn with_enabled(
        stage: Stage,
        generation: u64,
        permission_generation: u64,
        enabled: bool,
    ) -> Self {
        Self {
            start: enabled.then(crate::time::now),
            stage,
            generation,
            permission_generation,
        }
    }

    fn into_row(self, cache_result: CacheResult) -> Option<Row> {
        self.start.map(|start| Row {
            stage: self.stage,
            duration_us: u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX),
            cache_result,
            generation: self.generation,
            permission_generation: self.permission_generation,
        })
    }

    pub(crate) fn finish(self, cache_result: CacheResult) {
        if let Some(row) = self.into_row(cache_result)
            && let Ok(json) = serde_json::to_string(&row)
        {
            eprintln!("tilo-resource-stage {json}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_traces_are_free_when_disabled_and_survive_serialisation() {
        // phase: disabled_spans_have_no_clock_or_record
        {
            let span = Span::with_enabled(Stage::ResourceCache, 9, 3, false);
            assert!(span.start.is_none());
            assert!(span.into_row(CacheResult::Hit).is_none());
        }
        // phase: cache_results_and_generations_survive_serialization
        {
            for (stage, stage_name, result, result_name) in [
                (
                    Stage::ResourceCache,
                    "resource_cache",
                    CacheResult::Hit,
                    "hit",
                ),
                (Stage::LocalCache, "local_cache", CacheResult::Miss, "miss"),
                (
                    Stage::ControlRefresh,
                    "control_refresh",
                    CacheResult::NotApplicable,
                    "not_applicable",
                ),
                (
                    Stage::BoundDiscovery,
                    "bound_discovery",
                    CacheResult::NotObserved,
                    "not_observed",
                ),
            ] {
                let row = Span::with_enabled(stage, 9, 3, true)
                    .into_row(result)
                    .expect("enabled row");
                let json = serde_json::to_value(row).expect("serialize row");
                assert_eq!(json["stage"], stage_name);
                assert_eq!(json["cache_result"], result_name);
                assert_eq!(json["generation"], 9);
                assert_eq!(json["permission_generation"], 3);
                assert!(json["duration_us"].as_u64().is_some());
            }
        }
    }
}
