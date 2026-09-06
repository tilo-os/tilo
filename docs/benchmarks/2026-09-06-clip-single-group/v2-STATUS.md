# Validation v2 failed

Scoped format/diff check passed. Restoring the original shader registration cleared the v1 compiler failure. Strict production Clippy then failed E0382 at paint.rs:1880: the new optional mutable full-attempt counter is moved inside the retry loop. Root requested the existing neighboring counters' mutable reborrow pattern. Command is terminal; no runtime correctness test, release build or performance timing ran. Failed v1/v2 logs remain intact.
