use ui::i18n::{ArgValue, EmbeddedCatalog};
static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", include_str!("../locales/en-US.ftl"))],
    "calendar-tasks-title",
);
pub(crate) fn t(key: &str) -> String {
    CATALOG.t(key)
}
pub(crate) fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}
