//! Shared scenario registry and command-line dispatcher for the three UI
//! example surfaces.

#![expect(
    dead_code,
    reason = "each example binary includes this registry and constructs only its own surface"
)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Lab,
    Shots,
    Perf,
}

impl Surface {
    fn label(self) -> &'static str {
        match self {
            Self::Lab => "ui-lab",
            Self::Shots => "ui-shots",
            Self::Perf => "ui-perf",
        }
    }
}

pub struct Scenario {
    pub name: &'static str,
    pub source: &'static str,
    pub summary: &'static str,
    pub run: fn(&[String]),
}

impl Scenario {
    pub const fn new(
        name: &'static str,
        source: &'static str,
        summary: &'static str,
        run: fn(&[String]),
    ) -> Self {
        Self {
            name,
            source,
            summary,
            run,
        }
    }
}

macro_rules! scenario_modules {
    ($surface:ident; $( $module:ident, $source:literal, $summary:literal );+ $(;)?) => {
        $(
            #[path = $source]
            mod $module;
        )+

        static SCENARIOS: &[$crate::registry::Scenario] = &[
            $(
                $crate::registry::Scenario::new(
                    stringify!($module),
                    $source,
                    $summary,
                    $module::run,
                ),
            )+
        ];

        #[cfg(test)]
        mod scenario_registry_tests {
            #[test]
            fn registry_is_unique_and_complete() -> Result<(), String> {
                $crate::registry::validate(
                    $crate::registry::Surface::$surface,
                    super::SCENARIOS,
                )
            }
        }
    };
}

pub(crate) use scenario_modules;

const LAB_SOURCES: &[&str] = &[
    "activity_timeline.rs",
    "chart_gallery.rs",
    "dashboard.rs",
    "gallery.rs",
    "responsive_gallery.rs",
];
const SHOT_SOURCES: &[&str] = &[
    "chart_gallery_shots.rs",
    "content_shots.rs",
    "ctrl_k_palette.rs",
    "data_table.rs",
    "denied_state_shots.rs",
    "design_gallery_shots.rs",
    "fractional_scale_shots.rs",
    "floating_surface_shots.rs",
    "gallery_harness_shots.rs",
    "gallery_shots.rs",
    "history_cases.rs",
    "inline_emoji_shots.rs",
    "nav_shell_shots.rs",
    "perf_hud_shot.rs",
    "scrollbar_shots.rs",
    "search_showcase.rs",
    "soft_shadow_shots.rs",
    "table_shots.rs",
    "text_shots.rs",
    "textinput_ext_shots.rs",
    "undo_controls_shots.rs",
    "widget_states_shots.rs",
    "window_chrome_shots.rs",
];
const PERF_SOURCES: &[&str] = &[
    "dashboard_perf_bench.rs",
    "perf_scenario.rs",
    "widget_perf_bench.rs",
];

fn expected(surface: Surface) -> &'static [&'static str] {
    match surface {
        Surface::Lab => LAB_SOURCES,
        Surface::Shots => SHOT_SOURCES,
        Surface::Perf => PERF_SOURCES,
    }
}

/// Prove that a surface has exactly one named entry for every implementation
/// assigned to it, with no duplicate names or source files.
pub fn validate(surface: Surface, scenarios: &[Scenario]) -> Result<(), String> {
    let expected = expected(surface);
    if scenarios.len() != expected.len() {
        return Err(format!(
            "{} registry has {} entries; expected {}",
            surface.label(),
            scenarios.len(),
            expected.len()
        ));
    }
    for (index, scenario) in scenarios.iter().enumerate() {
        if scenario.name.is_empty() || scenario.summary.is_empty() {
            return Err(format!("registry entry {index} has empty metadata"));
        }
        // `index` comes from `enumerate()` over this same slice, so it is
        // always in bounds.
        let seen = scenarios.get(..index).unwrap_or_default();
        if seen.iter().any(|other| other.name == scenario.name) {
            return Err(format!("duplicate scenario name {:?}", scenario.name));
        }
        if seen.iter().any(|other| other.source == scenario.source) {
            return Err(format!("duplicate scenario source {:?}", scenario.source));
        }
        if !expected.contains(&scenario.source) {
            return Err(format!(
                "unexpected {} source {:?}",
                surface.label(),
                scenario.source
            ));
        }
    }
    for source in expected {
        if !scenarios.iter().any(|scenario| scenario.source == *source) {
            return Err(format!("missing {} scenario for {source}", surface.label()));
        }
    }
    Ok(())
}

/// Report a scenario setup/rendering failure to its command-line caller.
#[allow(
    clippy::exit,
    reason = "scenario runners are CLI binaries and must return a failing status when rendering fails"
)]
pub fn fatal(message: impl std::fmt::Display) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

