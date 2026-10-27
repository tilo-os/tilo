//! Plain logic in a program: comparisons, arithmetic, text, and the bounded
//! loops that read a list a step produced.

use serde_json::{Value, json};
use tilo_action_policy::Rule;
use tilo_actions::contract::{ActionContract, ResultSpec};
use tilo_actions::remote::InvokeArgument;

use super::{
    FakeHost, Hooks, INBOX, NOTES, app_rule, failure, invoke, list, row, run_with, shortcut_over,
};
use crate::expression::{
    Expr, ExprError, Member, Op, arithmetic, compare, field, length, map_field, negate, replace,
    sort, split, take, truthy, type_name,
};
use crate::run::MAX_REPEAT;
use crate::shortcut::Condition;
use crate::{
    Binding, CheckErrorKind, RunId, RunMode, RunOutcome, RunReport, Selector, Shortcut, Step,
    StepFailure, StepKind, check,
};
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::CatalogueRow;

const FILES: &str = "so.tilo.files";
const FOLDER: &str = "files.folder:inbox";

/// The grant a program that reaches the files and notes apps carries.
fn grant() -> Vec<Rule> {
    vec![app_rule(FILES), app_rule(NOTES)]
}

/// The shortcut these tests run: they read a folder, so their grant says so.
fn over_files(steps: Vec<Step>) -> Shortcut {
    shortcut_over("tidy", steps, grant())
}

/// The shape a folder listing answers: what is in it, each file with its
/// size.
fn listing() -> Value {
    json!({
        "type": "object",
        "properties": {
            "files": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "size_mb": {"type": "number"}
                    },
                    "additionalProperties": false
                }
            }
        },
        "additionalProperties": false
    })
}

/// What a folder listing answers here: one small file, and two a person
/// would want compressed.
fn listing_answer() -> Value {
    json!({
        "files": [
            {"name": "notes.txt", "size_mb": 0.1},
            {"name": "big.iso", "size_mb": 42},
            {"name": "huge.mov", "size_mb": 700}
        ]
    })
}

fn files_catalog() -> CatalogueRow {
    row(
        FILES,
        FOLDER,
        "files.folder",
        "list",
        Vec::new(),
        returning(ResultSpec::Json { schema: listing() }),
    )
}

/// A row for the folder's compress action: it takes the file to compress and
/// answers no value.
fn compress_row() -> CatalogueRow {
    row(
        FILES,
        FOLDER,
        "files.folder",
        "compress",
        vec![ActionParam::required(
            "file",
            ParamType::Object,
            "The file to compress",
        )],
        ActionContract::default(),
    )
}

fn returning(result: ResultSpec) -> ActionContract {
    ActionContract {
        result,
        ..ActionContract::default()
    }
}

/// A host offering the files app beside the notes app the shared fixtures
/// already build.
fn files_host() -> FakeHost {
    let mut host = FakeHost::new();
    host.add_row(files_catalog());
    host.add_row(compress_row());
    host
}

/// The folder the listing step reads.
fn folder() -> Selector {
    Selector::Fixed(FOLDER.into())
}

/// The files the listing step answered, as a list of dictionaries.
fn files() -> Expr {
    Expr::Field {
        value: Box::new(Expr::Value {
            binding: Binding::step("list", ""),
        }),
        field: "files".into(),
    }
}

/// The size of one file of a listing, read as `file.size_mb`.
fn size_of(file: Expr) -> Expr {
    Expr::Field {
        value: Box::new(file),
        field: "size_mb".into(),
    }
}

/// `file.size_mb` for the file a filter is judging.
fn size() -> Expr {
    size_of(Expr::Value {
        binding: Binding::Var("file".into()),
    })
}

/// Comparand: the size threshold a listing is filtered at.
fn ten() -> Expr {
    Expr::Value {
        binding: Binding::Literal(json!(10)),
    }
}

fn comparing(op: Op, left: Expr, right: Expr) -> Expr {
    Expr::Compare {
        op,
        left: Box::new(left),
        right: Box::new(right),
    }
}

