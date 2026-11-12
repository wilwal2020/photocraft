# craft-ui, patched for the Photoshop-parity fork

A copy of `crates/ui` from [storytold/craft-libs](https://github.com/storytold/craft-libs) at
`6e03f65bf45181db77ba6229716e6624db9eb25b` (the rev PhotoCraft 0.23.0 pins; first copied at 0.17.0's 92d392c, then c5a1cbf for 0.19), with these changes.
Every change is marked `PhotoCraft fork` in the source. When upstream PhotoCraft moves to a newer
craft-libs rev, take that rev's `crates/ui` and apply these again (or drop the vendored copy and
go back to the git dependency in the root `Cargo.toml` if upstream has them).

- `docking/placement.rs`, `docking/render/native.rs`: a floating panel's own OS window has a
  14 pt bar (was 18). The × is centred on the slimmer bar. (Its name on the bar, which the tab
  strip right under it shows too, upstream itself dropped by 6e03f65.)
- `docking/model.rs` (`Layout::apply_inner`): `Action::Move` of a panel not in the layout opens
  it at the target, as `Action::OpenAt` does. PhotoCraft's Tools panel is dragged in from
  outside the layout and docks among the panels this way.
- `docking/surface.rs`, `docking/mod.rs`, `docking/render/preview.rs`: `begin_external_drag`
  starts a tab drag of such a panel; the drag isn't cancelled for the panel being absent.

The manifest is standalone (no workspace inheritance) and the root workspace excludes `vendor/`,
so the workspace's stricter lints don't apply to this code. Its tests and examples are not built.

Copied again at 6e03f65 for PhotoCraft 0.23 (2026-11-12): the copy is rustfmt-formatted with the
root `rustfmt.toml`, so diff against a rustfmt-formatted upstream to see only these patches.
