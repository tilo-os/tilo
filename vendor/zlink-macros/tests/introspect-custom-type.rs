#![cfg(feature = "introspection")]

use zlink::{idl, introspect::CustomType};

#[test]
fn named_struct_custom_type() {
    match Person::CUSTOM_TYPE {
        idl::CustomType::Object(obj) => {
            // Verify the custom type includes the name
            assert_eq!(obj.name(), "Person");

            let field_vec: Vec<_> = obj.fields().collect();
            assert_eq!(field_vec.len(), 3);

            // Check name field
            assert_eq!(field_vec[0].name(), "name");
            assert_eq!(field_vec[0].ty(), &idl::Type::String);

            // Check age field
            assert_eq!(field_vec[1].name(), "age");
            assert_eq!(field_vec[1].ty(), &idl::Type::Int);

            // Check active field
            assert_eq!(field_vec[2].name(), "active");
            assert_eq!(field_vec[2].ty(), &idl::Type::Bool);
        }
        _ => panic!("Expected custom object type for Person"),
    }
}

#[test]
fn unit_struct_custom_type() {
    match Unit::CUSTOM_TYPE {
        idl::CustomType::Object(obj) => {
            // Verify the custom type includes the name
            assert_eq!(obj.name(), "Unit");

            let field_vec: Vec<_> = obj.fields().collect();
            assert_eq!(field_vec.len(), 0);
        }
        _ => panic!("Expected custom object type for Unit"),
    }
}

#[test]
fn complex_struct_custom_type() {
    match Complex::CUSTOM_TYPE {
        idl::CustomType::Object(obj) => {
            // Verify the custom type includes the name
            assert_eq!(obj.name(), "Complex");

            let field_vec: Vec<_> = obj.fields().collect();
            assert_eq!(field_vec.len(), 4);

            // Check id field
            assert_eq!(field_vec[0].name(), "id");
            assert_eq!(field_vec[0].ty(), &idl::Type::Int);

            // Check description field (Option<String>)
            assert_eq!(field_vec[1].name(), "description");
            match field_vec[1].ty() {
                idl::Type::Optional(inner) => assert_eq!(inner.inner(), &idl::Type::String),
                _ => panic!("Expected optional type for description"),
            }

            // Check tags field (Vec<String>)
            assert_eq!(field_vec[2].name(), "tags");
            match field_vec[2].ty() {
                idl::Type::Array(inner) => assert_eq!(inner.inner(), &idl::Type::String),
                _ => panic!("Expected array type for tags"),
            }

            // Check coordinates field (Option<Vec<f64>>)
            assert_eq!(field_vec[3].name(), "coordinates");
            match field_vec[3].ty() {
                idl::Type::Optional(optional_inner) => match optional_inner.inner() {
                    idl::Type::Array(array_inner) => {
                        assert_eq!(array_inner.inner(), &idl::Type::Float)
                    }
                    _ => panic!("Expected array inside optional for coordinates"),
                },
                _ => panic!("Expected optional type for coordinates"),
            }
        }
        _ => panic!("Expected custom object type for Complex"),
    }
}

#[test]
fn primitives_struct_custom_type() {
    match Primitives::CUSTOM_TYPE {
        idl::CustomType::Object(obj) => {
            // Verify the custom type includes the name
            assert_eq!(obj.name(), "Primitives");

            let field_vec: Vec<_> = obj.fields().collect();
            assert_eq!(field_vec.len(), 5);

            assert_eq!(field_vec[0].name(), "boolean");
            assert_eq!(field_vec[0].ty(), &idl::Type::Bool);

            assert_eq!(field_vec[1].name(), "signed");
            assert_eq!(field_vec[1].ty(), &idl::Type::Int);

            assert_eq!(field_vec[2].name(), "unsigned");
            assert_eq!(field_vec[2].ty(), &idl::Type::Int);

            assert_eq!(field_vec[3].name(), "floating");
            assert_eq!(field_vec[3].ty(), &idl::Type::Float);

            assert_eq!(field_vec[4].name(), "text");
            assert_eq!(field_vec[4].ty(), &idl::Type::String);
        }
        _ => panic!("Expected custom object type for Primitives"),
    }
}

#[test]
fn basic_enum_custom_type() {
    match Status::CUSTOM_TYPE {
        idl::CustomType::Enum(enm) => {
            // Verify the custom type includes the name
            assert_eq!(enm.name(), "Status");

            let variant_vec: Vec<_> = enm.variants().collect();
            assert_eq!(variant_vec.len(), 3);
            assert_eq!(variant_vec[0].name(), "Active");
            assert_eq!(variant_vec[1].name(), "Inactive");
            assert_eq!(variant_vec[2].name(), "Pending");
        }
        _ => panic!("Expected custom enum type for Status"),
    }
}

#[test]
fn multi_variant_enum_custom_type() {
    match Color::CUSTOM_TYPE {
        idl::CustomType::Enum(enm) => {
            // Verify the custom type includes the name
            assert_eq!(enm.name(), "Color");

            let variant_vec: Vec<_> = enm.variants().collect();
            assert_eq!(variant_vec.len(), 6);
            assert_eq!(variant_vec[0].name(), "Red");
            assert_eq!(variant_vec[1].name(), "Green");
            assert_eq!(variant_vec[2].name(), "Blue");
            assert_eq!(variant_vec[3].name(), "Yellow");
            assert_eq!(variant_vec[4].name(), "Orange");
            assert_eq!(variant_vec[5].name(), "Purple");
        }
        _ => panic!("Expected custom enum type for Color"),
    }
}

