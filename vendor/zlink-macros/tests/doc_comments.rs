#![cfg(feature = "introspection")]
#![allow(unused)]

use zlink::{
    idl::{CustomType, Type},
    introspect::{CustomType as CustomTypeTrait, ReplyError, Type as TypeTrait},
};

#[derive(zlink::introspect::Type)]
struct SimpleStruct {
    #[doc = "A simple field"]
    field: String,
}

#[derive(zlink::introspect::Type)]
enum SimpleEnum {
    #[doc = "First variant"]
    First,
    #[doc = "Second variant"]
    Second,
}

#[doc = "A documented custom struct"]
#[derive(zlink::introspect::CustomType)]
struct DocumentedStruct {
    #[doc = "The identifier"]
    id: u64,
    #[doc = "The name field"]
    name: String,
}

#[doc = "A documented custom enum"]
#[derive(zlink::introspect::CustomType)]
enum DocumentedEnum {
    #[doc = "Active state"]
    Active,
    #[doc = "Inactive state"]
    Inactive,
}

#[derive(zlink::introspect::ReplyError)]
enum DocumentedError {
    #[doc = "Not found error"]
    NotFound,
    #[doc = "Validation failed"]
    ValidationFailed {
        #[doc = "Error message"]
        message: String,
        #[doc = "Line number"]
        line: u32,
    },
}

#[test]
fn type_derive_includes_field_comments() {
    match SimpleStruct::TYPE {
        Type::Object(fields) => {
            let field_vec: Vec<_> = fields.iter().collect();
            assert_eq!(field_vec.len(), 1);

            let field = field_vec[0];
            assert_eq!(field.name(), "field");

            let comments: Vec<_> = field.comments().collect();
            assert_eq!(comments.len(), 1);
            assert_eq!(comments[0].text(), "A simple field");
        }
        _ => panic!("Expected object type"),
    }
}

#[test]
fn type_derive_includes_enum_variant_comments() {
    match SimpleEnum::TYPE {
        Type::Enum(variants) => {
            let variant_vec: Vec<_> = variants.iter().collect();
            assert_eq!(variant_vec.len(), 2);

            let first_variant = &variant_vec[0];
            assert_eq!(first_variant.name(), "First");
            let first_comments: Vec<_> = first_variant.comments().collect();
            assert_eq!(first_comments.len(), 1);
            assert_eq!(first_comments[0].text(), "First variant");

            let second_variant = &variant_vec[1];
            assert_eq!(second_variant.name(), "Second");
            let second_comments: Vec<_> = second_variant.comments().collect();
            assert_eq!(second_comments.len(), 1);
            assert_eq!(second_comments[0].text(), "Second variant");
        }
        _ => panic!("Expected enum type"),
    }
}

#[test]
fn custom_type_derive_includes_type_comments() {
    match DocumentedStruct::CUSTOM_TYPE {
        CustomType::Object(obj) => {
            assert_eq!(obj.name(), "DocumentedStruct");

            let comments: Vec<_> = obj.comments().collect();
            assert_eq!(comments.len(), 1);
            assert_eq!(comments[0].text(), "A documented custom struct");

            let fields: Vec<_> = obj.fields().collect();
            assert_eq!(fields.len(), 2);

            let id_field = &fields[0];
            assert_eq!(id_field.name(), "id");
            let id_comments: Vec<_> = id_field.comments().collect();
            assert_eq!(id_comments.len(), 1);
            assert_eq!(id_comments[0].text(), "The identifier");

            let name_field = &fields[1];
            assert_eq!(name_field.name(), "name");
            let name_comments: Vec<_> = name_field.comments().collect();
            assert_eq!(name_comments.len(), 1);
            assert_eq!(name_comments[0].text(), "The name field");
        }
        _ => panic!("Expected custom object type"),
    }
}

