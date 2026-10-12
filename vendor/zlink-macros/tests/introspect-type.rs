#![cfg(feature = "introspection")]

use zlink::{idl, introspect::Type};

// Force diagnostics refresh
#[test]
fn named_struct_type() {
    match Person::TYPE {
        idl::Type::Object(fields) => {
            let field_vec: Vec<_> = fields.iter().collect();
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
        _ => panic!("Expected struct type for Person"),
    }
}

#[test]
fn unit_struct_type() {
    match Unit::TYPE {
        idl::Type::Object(fields) => {
            let field_vec: Vec<_> = fields.iter().collect();
            assert_eq!(field_vec.len(), 0);
        }
        _ => panic!("Expected struct type for Unit"),
    }
}

#[test]
fn complex_struct_type() {
    match Complex::TYPE {
        idl::Type::Object(fields) => {
            let field_vec: Vec<_> = fields.iter().collect();
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
        _ => panic!("Expected struct type for Complex"),
    }
}

#[test]
fn primitives_struct_type() {
    match Primitives::TYPE {
        idl::Type::Object(fields) => {
            let field_vec: Vec<_> = fields.iter().collect();
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
        _ => panic!("Expected struct type for Primitives"),
    }
}

#[test]
fn basic_enum_type() {
    match Status::TYPE {
        idl::Type::Enum(variants) => {
            let variant_vec: Vec<_> = variants.iter().collect();
            assert_eq!(variant_vec.len(), 3);
            assert_eq!(variant_vec[0].name(), "Active");
            assert_eq!(variant_vec[1].name(), "Inactive");
            assert_eq!(variant_vec[2].name(), "Pending");
        }
        _ => panic!("Expected enum type for Status"),
    }
}

#[test]
fn multi_variant_enum_type() {
    match Color::TYPE {
        idl::Type::Enum(variants) => {
            let variant_vec: Vec<_> = variants.iter().collect();
            assert_eq!(variant_vec.len(), 6);
            assert_eq!(variant_vec[0].name(), "Red");
            assert_eq!(variant_vec[1].name(), "Green");
            assert_eq!(variant_vec[2].name(), "Blue");
            assert_eq!(variant_vec[3].name(), "Yellow");
            assert_eq!(variant_vec[4].name(), "Orange");
            assert_eq!(variant_vec[5].name(), "Purple");
        }
        _ => panic!("Expected enum type for Color"),
    }
}

#[test]
fn single_variant_enum_type() {
    match UnitEnum::TYPE {
        idl::Type::Enum(variants) => {
            let variant_vec: Vec<_> = variants.iter().collect();
            assert_eq!(variant_vec.len(), 1);
            assert_eq!(variant_vec[0].name(), "Only");
        }
        _ => panic!("Expected enum type for UnitEnum"),
    }
}

// Test that the macro generates const-compatible code
#[test]
fn const_compatibility() {
    const _: &idl::Type<'static> = Person::TYPE;
    const _: &idl::Type<'static> = Unit::TYPE;
    const _: &idl::Type<'static> = Complex::TYPE;
    const _: &idl::Type<'static> = Status::TYPE;
    const _: &idl::Type<'static> = Color::TYPE;
    const _: &idl::Type<'static> = UnitEnum::TYPE;
}

#[test]
fn nested_struct_type() {
    // First verify Address works
    match Address::TYPE {
        idl::Type::Object(fields) => {
            let field_vec: Vec<_> = fields.iter().collect();
            assert_eq!(field_vec.len(), 2);
        }
        _ => panic!("Expected struct type for Address"),
    }

    // Then verify PersonWithAddress works
    match PersonWithAddress::TYPE {
        idl::Type::Object(fields) => {
            let field_vec: Vec<_> = fields.iter().collect();
            assert_eq!(field_vec.len(), 2);

            assert_eq!(field_vec[0].name(), "name");
            assert_eq!(field_vec[0].ty(), &idl::Type::String);

            assert_eq!(field_vec[1].name(), "address");
            // The address field should reference Address::TYPE
            match field_vec[1].ty() {
                idl::Type::Object(_) => {
                    // This should be the same as Address::TYPE
                    assert_eq!(field_vec[1].ty(), Address::TYPE);
                }
                _ => panic!("Expected struct type for address field"),
            }
        }
        _ => panic!("Expected struct type for PersonWithAddress"),
    }
}

// Test basic named struct
#[derive(Type)]
#[allow(unused)]
struct Person {
    name: String,
    age: i32,
    active: bool,
}

// Test unit struct
#[derive(Type)]
#[allow(unused)]
struct Unit;

// Test struct with optional and array types
#[derive(Type)]
#[allow(unused)]
struct Complex {
    id: u64,
    description: Option<String>,
    tags: Vec<String>,
    coordinates: Option<Vec<f64>>,
}

// Test struct with primitive types
#[derive(Type)]
#[allow(unused)]
struct Primitives {
    boolean: bool,
    signed: i64,
    unsigned: u32,
    floating: f64,
    text: String,
}

// Test nested struct (this will require the other struct to also have Type)
#[derive(Type)]
#[allow(unused)]
struct PersonWithAddress {
    name: String,
    address: Address,
}

#[derive(Type)]
#[allow(unused)]
struct Address {
    street: String,
    city: String,
}

// Test basic unit enum
#[derive(Type)]
#[allow(unused)]
enum Status {
    Active,
    Inactive,
    Pending,
}

// Test enum with more variants
#[derive(Type)]
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
#[derive(Type)]
#[allow(unused)]
enum UnitEnum {
    Only,
}
