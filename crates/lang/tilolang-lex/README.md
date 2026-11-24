# tilolang-lex

The lexer for [tilolang](https://tilolang.dev), the Tilo app language. It reads a program's
source text as a list of tokens, each with the byte span it covers, the literal it holds and the
diagnostics for text that is not the language.

The `tilolang` compiler is the main user. Editor and tooling
code depends on this crate directly when it has to read source the way the compiler does, so
syntax highlighting, bracket matching and formatting never disagree with what the language
accepts. Apps written in tilolang do not depend on it.

## Use it

```toml
[dependencies]
tilolang-lex = "0.4"
```

```rust
use tilolang_lex::lexer::{Tok, lex};

let (tokens, diagnostics) = lex("let total = 2 + 3\n");
assert!(diagnostics.is_empty());
assert!(matches!(tokens.first().map(|t| &t.tok), Some(Tok::Kw("let"))));
assert!(
    matches!(tokens.get(1).map(|t| &t.tok), Some(Tok::Ident(name)) if name.as_str() == "total")
);
```

A `Token` is a `Tok` and a `Span` into the source, so a compiler error, an editor's colouring
and a rename point at the same bytes. `lex_at` lexes a fragment whose first byte sits at a known
offset in the file, which is how the inside of a `{ ... }` string interpolation is read.

The crate also covers the parts of source an editor needs without a parse:

- `plain_comments` returns the `//` comments in source order, with their spans.
- `calendar::check_date` and `calendar::check_time` validate the text of day and instant
  literals.
- `Tok::describe` names a token the way a diagnostic does.

## Learn more

- Language documentation: https://tilolang.dev/docs/
- Repository: https://code.tilo.so/tilo/tilo

## License

MIT
