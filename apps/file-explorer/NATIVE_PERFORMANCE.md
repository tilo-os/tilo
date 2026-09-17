# Native performance evidence adapter

Build the helper `cargo build -p file-explorer --example seed-file-explorer-fixture`.
Run `target/debug/examples/seed-file-explorer-fixture /absolute/path/to/new-directory`.
The directory's parent must exist. The helper refuses existing directories,
files and symlinks, emits exactly one JSON manifest on stdout, and never clears
an existing directory. A failed seed may leave a partial new directory.

The fixture contains 1,200 real directory entries: four folders, 299 distinct
64×48 PNG images, and 897 Markdown, Rust and text files. Images enter the normal
thumbnail generator/cache during drawing. No prewarming or synthetic preview
metadata is applied. Names, content and default-order digest are deterministic;
filesystem timestamps and inode numbers are host-specific.

Launch the normal binary with the fixture directory as an **absolute positional
argument**: `file-explorer /absolute/path/to/new-directory`. `--path` belongs to
the screenshot route and does not select the live directory. Use an isolated
normal app configuration/cache environment for repeatable captures; the adapter
does not change persisted view preferences or the production launch path.

The fixture manifest contains `schema: 1`, `hash_algorithm: "sha256"`,
`entry_count: 1200`, `image_count: 299`, `directory_count: 4`, and `order_hash`.
The digest covers production default directories-first, ascending-name order.
Each entry contributes its UTF-8 name byte length as a little-endian u64, its
name bytes, and one byte (1 for a directory, 0 for a file). It deliberately
does not claim file-content integrity.

The opt-in framework native observer receives a read-only root snapshot:

- `app: "dev.qos.file-explorer"`, `view_id: "file_view"`, `directory`;
- `entry_count`, `order_hash`, `hash_algorithm: "sha256"` from the loaded model;
- `ready`, `is_loading`, `has_error`, `thumbnail_pending` (null without service);
- `selected_indices`, corresponding `selected_names`, and nullable `primary_index`;
- `offset_x`, `offset_y`, `offset_units: "pixels"` from the persisted scroll cell;
- `last_cursor: [x, y]`, the root's last observed logical pointer position;
- `root_rect`, nullable `viewport_rect` and `content_rect`, in logical coordinates;
- `view_mode` (`List`/`Grid`), `sort_by`, and `sort_order`.

`ready` requires a successful directory load and valid root/content/viewport
geometry. It does not assert thumbnails have finished or that a frame reached
the display. The viewport is the nearest Scrollable ancestor of `file_view`,
not the independently scrolling sidebar. Native scenario pointer coordinates
are physical: divide by the capture's actual window scale factor before testing
them against these logical rectangles.

`last_cursor` observes the root's stored pointer; it does not infer a position
from a wheel event, which carries no coordinates. Native cursor movement is
converted using the event's scale factor. The root seeds replacement content
scroll containers with this value. The Scrollable's own stored cursor has no
public read-only accessor, so this field cannot establish that every descendant
received the same pointer event. A diagnostic must not infer external pointer
interference or a routing defect solely from an unmoving wheel.

A wheel validator should require a correctly directed offset change on every
step, stable directory/order/selection/view settings, and pointer containment.
Reverse before clamping; do not discard unmoving samples. A selection validator
should check the exact expected displayed index and name after the normal click
or keyboard input. Thumbnail pending counts may change independently. Frame
snapshots must match the last applied input state, and display-feedback evidence
is still required separately. These hooks and headless tests do not qualify
native latency, frame times, thumbnail completion, or pixel correctness.
