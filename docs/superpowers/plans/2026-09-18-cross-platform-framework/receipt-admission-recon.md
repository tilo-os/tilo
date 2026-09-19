# Receipt admission preparation

Source inspection for durable-publication Task8; not an implemented contract or native qualification.

`OperationOwner::open` currently tests directory kind, then creates `.tilo-operation-writer`, takes advisory flock and reads receipts. Framework `StorageDirectory::from_file` likewise validates kind and commit capability, not private namespace ownership. Consequently Task8 needs a shared held-directory privacy admission operation before any sentinel or receipt access; passing a directory through the current constructor is insufficient. Keep the admission mechanism in atomic-file, not runtime-fs or apps.

The existing Unix file privacy validator combines owner, mode, single-link and macOS ACL admission. Its single-link condition cannot be copied to directories, whose ordinary link counts differ. Factor common principal/mode/ACL policy deliberately and retain file-specific hardlink checks. macOS admission must inspect ACLs on the retained directory handle and include benign-owner/deny acceptance and additional-principal rejection tests. Linux effective permission policy must be assessed separately; do not label a mode-only check universal across arbitrary filesystems. Do not chmod an incoming authority to force admission or reopen its display path.

Task8 remains Unix grant-provider scoped. A new shared constructor or validation method must have a clear host contract and cannot advertise a successful unimplemented Windows privacy route. The ongoing Task4 probe may also need directory ACL inspection; reconcile helper ownership against its accepted diff before implementing Task8 to avoid competing native-policy copies.

The journal writer handle is currently cap-std File plus rustix flock; preserving that cooperative exclusion is required. If FileLease migration is included later, never duplicate a lock-bearing lease or silently change its sentinel namespace. The retained journal directory may be duplicated to construct shared publication authority.

`update` clones a receipt and updates memory only after write success; it poisons the store on write failure. `admit` writes before insertion but currently propagates errors without poisoning. New fault coverage must inspect the initial admission path as well as post-effect checkpoints: uncertain admitted receipts can exist on disk without an in-memory entry, so choose and document the conservative policy rather than assuming all failures already poison. No user-file operation may start before admitted metadata succeeds.

FsError currently retains sources for Io and Transport. Preserve typed publication phase through a new variant and review exhaustive classifications against existing public operation codes. Postcard receipt v1 bytes, owner strings, IDs and stored device/inode representations remain unchanged; do not reinterpret those fields as portable Windows identities.

Required preparation before dispatch: finalized admission API, exact owned files including shared primitive changes, tests for no-effects rejection and renamed parent authority, real publication faults across admission and after-effects checkpoints, and review of production error mapping. Existing native_operations fixtures use private tempfile roots; test fixture access must remain intentional.