/// The reasons the checker refuses `program`, per step path.
fn refused(host: &FakeHost, steps: Vec<Step>) -> Vec<(String, CheckErrorKind)> {
    let errors = check(&over_files(steps), &host.catalog(), &crate::NoShortcuts).unwrap_err();
    errors
        .into_iter()
        .map(|error| (error.path.as_str().to_owned(), error.kind))
        .collect()
}

/// Whether `reasons` holds an expression error whose text contains `needle`.
fn says(reasons: &[(String, CheckErrorKind)], needle: &str) -> bool {
    reasons.iter().any(|(_, kind)| match kind {
        CheckErrorKind::Expression(reason) => reason.contains(needle),
        other => format!("{other}").contains(needle),
    })
}

fn completed(report: &RunReport) -> bool {
    matches!(report.outcome, RunOutcome::Completed)
}

/// The paths of the steps a report recorded, in order.
fn paths(report: &RunReport) -> Vec<String> {
    report
        .steps
        .iter()
        .map(|record| record.path.as_str().to_owned())
        .collect()
}

/// The report an unattended run of this program produced.
fn ran(host: &FakeHost, steps: Vec<Step>) -> RunReport {
    let hooks = Hooks::default();
    run_with(
        host,
        &hooks,
        &over_files(steps),
        RunId::new(),
        RunMode::Unattended,
    )
}

// ---------------------------------------------------------------------------
// The program the issue asks for, written without any AI step.
// ---------------------------------------------------------------------------

#[test]
fn a_filter_and_a_loop_compress_the_large_files_and_collect_their_names() {
    let host = files_host();
    // For each file in a folder larger than 10 MB, compress it and collect
    // the names.
    let program = vec![
        invoke("list", FILES, folder(), "list", &[]),
        Step::new(
            "large",
            StepKind::Compute {
                expr: Expr::Filter {
                    items: Box::new(files()),
                    test: Box::new(comparing(Op::Greater, size(), ten())),
                },
            },
        ),
        Step::new(
            "compress",
            StepKind::ForEach {
                items: Binding::step("large", ""),
                item: "file".into(),
                body: vec![invoke(
                    "compress-one",
                    FILES,
                    folder(),
                    "compress",
                    &[("file", Binding::Var("file".into()))],
                )],
            },
        ),
        Step::new(
            "names",
            StepKind::Compute {
                expr: Expr::Map {
                    items: Box::new(Expr::Value {
                        binding: Binding::step("large", ""),
                    }),
                    field: "name".into(),
                },
            },
        ),
        Step::new(
            "finish",
            StepKind::Return {
                value: Some(Binding::step("names", "")),
            },
        ),
    ];
    let errors = check(
        &over_files(program.clone()),
        &host.catalog(),
        &crate::NoShortcuts,
    );
    assert!(errors.is_ok(), "{errors:?}");

    host.answer_files(listing_answer());
    let report = ran(&host, program);
    assert!(completed(&report), "{:?}", report.outcome);
    assert_eq!(report.output, Some(json!(["big.iso", "huge.mov"])));
    assert_eq!(
        paths(&report),
        ["0", "1", "2", "2.each[0].0", "2.each[1].0", "3", "4",]
    );
    let compressed = host
        .started()
        .into_iter()
        .filter(|request| request.target.address.action == "compress")
        .count();
    assert_eq!(compressed, 2, "one compress per large file");
}

// ---------------------------------------------------------------------------
// What the checker refuses before anything runs.
// ---------------------------------------------------------------------------

#[test]
fn a_comparison_between_incompatible_types_is_refused_at_save() {
    let host = files_host();
    // A size against a text: neither is the other's kind, so no answer could
    // be right whatever the listing holds.
    let program = vec![
        invoke("list", FILES, folder(), "list", &[]),
        Step::new(
            "large",
            StepKind::Compute {
                expr: Expr::Filter {
                    items: Box::new(files()),
                    test: Box::new(comparing(
                        Op::Greater,
                        size(),
                        Expr::Value {
                            binding: Binding::Literal(json!("ten")),
                        },
                    )),
                },
            },
        ),
    ];
    let reasons = refused(&host, program);
    assert!(
        says(&reasons, "is not comparable with"),
        "a number ordered against a text must be refused: {reasons:?}"
    );

    // The same program with a number on both sides is accepted.
    let program = vec![
        invoke("list", FILES, folder(), "list", &[]),
        Step::new(
            "large",
            StepKind::Compute {
                expr: Expr::Filter {
                    items: Box::new(files()),
                    test: Box::new(comparing(Op::Greater, size(), ten())),
                },
            },
        ),
    ];
    assert!(check(&over_files(program), &host.catalog(), &crate::NoShortcuts).is_ok());
}

