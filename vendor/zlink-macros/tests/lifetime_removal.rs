#![cfg(feature = "introspection")]
#![allow(unused)]
//! Tests for lifetime removal functionality in derive macros.

use zlink::{idl, introspect::Type};

#[derive(Type)]
struct WithLifetimes<'a> {
    name: &'a str,
    description: Option<&'a str>,
    tags: Vec<&'a str>,
}

#[derive(Type)]
struct ComplexLifetimes<'a, 'b> {
    primary: &'a str,
    secondary: &'b str,
    optional_primary: Option<&'a str>,
    list_of_refs: Vec<&'a str>,
    nested: WithLifetimes<'a>,
}

#[test]
fn lifetime_removal_basic() {
    // This test verifies that the Type derive macro can handle lifetimes
    // and that the generated const TYPE is accessible
    match WithLifetimes::TYPE {
        idl::Type::Object(fields) => {
            let field_vec: Vec<_> = fields.iter().collect();
            assert_eq!(field_vec.len(), 3);

            // Check name field
            assert_eq!(field_vec[0].name(), "name");
            assert_eq!(field_vec[0].ty(), &idl::Type::String);

            // Check description field (Optional<String>)
            assert_eq!(field_vec[1].name(), "description");
            match field_vec[1].ty() {
                idl::Type::Optional(inner) => {
                    assert_eq!(inner.inner(), &idl::Type::String);
                }
                _ => panic!("Expected optional type"),
            }

            // Check tags field (Array<String>)
            assert_eq!(field_vec[2].name(), "tags");
            match field_vec[2].ty() {
                idl::Type::Array(inner) => {
                    assert_eq!(inner.inner(), &idl::Type::String);
                }
                _ => panic!("Expected array type"),
            }
        }
        _ => panic!("Expected struct type"),
    }
}

#[test]
fn lifetime_removal_multiple_lifetimes() {
    // Test that multiple lifetime parameters are handled correctly
    match ComplexLifetimes::TYPE {
        idl::Type::Object(fields) => {
            let field_vec: Vec<_> = fields.iter().collect();
            assert_eq!(field_vec.len(), 5);

            // All string reference fields should become Type::String
            assert_eq!(field_vec[0].name(), "primary");
            assert_eq!(field_vec[0].ty(), &idl::Type::String);

            assert_eq!(field_vec[1].name(), "secondary");
            assert_eq!(field_vec[1].ty(), &idl::Type::String);

            // Optional string reference
            assert_eq!(field_vec[2].name(), "optional_primary");
            match field_vec[2].ty() {
                idl::Type::Optional(inner) => {
                    assert_eq!(inner.inner(), &idl::Type::String);
                }
                _ => panic!("Expected optional type"),
            }

            // Array of string references
            assert_eq!(field_vec[3].name(), "list_of_refs");
            match field_vec[3].ty() {
                idl::Type::Array(inner) => {
                    assert_eq!(inner.inner(), &idl::Type::String);
                }
                _ => panic!("Expected array type"),
            }

            // Check nested field (should be Object type since WithLifetimes<'a> becomes
            // WithLifetimes)
            assert_eq!(field_vec[4].name(), "nested");
            match field_vec[4].ty() {
                idl::Type::Object(nested_fields) => {
                    let nested_field_vec: Vec<_> = nested_fields.iter().collect();
                    assert_eq!(nested_field_vec.len(), 3);
                    assert_eq!(nested_field_vec[0].name(), "name");
                    assert_eq!(nested_field_vec[0].ty(), &idl::Type::String);
                }
                _ => panic!("Expected nested object type"),
            }
        }
        _ => panic!("Expected struct type"),
    }
}

#[test]
fn const_compatibility_with_lifetimes() {
    // This test ensures that the TYPE constant can be used in const contexts
    // even when the original type has lifetime parameters
    const _: &idl::Type = WithLifetimes::TYPE;
    const _: &idl::Type = ComplexLifetimes::TYPE;
}

#[derive(Type)]
struct NestedLifetimes<'a> {
    nested_option: Option<Option<&'a str>>,
    nested_vec: Vec<Option<&'a str>>,
}

#[test]
fn deeply_nested_lifetime_removal() {
    // Test deeply nested types with lifetimes
    match NestedLifetimes::TYPE {
        idl::Type::Object(fields) => {
            let field_vec: Vec<_> = fields.iter().collect();
            assert_eq!(field_vec.len(), 2);

            // nested_option: Option<Option<&str>> -> Optional<Optional<String>>
            assert_eq!(field_vec[0].name(), "nested_option");
            match field_vec[0].ty() {
                idl::Type::Optional(outer) => match outer.inner() {
                    idl::Type::Optional(inner) => {
                        assert_eq!(inner.inner(), &idl::Type::String);
                    }
                    _ => panic!("Expected nested optional"),
                },
                _ => panic!("Expected optional type"),
            }

            // nested_vec: Vec<Option<&str>> -> Array<Optional<String>>
            assert_eq!(field_vec[1].name(), "nested_vec");
            match field_vec[1].ty() {
                idl::Type::Array(array) => match array.inner() {
                    idl::Type::Optional(optional) => {
                        assert_eq!(optional.inner(), &idl::Type::String);
                    }
                    _ => panic!("Expected optional inside array"),
                },
                _ => panic!("Expected array type"),
            }
        }
        _ => panic!("Expected struct type"),
    }
}

#[test]
fn generic_lifetime_argument_removal() {
    // Test that lifetime arguments in generic types are properly removed
    // WithLifetimes<'a> should become WithLifetimes (no lifetime args)
    #[derive(Type)]
    struct TestGenericLifetimes<'a> {
        generic_field: WithLifetimes<'a>,
        complex_generic: Option<WithLifetimes<'a>>,
    }

    match TestGenericLifetimes::TYPE {
        idl::Type::Object(fields) => {
            let field_vec: Vec<_> = fields.iter().collect();
            assert_eq!(field_vec.len(), 2);

            // Both fields should reference the same WithLifetimes type without lifetime args
            assert_eq!(field_vec[0].name(), "generic_field");
            match field_vec[0].ty() {
                idl::Type::Object(nested_fields) => {
                    let nested_field_vec: Vec<_> = nested_fields.iter().collect();
                    assert_eq!(nested_field_vec.len(), 3);
                    assert_eq!(nested_field_vec[0].name(), "name");
                    assert_eq!(nested_field_vec[0].ty(), &idl::Type::String);
                }
                _ => panic!("Expected object type for generic_field"),
            }

            assert_eq!(field_vec[1].name(), "complex_generic");
            match field_vec[1].ty() {
                idl::Type::Optional(inner) => match inner.inner() {
                    idl::Type::Object(nested_fields) => {
                        let nested_field_vec: Vec<_> = nested_fields.iter().collect();
                        assert_eq!(nested_field_vec.len(), 3);
                        assert_eq!(nested_field_vec[0].name(), "name");
                        assert_eq!(nested_field_vec[0].ty(), &idl::Type::String);
                    }
                    _ => panic!("Expected object type inside optional"),
                },
                _ => panic!("Expected optional type for complex_generic"),
            }
        }
        _ => panic!("Expected struct type"),
    }
}
