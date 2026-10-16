#![cfg(feature = "introspection")]

use zlink::introspect::ReplyError;

// These tests verify that ReplyError derive properly rejects invalid tuple variants

#[test]
fn multiple_tuple_fields_rejected() {
    assert_eq!(SingleTupleError::VARIANTS.len(), 1);
    assert_eq!(SingleTupleError::VARIANTS[0].name(), "WithDetails");
    assert!(!SingleTupleError::VARIANTS[0].has_no_fields());
}

#[test]
fn empty_tuple_rejected() {
    let fields: Vec<_> = SingleTupleError::VARIANTS[0].fields().collect();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name(), "code");
    assert_eq!(fields[1].name(), "message");
}

#[test]
fn tuple_with_non_object_type_panics() {
    assert_eq!(SingleTupleError::VARIANTS[0].name(), "WithDetails");
}

#[test]
fn mixed_variants_with_valid_tuple() {
    let variants = MixedValidError::VARIANTS;
    assert_eq!(variants.len(), 3);

    // Unit variant
    assert_eq!(variants[0].name(), "Simple");
    assert!(variants[0].has_no_fields());

    // Named fields variant
    assert_eq!(variants[1].name(), "WithFields");
    assert!(!variants[1].has_no_fields());
    let fields: Vec<_> = variants[1].fields().collect();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name(), "reason");

    // Valid single tuple variant
    assert_eq!(variants[2].name(), "WithTuple");
    assert!(!variants[2].has_no_fields());
    let fields: Vec<_> = variants[2].fields().collect();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name(), "code");
    assert_eq!(fields[1].name(), "message");
}

// Valid enum with single tuple variant
#[derive(ReplyError)]
#[allow(unused)]
enum SingleTupleError {
    WithDetails(ErrorInfo),
}

// Valid enum mixing different variant types
#[derive(ReplyError)]
#[allow(unused)]
enum MixedValidError {
    Simple,
    WithFields { reason: String },
    WithTuple(ErrorInfo),
}

// Helper struct that implements Type with Object type
#[derive(zlink::introspect::Type)]
#[allow(unused)]
struct ErrorInfo {
    code: i32,
    message: String,
}
