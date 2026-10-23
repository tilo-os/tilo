use std::fmt::Write as _;

/// Markdown of at least `target` bytes: repeated sections of headings,
/// paragraphs with inline markup and links, nested and task lists, quotes,
/// fenced code, tables, styled spans and footnotes.
pub fn markdown(target: usize) -> Vec<u8> {
    let mut out = String::with_capacity(target.saturating_add(4096));
    let mut section = 0usize;
    while out.len() < target {
        section = section.saturating_add(1);
        let _ = write!(
            out,
            "## Section {section}: quarterly *planning* notes\n\n\
The **format host** converts documents between native formats and the \
canonical CRDT. This paragraph mixes *emphasis*, **strong text**, `inline code` and a \
[link to the docs](https://tilo.so/docs/formats) so the parser exercises inline rules. \
It keeps going for a while to resemble a real paragraph written by a person who \
likes long sentences, commas, and the occasional parenthetical (like this one).[^n{section}]\n\n\
- first bullet with **bold**\n- second bullet with `code`\n  - nested item\n- third bullet\n\n\
1. step one\n2. step two\n3. step three\n\n\
- [x] done task\n- [ ] open task with ~~struck~~ text\n\n\
> A quoted remark that spans\n> two lines of text.\n\n\
```rust\nfn main() {{\n    println!(\"hello {{}}\", 42);\n}}\n```\n\n\
| Name | Value |\n| ---- | ----- |\n| alpha | 1 |\n| beta | 2 |\n\n\
Styled <span style=\"color:#ff0000\">red words</span>, H<sub>2</sub>O and x<sup>2</sup>; \
umlauts äöü and emoji 🎉 keep offsets honest.\n\n\
[^n{section}]: Footnote body for section {section}.\n\n"
        );
    }
    out.into_bytes()
}
