#![cfg(feature = "introspection")]

use zlink::introspect::ReplyError;

#[test]
fn simple_error_enum() {
    let variants = ServiceError::VARIANTS;
    assert_eq!(variants.len(), 3);

    assert_eq!(variants[0].name(), "NotFound");
    assert!(variants[0].has_no_fields());

    assert_eq!(variants[1].name(), "PermissionDenied");
    assert!(variants[1].has_no_fields());

    assert_eq!(variants[2].name(), "InternalError");
    assert!(variants[2].has_no_fields());
}

#[test]
fn single_variant_error() {
    let variants = SingleError::VARIANTS;
    assert_eq!(variants.len(), 1);
    assert_eq!(variants[0].name(), "OnlyError");
    assert!(variants[0].has_no_fields());
}

#[test]
fn multi_variant_error() {
    let variants = NetworkError::VARIANTS;
    assert_eq!(variants.len(), 5);
    assert_eq!(variants[0].name(), "Timeout");
    assert_eq!(variants[1].name(), "ConnectionRefused");
    assert_eq!(variants[2].name(), "HostUnreachable");
    assert_eq!(variants[3].name(), "InvalidResponse");
    assert_eq!(variants[4].name(), "Unauthorized");

    // Verify all have no fields
    for variant in variants {
        assert!(variant.has_no_fields());
    }
}

// Test that the macro generates const-compatible code
#[test]
fn const_compatibility() {
    const _: &[&zlink::idl::Error<'static>] = ServiceError::VARIANTS;
    const _: &[&zlink::idl::Error<'static>] = SingleError::VARIANTS;
    const _: &[&zlink::idl::Error<'static>] = NetworkError::VARIANTS;
    const _: &[&zlink::idl::Error<'static>] = DatabaseError::VARIANTS;
    const _: &[&zlink::idl::Error<'static>] = ValidationError::VARIANTS;
    const _: &[&zlink::idl::Error<'static>] = TupleError::VARIANTS;
    const _: &[&zlink::idl::Error<'static>] = MixedTupleError::VARIANTS;
}

#[test]
fn error_names_match_variants() {
    // Test that the generated error names exactly match the enum variant names
    assert_eq!(ServiceError::VARIANTS[0].name(), "NotFound");
    assert_eq!(ServiceError::VARIANTS[1].name(), "PermissionDenied");
    assert_eq!(ServiceError::VARIANTS[2].name(), "InternalError");

    assert_eq!(NetworkError::VARIANTS[0].name(), "Timeout");
    assert_eq!(NetworkError::VARIANTS[1].name(), "ConnectionRefused");
    assert_eq!(NetworkError::VARIANTS[2].name(), "HostUnreachable");
    assert_eq!(NetworkError::VARIANTS[3].name(), "InvalidResponse");
    assert_eq!(NetworkError::VARIANTS[4].name(), "Unauthorized");
}

#[test]
fn mixed_variants_error() {
    let variants = DatabaseError::VARIANTS;
    assert_eq!(variants.len(), 4);

    // Unit variant
    assert_eq!(variants[0].name(), "ConnectionFailed");
    assert!(variants[0].has_no_fields());

    // Variant with named fields
    assert_eq!(variants[1].name(), "InvalidQuery");
    assert!(!variants[1].has_no_fields());
    let fields: Vec<_> = variants[1].fields().collect();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name(), "message");
    assert_eq!(fields[1].name(), "line");

    // Another variant with named fields
    assert_eq!(variants[2].name(), "Timeout");
    assert!(!variants[2].has_no_fields());
    let fields: Vec<_> = variants[2].fields().collect();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name(), "seconds");

    // Variant with multiple named fields
    assert_eq!(variants[3].name(), "AccessDenied");
    assert!(!variants[3].has_no_fields());
    let fields: Vec<_> = variants[3].fields().collect();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0].name(), "user");
    assert_eq!(fields[1].name(), "resource");
    assert_eq!(fields[2].name(), "action");
}

#[test]
fn named_fields_only_error() {
    let variants = ValidationError::VARIANTS;
    assert_eq!(variants.len(), 2);

    // All variants should have fields
    for variant in variants {
        assert!(!variant.has_no_fields());
    }

    assert_eq!(variants[0].name(), "FieldMissing");
    let fields: Vec<_> = variants[0].fields().collect();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name(), "field_name");

    assert_eq!(variants[1].name(), "InvalidFormat");
    let fields: Vec<_> = variants[1].fields().collect();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0].name(), "field_name");
    assert_eq!(fields[1].name(), "expected");
    assert_eq!(fields[2].name(), "actual");
}

#[test]
fn tuple_variant_error() {
    let variants = TupleError::VARIANTS;
    assert_eq!(variants.len(), 2);

    // Unit variant
    assert_eq!(variants[0].name(), "Simple");
    assert!(variants[0].has_no_fields());

    // Tuple variant with struct fields
    assert_eq!(variants[1].name(), "Complex");
    assert!(!variants[1].has_no_fields());
    let fields: Vec<_> = variants[1].fields().collect();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name(), "code");
    assert_eq!(fields[1].name(), "description");
}

#[test]
fn mixed_tuple_and_named_error() {
    let variants = MixedTupleError::VARIANTS;
    assert_eq!(variants.len(), 3);

    // Unit variant
    assert_eq!(variants[0].name(), "NotFound");
    assert!(variants[0].has_no_fields());

    // Named field variant
    assert_eq!(variants[1].name(), "InvalidInput");
    assert!(!variants[1].has_no_fields());
    let fields: Vec<_> = variants[1].fields().collect();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name(), "message");

    // Tuple variant
    assert_eq!(variants[2].name(), "DatabaseError");
    assert!(!variants[2].has_no_fields());
    let fields: Vec<_> = variants[2].fields().collect();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name(), "code");
    assert_eq!(fields[1].name(), "description");
}

// Test basic service error enum
#[derive(ReplyError)]
#[allow(unused)]
enum ServiceError {
    NotFound,
    PermissionDenied,
    InternalError,
}

// Test single variant enum
#[derive(ReplyError)]
#[allow(unused)]
enum SingleError {
    OnlyError,
}

// Test enum with many variants
#[derive(ReplyError)]
#[allow(unused)]
enum NetworkError {
    Timeout,
    ConnectionRefused,
    HostUnreachable,
    InvalidResponse,
    Unauthorized,
}

// Test enum with named field variants
#[derive(ReplyError)]
#[allow(unused)]
enum DatabaseError {
    ConnectionFailed,
    InvalidQuery {
        message: String,
        line: u32,
    },
    Timeout {
        seconds: u64,
    },
    AccessDenied {
        user: String,
        resource: String,
        action: String,
    },
}

// Test single tuple variant error
#[derive(ReplyError)]
#[allow(unused)]
enum TupleError {
    Simple,
    Complex(ErrorDetails),
}

// Test mixed tuple and named variants
#[derive(ReplyError)]
#[allow(unused)]
enum MixedTupleError {
    NotFound,
    InvalidInput { message: String },
    DatabaseError(ErrorDetails),
}

// Helper struct for tuple variants
#[derive(zlink::introspect::Type)]
#[allow(unused)]
struct ErrorDetails {
    code: i32,
    description: String,
}

// Test enum with only named field variants
#[derive(ReplyError)]
#[allow(unused)]
enum ValidationError {
    FieldMissing {
        field_name: String,
    },
    InvalidFormat {
        field_name: String,
        expected: String,
        actual: String,
    },
}
