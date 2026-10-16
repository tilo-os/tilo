# Complete notification reading evidence

Verified source: f7ef336d29241ec38c47d499dfa48d817382c7ec. Before: merged main
18c4f1e472fffb39e7334c1160f6e6d5d27efba4. PNGs are saved to native ChatGPT
Library and representative images are attached to the PR; no capture PNGs are
committed here. artifacts.json retains their source revisions, dimensions,
checksums and both native Library identities.

The visible Read button uses ButtonFace. Its reading page wraps the complete
title/body with theme Heading/Body roles and ScrollRegion; the same region owns
the scrollbar, wheel, thumb interaction, keyboard paging and accessible scrolling.
Tab/Shift+Tab traverses history controls; Back/Esc returns to history selection,
then a second Esc leaves history. Reading performs no default action, dismissal
or agent handoff, and hidden previews remain hidden.

Fixtures contain only synthetic notices. The portable capture example runs the
production NotificationCenter painter at dark/light/high contrast, 200% text,
combined UI/text scaling and narrow width. The native capture regression runs the
real overlay painter and keyboard/accessibility paths in five theme/scale variants
at 960×540 and after resize to 800×480. End reaches the final recovery line in each
resized native variant. These are production offscreen captures and native host
input tests; they do not claim live compositor/VM or physical-device verification.

All render commands selected WGPU_BACKEND=vulkan and
VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json. The denied DRM path was
never retried.

Validation: baseline visible-reader regression failed because notification_read[0]
was absent. Afterward all 12 center cases, the portable WindowEvent cancellation
journey and both native reader journeys/capture cases pass. The native pointer
journey caught and fixes covered banners receiving history presses. Background
banner identities and history remain unchanged through read/back/repeated actions.
All-target Clippy with -D warnings, cargo check --locked -p shell --tests and staged
repository hooks pass.

| Image | Library file | File ID |
| --- | --- | --- |
| notification-reader-before-large-text.png | libfile_942371f4e3fc8191b0d88033c591dc32 | file_000000006e4881f5ad6a82ba555d31b4 |
| notification-reader-after-read-focus.png | libfile_95ea753387608191be957327d5909844 | file_00000000852481f99cce459386f3a599 |
| notification-reader-after-large-text-start.png | libfile_aed88d66e634819197f3c3fba6acc2e9 | file_00000000ca8c81f7958949a352c9bb5a |
| notification-reader-after-large-text-end.png | libfile_a8d6b1df28508191a0277b83cffea279 | file_000000006a1881f685f09905f6e63851 |
| notification-reader-after-light-start.png | libfile_746f5766bee8819192ab0bfad17cc438 | file_00000000f5e481f7bb118e8276254bcd |
| notification-reader-after-narrow-start.png | libfile_335be2583fb48191a00ecd62ae82652d | file_00000000ccd081fdb59251901dd12a5a |
| notification-reader-after-narrow-end.png | libfile_63fb9d8653c08191ac9d1d0594744bb8 | file_0000000044e481f5b4bb1c7d31a0962e |
| notification-reader-after-native-large-text-start.png | libfile_fe5b04061d6481918b26db70a51e6061 | file_0000000049ac81f7933a76f4002e0900 |
| notification-reader-after-native-large-text-end.png | libfile_aec698fc1d788191af7e34b6e0791e72 | file_00000000bf8881f788298eb452983889 |
| notification-reader-after-native-high-contrast-back.png | libfile_7c9aec92a5e881918a8354087fc08c08 | file_00000000005c81f6a095fd8ce526a10a |
| notification-reader-after-native-ui-text-resized.png | libfile_78ea3476e4dc8191b8915b4eb4c9bc5d | file_0000000038bc81fd8b1667395606cb17 |
