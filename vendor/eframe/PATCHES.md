# eframe, patched for the Photoshop-parity fork

A copy of `crates/eframe` from [storytold/egui](https://github.com/storytold/egui) at
`6d518fc799681571963842f95b55a6b68431b15a` (the rev the root `Cargo.toml` patches the egui family
to), with these changes. Every change is marked `PhotoCraft fork` in the source. When the root
moves to a newer egui rev, take that rev's `crates/eframe` and apply these again (or drop the copy
if upstream has them). The manifest is standalone (no workspace inheritance; the egui-family
crates resolve through the root's `[patch.crates-io]`), made by `_claude/m18/eframe_manifest.py`
from the crate's and the egui workspace's manifests.

- `native/wgpu_integration.rs`, `native/glow_integration.rs` (`render_immediate_viewport`): the
  viewport's platform output is handled with the current event loop
  (`handle_platform_output_with_event_loop`), so `PlatformOutput::cursor_image` applies in an
  immediate viewport too. PhotoCraft's Camera Raw window is one: its Zoom and Hand cursors were
  painted beside the arrow, a frame behind the pointer (user report: the zoom symbol lags).
