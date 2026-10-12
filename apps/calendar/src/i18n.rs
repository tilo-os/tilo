//! Embedded catalogue for Calendar's shared Contacts operations.
use ui::i18n::ArgValue;

pub fn t(key: &str) -> String {
    crate::resource::t(key)
}

pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    crate::resource::t_args(key, args)
}
