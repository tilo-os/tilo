//! What the crash reports publish to programs: the reports this computer recorded (`reports` of
//! `tilo.crash-reports`), each what happened, when, and what the kernel met, and one report's
//! text (`crash-report:<id>`) with the stacks it kept. A report is read, never edited; the list
//! offers one change, removing a report from this computer.
//!
//! The file beside this crate ([`crash-reports-surface.json`]) is what `tilolang check` compiles
//! a view against and a host reads the package from; the test here writes it, so the two cannot
//! drift.

#[cfg(test)]
mod tests {
    use tilo_actions::field_type::{FieldType, NamedType, TypeCase, TypeField};
    use tilo_actions::surface::{FieldSurface, ParamSurface, Surface, VerbSurface};
    use tilo_resource::Effect as _;

    fn reading(path: &str, ty: FieldType, title: &str) -> FieldSurface {
        FieldSurface {
            path: path.to_owned(),
            owner: String::new(),
            title: title.to_owned(),
            stored: "value".to_owned(),
            ty,
            readonly: true,
            unit: false,
            label: false,
            whole: false,
            ops: None,
        }
    }

    fn named(name: &str) -> FieldType {
        FieldType::Named(name.to_owned())
    }

    fn option(ty: FieldType) -> FieldType {
        FieldType::Option(Box::new(ty))
    }

    fn list(ty: FieldType) -> FieldType {
        FieldType::List(Box::new(ty))
    }

    fn record(name: &str, doc: &str, fields: &[(&str, FieldType, &str)]) -> NamedType {
        NamedType {
            name: name.to_owned(),
            doc: doc.to_owned(),
            fields: fields
                .iter()
                .map(|(name, ty, title)| TypeField {
                    name: (*name).to_owned(),
                    ty: ty.clone(),
                    title: (*title).to_owned(),
                })
                .collect(),
            cases: Vec::new(),
        }
    }

    fn enumeration(name: &str, doc: &str, cases: &[(&str, &str)]) -> NamedType {
        NamedType {
            name: name.to_owned(),
            doc: doc.to_owned(),
            fields: Vec::new(),
            cases: cases
                .iter()
                .map(|(name, label)| TypeCase {
                    name: (*name).to_owned(),
                    label: (*label).to_owned(),
                    fields: Vec::new(),
                })
                .collect(),
        }
    }

    /// What a report says happened, one case per value the reading serialises the kind as.
    fn kind() -> NamedType {
        enumeration(
            "CrashKind",
            "What happened.",
            &[
                ("froze", "The desktop stopped responding"),
                ("crashed", "The desktop stopped unexpectedly"),
                ("part-restarted", "Part of the desktop restarted"),
                ("computer-stopped", "The computer stopped"),
                (
                    "system-fault",
                    "The system ran into a fault and kept running",
                ),
            ],
        )
    }

    /// What the kernel met, one case per value the reading serialises it as.
    fn reason() -> NamedType {
        enumeration(
            "StopReason",
            "What the system met.",
            &[
                ("panic", "The system met an error it could not recover from"),
                ("oops", "A fault in the system's core"),
                ("emergency", "The system restarted itself at once"),
            ],
        )
    }

    fn report() -> NamedType {
        record(
            "CrashReport",
            "One report, without its text.",
            &[
                ("id", FieldType::Text, "The report's name."),
                (
                    "at",
                    FieldType::Int,
                    "When it happened, in seconds since the Unix epoch.",
                ),
                (
                    "when",
                    FieldType::Text,
                    "When it happened, as this computer reads a clock.",
                ),
                ("kind", named("CrashKind"), "What happened."),
                (
                    "reason",
                    option(named("StopReason")),
                    "What the kernel met, when known.",
                ),
                (
                    "version",
                    option(FieldType::Text),
                    "The version that wrote the report.",
                ),
                (
                    "build",
                    option(FieldType::Text),
                    "The exact build that wrote it.",
                ),
            ],
        )
    }

    /// The verbs the reports take, as the crate offers them: a report is read, and removed from
    /// this computer once the person asks.
    fn verbs() -> Vec<VerbSurface> {
        crate::service::CrashReportsAction::specs()
            .iter()
            .map(|action| {
                let params = action
                    .params()
                    .iter()
                    .map(|param| ParamSurface {
                        name: param.name.to_string(),
                        required: param.required,
                        schema: serde_json::Value::Null,
                        ty: tilo_actions::surface::param_type(param),
                        within: None,
                    })
                    .collect();
                tilo_actions::surface::verb_of(action, params)
            })
            .collect()
    }

    /// The reports this computer recorded, and one report's text, as a view of another language
    /// reads them.
    #[must_use]
    pub fn surfaces() -> Vec<Surface> {
        vec![
            Surface {
                kind: "crash-reports".to_owned(),
                label: "Crash reports".to_owned(),
                version: 1,
                person: false,
                readers: None,
                verbs: verbs(),
                fields: vec![reading(
                    "reports",
                    list(named("CrashReport")),
                    "Every report, newest first.",
                )],
                types: vec![report(), kind(), reason()],
                shows: Vec::new(),
                live: None,
                setters: false,
            },
            Surface {
                kind: "crash-report-text".to_owned(),
                label: "Crash report".to_owned(),
                version: 1,
                person: false,
                readers: None,
                verbs: Vec::new(),
                fields: vec![
                    reading("report", FieldType::Any, "The report's facts."),
                    reading(
                        "text",
                        FieldType::Text,
                        "The report as it was written, its stacks included.",
                    ),
                    reading(
                        "truncated",
                        FieldType::Bool,
                        "Whether the text is the start of a longer report.",
                    ),
                ],
                types: Vec::new(),
                shows: Vec::new(),
                live: None,
                setters: false,
            },
        ]
    }

    /// The file a tilolang view compiles the reports against is what this crate serves.
    /// `UPDATE_SNAPSHOTS=1` writes it.
    #[test]
    fn the_crash_reports_surface_file_is_what_this_crate_publishes() {
        let text = tilo_surface_compile::canonical(&serde_json::json!({ "resources": surfaces() }));
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("crash-reports-surface.json");
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
            std::fs::write(&path, &text).expect("the surface file is written");
            return;
        }
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some(text.as_str()),
            "crates/services/crash-reports/crash-reports-surface.json is not what this crate \
             publishes; regenerate it with UPDATE_SNAPSHOTS=1"
        );
    }
}