/// The capture theme every `--shots` honours (`TILO_SHOTS_THEME`,
/// `TILO_SHOTS_TEXT_SCALE`, …). An unknown theme name is fatal, never a
/// silent dark shot.
pub fn shot_theme() -> tilo_ui::theme::Theme {
    tilo_ui::launch::shot_theme_from_env().unwrap_or_else(|error| fatal(error))
}

#[allow(
    clippy::exit,
    reason = "dispatch is the shared entry point every example's fn main calls directly; \
              returning an exit code instead would mean threading it through every example \
              binary's main for no behavioural difference"
)]
pub fn dispatch(surface: Surface, scenarios: &[Scenario], default: Option<&str>) {
    if let Err(error) = validate(surface, scenarios) {
        eprintln!("{}: invalid registry: {error}", surface.label());
        std::process::exit(2);
    }

    let mut args = std::env::args().skip(1);
    let first = args.next();
    if matches!(first.as_deref(), Some("--list" | "-l")) {
        print_catalog(surface, scenarios);
        return;
    }
    if matches!(first.as_deref(), Some("--help" | "-h")) {
        print_usage(surface, scenarios);
        return;
    }
    let name = if first.as_deref() == Some("--scenario") {
        args.next()
            .unwrap_or_else(|| usage_error(surface, scenarios, "--scenario needs a name"))
    } else if let Some(name) = first {
        name
    } else if let Some(name) = default {
        name.to_owned()
    } else {
        print_catalog(surface, scenarios);
        return;
    };
    let remaining: Vec<String> = args.collect();
    let Some(scenario) = scenarios.iter().find(|scenario| scenario.name == name) else {
        usage_error(surface, scenarios, &format!("unknown scenario {name:?}"))
    };
    (scenario.run)(&remaining);
}

#[allow(
    clippy::exit,
    reason = "returns `!` for a bad CLI arg the caller cannot recover from; propagating an \
              exit code instead would mean threading it through every example binary's main \
              for no behavioural difference"
)]
fn usage_error(surface: Surface, scenarios: &[Scenario], message: &str) -> ! {
    eprintln!("{}: {message}", surface.label());
    print_usage(surface, scenarios);
    std::process::exit(2);
}

fn print_usage(surface: Surface, scenarios: &[Scenario]) {
    eprintln!(
        "usage: cargo run -p ui-examples --example {} -- [--list | SCENARIO [ARGS...]]",
        surface.label()
    );
    print_catalog(surface, scenarios);
}

fn print_catalog(surface: Surface, scenarios: &[Scenario]) {
    println!("{} scenarios:", surface.label());
    for scenario in scenarios {
        println!("  {:<28} {}", scenario.name, scenario.summary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn noop(_: &[String]) {}

    #[test]
    fn duplicate_names_and_sources_are_rejected() {
        let duplicate_name = [
            Scenario::new("gallery", "gallery.rs", "one", noop),
            Scenario::new("gallery", "dashboard.rs", "two", noop),
        ];
        assert!(validate(Surface::Lab, &duplicate_name).is_err());

        let duplicate_source = [
            Scenario::new("gallery", "gallery.rs", "one", noop),
            Scenario::new("dashboard", "gallery.rs", "two", noop),
        ];
        assert!(validate(Surface::Lab, &duplicate_source).is_err());
    }

    /// Example binary roots that are not scenario modules. The `ui_*`
    /// dispatchers and `registry.rs` are filtered by name; these are the
    /// remaining `[[example]]` entry points declared in Cargo.toml.
    const STANDALONE_ENTRYPOINTS: &[&str] = &[
        "block_editor_typing.rs",
        "card_composition_shots.rs",
        "distinct_letters_shots.rs",
        "distinct_letters_specimen.rs",
        "form_composition_shots.rs",
        "managed_history_probe.rs",
        "manual_composition_shots.rs",
        "multiline_typing.rs",
        "native_file_picker.rs",
        "native_input_probe.rs",
        "reading_face_shots.rs",
        "search_latency.rs",
        "type_specimen_shots.rs",
        "windows_host.rs",
    ];

    #[test]
    fn every_implementation_file_is_catalogued() {
        let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        let actual: BTreeSet<String> = std::fs::read_dir(examples)
            .expect("examples directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?.to_owned();
                (!name.starts_with("ui_")
                    && name != "registry.rs"
                    && !STANDALONE_ENTRYPOINTS.contains(&name.as_str()))
                .then_some(name)
            })
            .collect();
        let catalogued: BTreeSet<String> = [LAB_SOURCES, SHOT_SOURCES, PERF_SOURCES]
            .into_iter()
            .flatten()
            .map(|source| (*source).to_owned())
            .collect();
        assert_eq!(actual, catalogued);
    }
}
