# Tilo patch: the metadata database's page cache is its owner's to bound

The file store keeps its metadata, and the bytes of every blob up to 16 KiB (inlined), in a
redb database that `meta::Actor::new` opened with `redb::Database::create`, and so with
redb's default page cache of 1 GiB. redb keeps the pages it reads and writes in that cache
until it is full, and the store offers no way to set it.

A Tilo computer's synced folder writes each small file into this store. An agent editing
files in the folder for twelve minutes grew the process holding it by 112 MB of
`PagedCachedFile` pages (`write` and `read_direct_into_arc`), with no end before the
gigabyte: on a computer whose folder is used all day the process grows until it is killed.

`fs::options::Options` gains `meta_cache_bytes` (`src/store/fs/options.rs`), which
`Options::new` sets to redb's own default (`DEFAULT_META_CACHE_BYTES`), and the actor opens
the database with `redb::Builder::set_cache_size` of it (`src/store/fs/meta.rs`,
`src/store/fs.rs`). A store that does not set it behaves as before. Tilo's content store
sets it to `tilo_fs::store::META_CACHE_BYTES`. Each change is marked `TILO PATCH`.

Only the library is vendored: the manifest drops the example and test targets and the
dev-dependencies they use.

Upstream-ready description: "fs store: let the caller bound the metadata database's page
cache. `Options::meta_cache_bytes`, default unchanged (redb's 1 GiB)."

Remove this copy once a release lets the caller set the cache.