#[test]
fn a_field_a_result_does_not_have_is_refused_at_save() {
    let host = files_host();
    let program = vec![
        invoke("list", FILES, folder(), "list", &[]),
        Step::new(
            "biggest",
            StepKind::Compute {
                expr: Expr::Map {
                    items: Box::new(files()),
                    field: "bytes".into(),
                },
            },
        ),
    ];
    let reasons = refused(&host, program);
    assert!(
        says(&reasons, "`bytes`"),
        "a map over a field the files do not have must be refused: {reasons:?}"
    );
}

#[test]
fn a_repeat_past_the_limit_is_refused() {
    let host = FakeHost::new();
    let program = vec![Step::new(
        "again",
        StepKind::Repeat {
            count: Binding::Literal(json!(MAX_REPEAT + 1)),
            index: "n".into(),
            body: vec![list()],
        },
    )];
    let reasons = refused(&host, program);
    assert!(
        reasons.iter().any(|(path, kind)| path == "0"
            && matches!(kind, CheckErrorKind::NotCountable(reason) if reason.contains("10000"))),
        "{reasons:?}"
    );
}

#[test]
fn a_while_without_a_limit_is_refused() {
    let host = FakeHost::new();
    let program = vec![Step::new(
        "again",
        StepKind::While {
            cond: Condition::Holds(Expr::Truthy {
                value: Box::new(Expr::Value {
                    binding: Binding::Literal(json!(true)),
                }),
            }),
            max: 0,
            body: vec![list()],
        },
    )];
    let reasons = refused(&host, program);
    assert!(
        reasons
            .iter()
            .any(|(_, kind)| matches!(kind, CheckErrorKind::UnboundedLoop)),
        "{reasons:?}"
    );
}

// ---------------------------------------------------------------------------
// What a run does with the new steps.
// ---------------------------------------------------------------------------

#[test]
fn a_repeat_counts_its_iterations_and_names_each_one() {
    let host = FakeHost::new();
    let program = vec![
        list(),
        Step::new(
            "notes",
            StepKind::SetVar {
                name: "made".into(),
                value: Binding::List(Vec::new()),
            },
        ),
        Step::new(
            "again",
            StepKind::Repeat {
                count: Binding::Literal(json!(3)),
                index: "n".into(),
                body: vec![invoke(
                    "create-one",
                    NOTES,
                    Selector::Fixed(INBOX.into()),
                    "create",
                    &[(
                        "title",
                        Binding::Expression(Box::new(Expr::Math {
                            op: Op::Concat,
                            left: Box::new(Expr::Value {
                                binding: Binding::Literal(json!("pass ")),
                            }),
                            right: Box::new(Expr::Value {
                                binding: Binding::Var("n".into()),
                            }),
                        })),
                    )],
                )],
            },
        ),
    ];
    let report = ran(&host, program);
    assert!(completed(&report), "{:?}", report.outcome);
    assert_eq!(
        paths(&report),
        [
            "0",
            "1",
            "2",
            "2.repeat[0].0",
            "2.repeat[1].0",
            "2.repeat[2].0"
        ]
    );
    let titles = host
        .started()
        .into_iter()
        .filter(|request| request.target.address.action == "create")
        .map(|request| match request.argument {
            InvokeArgument::Json(args) => args["title"].clone(),
            _ => Value::Null,
        })
        .collect::<Vec<_>>();
    assert_eq!(titles, [json!("pass 0"), json!("pass 1"), json!("pass 2")]);
}

