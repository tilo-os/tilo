//! The runtime provides every import the compiler can emit, with the same signature.

use tilolang::types::Vt;
use tilolang_runtime::abi::IMPORTS;

fn letters(types: &[Vt]) -> String {
    types
        .iter()
        .map(|t| match t {
            Vt::I32 => 'i',
            Vt::I64 => 'l',
            Vt::F64 => 'f',
        })
        .collect()
}

#[test]
fn every_compiler_import_has_a_host_with_the_same_signature() {
    for imp in tilolang::abi::Imp::ALL {
        let row = IMPORTS
            .iter()
            .find(|(_, name, _, _)| *name == imp.name())
            .unwrap_or_else(|| panic!("the runtime has no import `{}`", imp.name()));
        assert_eq!(row.2, letters(imp.params()), "params of {}", imp.name());
        assert_eq!(row.3, letters(imp.results()), "results of {}", imp.name());
    }
}
