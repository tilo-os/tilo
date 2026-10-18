# Tilo patch: an entity table the loader does not relocate

`src/entities.rs` kept the 2125 HTML5 entities as
`const ENTITIES: [(&[u8], &str); 2125]`. Each entry is two fat pointers, so the
table is 68 KB of `.data.rel.ro` that the dynamic loader copies and relocates
in every process mapping the library that holds it. In Tilo that library is
the shared app runtime (`libruntime_dylib.so`, through tilo-doc-codecs'
Markdown codec): every app paid it before `main`, whether it ever read a
`&amp;`.

The patched file holds the same entities in the same order: the names back to
back in one string literal, the expansions in another, and the table as
`(u16, u8, u16, u8)` offsets and lengths into them. `get_entity` binary-searches
the names as before. Nothing in the table is a pointer, so it is plain
`.rodata`. A test in the file reads every entity back and checks the order.

Upstream: the same layout would fit `mk_entities.py`'s output; offer it there
and drop this copy once a release carries it.