#[test]
fn single_variant_enum_custom_type() {
    match UnitEnum::CUSTOM_TYPE {
        idl::CustomType::Enum(enm) => {
            // Verify the custom type includes the name
            assert_eq!(enm.name(), "UnitEnum");

            let variant_vec: Vec<_> = enm.variants().collect();
            assert_eq!(variant_vec.len(), 1);
            assert_eq!(variant_vec[0].name(), "Only");
        }
        _ => panic!("Expected custom enum type for UnitEnum"),
    }
}

// Test that the macro generates const-compatible code
#[test]
fn const_compatibility() {
    const _: &idl::CustomType<'static> = Person::CUSTOM_TYPE;
    const _: &idl::CustomType<'static> = Unit::CUSTOM_TYPE;
    const _: &idl::CustomType<'static> = Complex::CUSTOM_TYPE;
    const _: &idl::CustomType<'static> = Status::CUSTOM_TYPE;
    const _: &idl::CustomType<'static> = Color::CUSTOM_TYPE;
    const _: &idl::CustomType<'static> = UnitEnum::CUSTOM_TYPE;
}

#[test]
fn nested_struct_custom_type() {
    // First verify Address works
    match <Address as CustomType>::CUSTOM_TYPE {
        idl::CustomType::Object(obj) => {
            assert_eq!(obj.name(), "Address");
            let field_vec: Vec<_> = obj.fields().collect();
            assert_eq!(field_vec.len(), 2);
        }
        _ => panic!("Expected custom object type for Address"),
    }

    // Then verify PersonWithAddress works
    match PersonWithAddress::CUSTOM_TYPE {
        idl::CustomType::Object(obj) => {
            assert_eq!(obj.name(), "PersonWithAddress");
            let field_vec: Vec<_> = obj.fields().collect();
            assert_eq!(field_vec.len(), 2);

            assert_eq!(field_vec[0].name(), "name");
            assert_eq!(field_vec[0].ty(), &idl::Type::String);

            assert_eq!(field_vec[1].name(), "address");
            match field_vec[1].ty() {
                idl::Type::Custom(name) if *name == "Address" => (),
                _ => panic!("Expected custom object type for address field"),
            }
        }
        _ => panic!("Expected custom object type for PersonWithAddress"),
    }
}

#[test]
fn custom_type_name_access() {
    // Test the name() method on custom types
    assert_eq!(Person::CUSTOM_TYPE.name(), "Person");
    assert_eq!(Unit::CUSTOM_TYPE.name(), "Unit");
    assert_eq!(Complex::CUSTOM_TYPE.name(), "Complex");
    assert_eq!(Status::CUSTOM_TYPE.name(), "Status");
    assert_eq!(Color::CUSTOM_TYPE.name(), "Color");
    assert_eq!(UnitEnum::CUSTOM_TYPE.name(), "UnitEnum");
}

#[test]
fn custom_type_variant_checking() {
    // Test struct types
    assert!(Person::CUSTOM_TYPE.is_object());
    assert!(!Person::CUSTOM_TYPE.is_enum());
    assert!(Person::CUSTOM_TYPE.as_object().is_some());
    assert!(Person::CUSTOM_TYPE.as_enum().is_none());

    // Test enum types
    assert!(!Status::CUSTOM_TYPE.is_object());
    assert!(Status::CUSTOM_TYPE.is_enum());
    assert!(Status::CUSTOM_TYPE.as_object().is_none());
    assert!(Status::CUSTOM_TYPE.as_enum().is_some());
}

// Test basic named struct
#[derive(CustomType)]
#[allow(unused)]
struct Person {
    name: String,
    age: i32,
    active: bool,
}

// Test unit struct
#[derive(CustomType)]
#[allow(unused)]
struct Unit;

// Test struct with optional and array types
#[derive(CustomType)]
#[allow(unused)]
struct Complex {
    id: u64,
    description: Option<String>,
    tags: Vec<String>,
    coordinates: Option<Vec<f64>>,
}

// Test struct with primitive types
#[derive(CustomType)]
#[allow(unused)]
struct Primitives {
    boolean: bool,
    signed: i64,
    unsigned: u32,
    floating: f64,
    text: String,
}

// Test nested struct (this will require the other struct to also have TypeInfo)
#[derive(CustomType)]
#[allow(unused)]
struct PersonWithAddress {
    name: String,
    address: Address,
}

#[derive(CustomType)]
#[allow(unused)]
struct Address {
    street: String,
    city: String,
}

// Test basic unit enum
#[derive(CustomType)]
#[allow(unused)]
enum Status {
    Active,
    Inactive,
    Pending,
}

// Test enum with more variants
#[derive(CustomType)]
#[allow(unused)]
enum Color {
    Red,
    Green,
    Blue,
    Yellow,
    Orange,
    Purple,
}

// Test single variant enum
#[derive(CustomType)]
#[allow(unused)]
enum UnitEnum {
    Only,
}
