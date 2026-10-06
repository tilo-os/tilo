//! Embedded catalogue for Calendar's shared Contacts operations.
use ui::i18n::{ArgValue, EmbeddedCatalog};

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", include_str!("../locales/en-US.ftl"))],
    "calendar-contact-attendees",
);

pub fn catalog() -> &'static EmbeddedCatalog {
    &CATALOG
}

pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}
