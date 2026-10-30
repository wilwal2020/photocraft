# craft-ui, patched for the Photoshop-parity fork

A copy of `crates/ui` from [storytold/craft-libs](https://github.com/storytold/craft-libs) at
`92d392cff132eaf53e13afff77f38b5c11b73aa0` (the rev PhotoCraft 0.17.0 pins), with these changes.
Every change is marked `PhotoCraft fork` in the source. When upstream PhotoCraft moves to a newer
craft-libs rev, take that rev's `crates/ui` and apply these again (or drop the vendored copy and
go back to the git dependency in the root `Cargo.toml` if upstream has them).

- `docking/placement.rs`, `docking/render/native.rs`: a floating panel's own OS window has a
  14 pt bar (was 18) without the panel's name, which the tab strip right under it shows (user
  report: the name showed twice). The × is centred on the slimmer bar.
- `docking/model.rs` (`Layout::apply_inner`): `Action::Move` of a panel not in the layout opens
  it at the target, as `Action::OpenAt` does. PhotoCraft's Tools panel is dragged in from
  outside the layout and docks among the panels this way.
- `docking/surface.rs`, `docking/mod.rs`, `docking/render/preview.rs`: `begin_external_drag`
  starts a tab drag of such a panel; the drag isn't cancelled for the panel being absent.

The manifest is standalone (no workspace inheritance) and the root workspace excludes `vendor/`,
so the workspace's stricter lints don't apply to this code. Its tests and examples are not built.
