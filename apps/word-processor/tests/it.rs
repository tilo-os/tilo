// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "e2e_docx.rs"]
mod e2e_docx;
#[path = "e2e_html.rs"]
mod e2e_html;
#[path = "e2e_odt.rs"]
mod e2e_odt;