#[test]
fn a_while_that_never_stops_fails_at_its_limit() {
    let host = FakeHost::new();
    let program = vec![
        list(),
        Step::new(
            "again",
            StepKind::While {
                cond: Condition::Holds(Expr::Truthy {
                    value: Box::new(Expr::Value {
                        binding: Binding::Literal(json!(true)),
                    }),
                }),
                max: 3,
                body: vec![Step::new(
                    "tick",
                    StepKind::Compute {
                        expr: Expr::Value {
                            binding: Binding::Literal(json!(1)),
                        },
                    },
                )],
            },
        ),
    ];
    let report = ran(&host, program);
    let Some((path, failure)) = failure(&report) else {
        panic!("a loop that never stops must fail: {:?}", report.outcome);
    };
    assert_eq!(path.as_str(), "1");
    assert!(
        matches!(failure, StepFailure::LoopExhausted { limit: 3 }),
        "{failure:?}"
    );
}

#[test]
fn a_repeat_past_the_limit_fails_the_step() {
    let host = FakeHost::new();
    let program = vec![Step::new(
        "again",
        StepKind::Repeat {
            count: Binding::Expression(Box::new(Expr::Math {
                op: Op::Multiply,
                left: Box::new(Expr::Value {
                    binding: Binding::Literal(json!(MAX_REPEAT)),
                }),
                right: Box::new(Expr::Value {
                    binding: Binding::Literal(json!(2)),
                }),
            })),
            index: "n".into(),
            body: vec![],
        },
    )];
    let report = ran(&host, program);
    let Some((path, failure)) = failure(&report) else {
        panic!("a repeat past the limit must fail: {:?}", report.outcome);
    };
    assert_eq!(path.as_str(), "0");
    assert!(
        matches!(failure, StepFailure::LoopBound { limit: MAX_REPEAT }),
        "{failure:?}"
    );
}

#[test]
fn a_return_finishes_the_run_early() {
    let host = FakeHost::new();
    let program = vec![
        list(),
        Step::new(
            "finish",
            StepKind::Return {
                value: Some(Binding::step("list", "/count")),
            },
        ),
        // A step after the return is never taken.
        invoke(
            "create-one",
            NOTES,
            Selector::Fixed(INBOX.into()),
            "create",
            &[("title", Binding::Literal(json!("after")))],
        ),
    ];
    let report = ran(&host, program);
    assert!(completed(&report), "{:?}", report.outcome);
    assert_eq!(report.output, Some(json!(2)));
    assert_eq!(paths(&report), ["0", "1"]);
    assert!(
        host.started()
            .iter()
            .all(|request| request.target.address.action != "create"),
        "a step after the return must not run"
    );
}

// ---------------------------------------------------------------------------
// The operations themselves, each answering what it is defined for and
// nothing else.
// ---------------------------------------------------------------------------