#[test]
fn custom_type_derive_includes_enum_comments() {
    match DocumentedEnum::CUSTOM_TYPE {
        CustomType::Enum(enm) => {
            assert_eq!(enm.name(), "DocumentedEnum");

            let comments: Vec<_> = enm.comments().collect();
            assert_eq!(comments.len(), 1);
            assert_eq!(comments[0].text(), "A documented custom enum");

            let variants: Vec<_> = enm.variants().collect();
            assert_eq!(variants.len(), 2);

            let active_variant = &variants[0];
            assert_eq!(active_variant.name(), "Active");
            let active_comments: Vec<_> = active_variant.comments().collect();
            assert_eq!(active_comments.len(), 1);
            assert_eq!(active_comments[0].text(), "Active state");

            let inactive_variant = &variants[1];
            assert_eq!(inactive_variant.name(), "Inactive");
            let inactive_comments: Vec<_> = inactive_variant.comments().collect();
            assert_eq!(inactive_comments.len(), 1);
            assert_eq!(inactive_comments[0].text(), "Inactive state");
        }
        _ => panic!("Expected custom enum type"),
    }
}

#[test]
fn reply_error_derive_includes_variant_comments() {
    let variants = DocumentedError::VARIANTS;
    assert_eq!(variants.len(), 2);

    let not_found = variants[0];
    assert_eq!(not_found.name(), "NotFound");
    let not_found_comments: Vec<_> = not_found.comments().collect();
    assert_eq!(not_found_comments.len(), 1);
    assert_eq!(not_found_comments[0].text(), "Not found error");

    let validation_failed = variants[1];
    assert_eq!(validation_failed.name(), "ValidationFailed");
    let validation_comments: Vec<_> = validation_failed.comments().collect();
    assert_eq!(validation_comments.len(), 1);
    assert_eq!(validation_comments[0].text(), "Validation failed");

    let fields: Vec<_> = validation_failed.fields().collect();
    assert_eq!(fields.len(), 2);

    let message_field = fields[0];
    assert_eq!(message_field.name(), "message");
    let message_comments: Vec<_> = message_field.comments().collect();
    assert_eq!(message_comments.len(), 1);
    assert_eq!(message_comments[0].text(), "Error message");

    let line_field = fields[1];
    assert_eq!(line_field.name(), "line");
    let line_comments: Vec<_> = line_field.comments().collect();
    assert_eq!(line_comments.len(), 1);
    assert_eq!(line_comments[0].text(), "Line number");
}

#[test]
fn multiple_doc_comments() {
    #[doc = "First comment"]
    #[doc = "Second comment"]
    #[derive(zlink::introspect::CustomType)]
    struct MultiComment {
        #[doc = "Field comment 1"]
        #[doc = "Field comment 2"]
        field: String,
    }

    match MultiComment::CUSTOM_TYPE {
        CustomType::Object(obj) => {
            let comments: Vec<_> = obj.comments().collect();
            assert_eq!(comments.len(), 2);
            assert_eq!(comments[0].text(), "First comment");
            assert_eq!(comments[1].text(), "Second comment");

            let fields: Vec<_> = obj.fields().collect();
            let field = &fields[0];
            let field_comments: Vec<_> = field.comments().collect();
            assert_eq!(field_comments.len(), 2);
            assert_eq!(field_comments[0].text(), "Field comment 1");
            assert_eq!(field_comments[1].text(), "Field comment 2");
        }
        _ => panic!("Expected custom object type"),
    }
}

#[test]
fn no_comments_generates_empty_arrays() {
    #[derive(zlink::introspect::CustomType)]
    struct NoComments {
        field: String,
    }

    match NoComments::CUSTOM_TYPE {
        CustomType::Object(obj) => {
            let comments: Vec<_> = obj.comments().collect();
            assert_eq!(comments.len(), 0);

            let fields: Vec<_> = obj.fields().collect();
            let field = &fields[0];
            let field_comments: Vec<_> = field.comments().collect();
            assert_eq!(field_comments.len(), 0);
        }
        _ => panic!("Expected custom object type"),
    }
}