#[test]
fn the_operations_answer_what_they_are_defined_for() {
    // Arithmetic keeps numbers numbers and refuses anything else.
    assert_eq!(arithmetic(Op::Add, &json!(2), &json!(3)), Ok(json!(5)));
    assert_eq!(arithmetic(Op::Divide, &json!(7), &json!(2)), Ok(json!(3.5)));
    assert_eq!(arithmetic(Op::Modulo, &json!(7), &json!(4)), Ok(json!(3)));
    assert_eq!(
        arithmetic(Op::Divide, &json!(1), &json!(0)),
        Err(ExprError::DivideByZero {})
    );
    assert!(matches!(
        arithmetic(Op::Add, &json!(1), &json!("three")),
        Err(ExprError::Type { .. })
    ));
    // Concatenation joins text and nothing else.
    assert_eq!(
        arithmetic(Op::Concat, &json!("a"), &json!("b")),
        Ok(json!("ab"))
    );
    // A number joins into text as it reads, so a name built from a counter
    // needs no formatting step; a list or a dictionary has no such reading.
    assert_eq!(
        arithmetic(Op::Concat, &json!("pass "), &json!(1)),
        Ok(json!("pass 1"))
    );
    assert!(matches!(
        arithmetic(Op::Concat, &json!("a"), &json!([1])),
        Err(ExprError::Type { .. })
    ));
    assert_eq!(negate(&json!(4)), Ok(json!(-4)));

    // A comparison orders like values and refuses unlike ones.
    assert_eq!(compare(Op::Less, &json!(1), &json!(2)), Ok(json!(true)));
    assert_eq!(
        compare(Op::Greater, &json!("b"), &json!("a")),
        Ok(json!(true))
    );
    assert_eq!(compare(Op::Equals, &json!(1), &json!(1.0)), Ok(json!(true)));
    assert_eq!(
        compare(Op::Equals, &json!(1), &json!("1")),
        Ok(json!(false))
    );
    assert!(matches!(
        compare(Op::Less, &json!(1), &json!("2")),
        Err(ExprError::Unordered { .. })
    ));

    // Text, lists and dictionaries.
    assert_eq!(length(&json!("héllo")), Ok(json!(5)));
    assert_eq!(length(&json!([1, 2, 3])), Ok(json!(3)));
    assert_eq!(length(&json!({"a": 1})), Ok(json!(1)));
    assert!(length(&json!(5)).is_err());
    assert_eq!(
        replace(&json!("a-b-c"), &json!("-"), &json!("+")),
        Ok(json!("a+b+c"))
    );
    assert_eq!(
        split(&json!("a,b"), Some(&json!(","))),
        Ok(json!(["a", "b"]))
    );
    assert_eq!(split(&json!("ab"), None), Ok(json!(["a", "b"])));
    assert_eq!(take(&json!([1, 2, 3]), &json!(2)), Ok(json!([1, 2])));
    assert_eq!(sort(&json!([3, 1, 2])), Ok(json!([1, 2, 3])));
    assert_eq!(field(&json!({"a": {"b": [1, 2]}}), "a.b.1"), Ok(json!(2)));
    assert!(matches!(
        field(&json!({"a": 1}), "b"),
        Err(ExprError::MissingField { .. })
    ));
    assert_eq!(
        map_field(&json!([{"n": 1}, {"n": 2}]), "n"),
        Ok(json!([1, 2]))
    );
    assert!(map_field(&json!([{"n": 1}, {}]), "n").is_err());

    // Truthiness is what a condition asks about a value.
    assert!(!truthy(&Value::Null));
    assert!(!truthy(&json!(0)));
    assert!(!truthy(&json!("")));
    assert!(!truthy(&json!([])));
    assert!(truthy(&json!("x")));
    assert!(truthy(&json!([0])));
    assert_eq!(type_name(&json!("x")), "a text");
}

#[test]
fn an_expression_names_its_result_shape_for_a_later_step() {
    // A dictionary an expression built is reachable by name, and its members
    // have the shapes the expression gave them.
    let built = Expr::Object {
        members: vec![
            Member::new(
                "names",
                Expr::Map {
                    items: Box::new(Expr::Value {
                        binding: Binding::Literal(json!([])),
                    }),
                    field: "name".into(),
                },
            ),
            Member::new(
                "count",
                Expr::Length {
                    value: Box::new(Expr::Value {
                        binding: Binding::Literal(json!("ab")),
                    }),
                },
            ),
        ],
    };
    let schema = built.schema().expect("an object has a shape");
    assert_eq!(schema["type"], json!("object"));
    assert_eq!(schema["properties"]["count"]["type"], json!("integer"));
    assert_eq!(schema["properties"]["names"]["type"], json!("array"));
    assert_eq!(
        built.to_string(),
        "{names: name of each of [], count: length of “ab”}"
    );
}

#[test]
fn a_filters_element_does_not_escape_it() {
    let host = files_host();
    // A filter binds `file` while it judges each element; nothing after it
    // reads that name, so the checker refuses one that does.
    let program = vec![
        invoke("list", FILES, folder(), "list", &[]),
        Step::new(
            "large",
            StepKind::Compute {
                expr: Expr::Filter {
                    items: Box::new(files()),
                    test: Box::new(comparing(Op::Greater, size(), ten())),
                },
            },
        ),
        Step::new("after", StepKind::Compute { expr: size() }),
    ];
    let reasons = refused(&host, program);
    assert!(
        reasons.iter().any(|(path, kind)| path == "2"
            && matches!(kind, CheckErrorKind::UnknownVariable(name) if name == "file")),
        "{reasons:?}"
    );
}
