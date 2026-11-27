//! Built-in brush presets (our own designs; sampled tips and textures are generated procedurally).
//!
//! Grouped like Photoshop's default Brushes panel folders: General, Dry Media, Wet Media and
//! Special Effects. Every preset is plain [`BrushSettings`] data, so it serialises, journals and
//! can be edited like any user preset.

use crate::brush::{
    BrushPreset, BrushSettings, ColorDynamics, Control, DualBrush, Dynamic, MaskMode, Pattern, PatternStyle, Scattering, ShapeDynamics, Smoothing, Texture,
    TipShape, Transfer,
};
use photocraft_color::BlendMode;

use crate::procedural::{
    bristle_clump_tip, bristle_tip, chalk_tip, charcoal_tip, dash_tip, dry_wash_edged_tip, dry_wash_soft_side_tip, dry_wash_square_tip, fine_spatter_tip, gouache_rim_tip,
    granulation_tile, grass_tip, leaf_tip, rake_tip, spatter_tip, sponge_tip, star_tip,
};

/// Preset groups, in panel order.
pub const GROUPS: [&str; 4] = ["General", "Dry Media", "Wet Media", "Special Effects"];

fn preset(group: &str, name: &str, brush: BrushSettings) -> BrushPreset {
    BrushPreset { name: name.to_string(), brush, builtin: true, group: group.to_string(), folder: Vec::new(), captures: Default::default(), tool: None }
}

fn pressure() -> Dynamic {
    Dynamic::controlled(Control::PenPressure)
}

fn pressure_min(minimum: f32) -> Dynamic {
    Dynamic { minimum, ..pressure() }
}

fn size_dyn(size: Dynamic) -> ShapeDynamics {
    ShapeDynamics { enabled: true, size, ..Default::default() }
}

fn transfer(opacity: Dynamic, flow: Dynamic) -> Transfer {
    Transfer { enabled: true, opacity, flow, ..Default::default() }
}

fn paper(seed: u32, depth: f32) -> Texture {
    Texture { enabled: true, pattern: Pattern::Procedural { style: PatternStyle::Paper, size: 128, seed }, depth, ..Default::default() }
}

/// Soft cloudy value noise, as watercolour settles unevenly.
fn cloud(seed: u32) -> Texture {
    Texture { enabled: true, pattern: Pattern::Procedural { style: PatternStyle::Noise, size: 256, seed }, ..Default::default() }
}

fn general(base: &BrushSettings) -> Vec<BrushPreset> {
    let g = |n: &str, b: BrushSettings| preset("General", n, b);
    // Dense dabs keep large hard-round outlines and pressure-opacity tapers continuous (#8842).
    let hard_round = BrushSettings { spacing: 0.05, ..base.clone() };
    vec![
        g("Hard Round", BrushSettings { size: 30.0, hardness: 1.0, ..hard_round.clone() }),
        g("Soft Round", BrushSettings { size: 45.0, hardness: 0.0, ..base.clone() }),
        g("Hard Round Fine", BrushSettings { size: 5.0, hardness: 1.0, ..hard_round.clone() }),
        g("Soft Round Large", BrushSettings { size: 300.0, hardness: 0.0, spacing: 0.15, ..base.clone() }),
        g("Hard Round Pressure Size", BrushSettings { size: 30.0, hardness: 1.0, shape_dynamics: size_dyn(pressure()), ..hard_round.clone() }),
        g("Soft Round Pressure Size", BrushSettings { size: 45.0, hardness: 0.0, shape_dynamics: size_dyn(pressure()), ..base.clone() }),
        g("Hard Round Pressure Opacity", BrushSettings { size: 30.0, hardness: 1.0, transfer: transfer(pressure(), Dynamic::default()), ..hard_round.clone() }),
        g("Soft Round Pressure Opacity", BrushSettings { size: 45.0, hardness: 0.0, transfer: transfer(pressure(), Dynamic::default()), ..base.clone() }),
        g("Hard Round Pressure Opacity and Flow", BrushSettings { size: 30.0, hardness: 0.9, transfer: transfer(pressure(), pressure_min(0.2)), ..hard_round }),
        g("Airbrush Soft", BrushSettings { size: 80.0, hardness: 0.0, flow: 0.1, spacing: 0.1, build_up: true, build_up_rate: 25.0, ..base.clone() }),
        g(
            "Airbrush Pressure Flow",
            BrushSettings {
                size: 120.0,
                hardness: 0.0,
                flow: 0.15,
                spacing: 0.1,
                build_up: true,
                build_up_rate: 30.0,
                transfer: transfer(Dynamic::default(), pressure()),
                ..base.clone()
            },
        ),
        g("Calligraphy Flat", BrushSettings { size: 28.0, hardness: 1.0, roundness: 0.2, angle: 45.0, spacing: 0.05, ..base.clone() }),
    ]
}

fn dry_media(base: &BrushSettings) -> Vec<BrushPreset> {
    let g = |n: &str, b: BrushSettings| preset("Dry Media", n, b);
    vec![
        g("Hard Pencil", BrushSettings { size: 3.0, hardness: 1.0, aliased: true, spacing: 0.1, ..base.clone() }),
        g(
            "Soft Pencil",
            BrushSettings {
                size: 6.0,
                hardness: 0.6,
                spacing: 0.08,
                shape_dynamics: size_dyn(pressure_min(0.4)),
                transfer: transfer(pressure_min(0.15), Dynamic::default()),
                texture: Texture { scale: 0.5, ..paper(21, 0.8) },
                ..base.clone()
            },
        ),
        g(
            "Chalk",
            BrushSettings {
                size: 40.0,
                tip: TipShape::Sampled(chalk_tip(64, 7)),
                spacing: 0.2,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    angle: Dynamic::jitter(1.0),
                    size: Dynamic { jitter: 0.15, ..Default::default() },
                    ..Default::default()
                },
                texture: paper(3, 0.6),
                ..base.clone()
            },
        ),
        g(
            "Charcoal",
            BrushSettings {
                size: 45.0,
                tip: TipShape::Sampled(charcoal_tip(64, 4)),
                spacing: 0.12,
                shape_dynamics: ShapeDynamics { enabled: true, size: pressure_min(0.5), angle: Dynamic::controlled(Control::Direction), ..Default::default() },
                transfer: transfer(pressure_min(0.3), Dynamic::default()),
                // Subtract takes paint away where the pattern is light (#3340); inverting keeps
                // these presets as they were designed, with the dark parts taking it away.
                texture: Texture { mode: MaskMode::Subtract, invert: true, ..paper(5, 0.7) },
                ..base.clone()
            },
        ),
        g(
            "Pastel",
            BrushSettings {
                size: 50.0,
                tip: TipShape::Sampled(chalk_tip(64, 31)),
                spacing: 0.15,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    angle: Dynamic::jitter(1.0),
                    roundness: Dynamic { jitter: 0.3, minimum: 0.5, ..Default::default() },
                    ..Default::default()
                },
                scattering: Scattering { enabled: true, scatter: Dynamic::jitter(0.25), ..Default::default() },
                texture: Texture {
                    enabled: true,
                    pattern: Pattern::Procedural { style: PatternStyle::Canvas, size: 64, seed: 2 },
                    depth: 0.55,
                    mode: MaskMode::Subtract,
                    invert: true,
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
        g(
            "Wax Crayon",
            BrushSettings {
                size: 22.0,
                hardness: 0.85,
                roundness: 0.7,
                spacing: 0.08,
                shape_dynamics: ShapeDynamics { enabled: true, angle: Dynamic::controlled(Control::Direction), ..Default::default() },
                texture: Texture { each_tip: true, mode: MaskMode::HardMix, depth_jitter: Dynamic::jitter(0.3), ..paper(17, 0.9) },
                ..base.clone()
            },
        ),
        g(
            "Dry Bristle",
            BrushSettings {
                size: 50.0,
                tip: TipShape::Sampled(bristle_tip(64, 5)),
                spacing: 0.04,
                shape_dynamics: ShapeDynamics { enabled: true, angle: Dynamic::controlled(Control::Direction), ..Default::default() },
                transfer: Transfer {
                    enabled: true,
                    flow: Dynamic { control: Control::Fade, fade_steps: 400, minimum: 0.1, ..Default::default() },
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
        g(
            "Rake",
            BrushSettings {
                size: 60.0,
                tip: TipShape::Sampled(rake_tip(64, 3, 7)),
                spacing: 0.03,
                shape_dynamics: ShapeDynamics { enabled: true, angle: Dynamic::controlled(Control::Direction), ..Default::default() },
                transfer: transfer(pressure_min(0.25), Dynamic::default()),
                ..base.clone()
            },
        ),
    ]
}

fn wet_media(base: &BrushSettings) -> Vec<BrushPreset> {
    let g = |n: &str, b: BrushSettings| preset("Wet Media", n, b);
    vec![
        g(
            "Watercolor Wet Edges",
            BrushSettings {
                size: 60.0,
                hardness: 0.3,
                wet_edges: true,
                texture: Texture { scale: 1.5, ..paper(9, 0.35) }.with_pattern_size(256),
                ..base.clone()
            },
        ),
        g(
            "Watercolor Wash",
            BrushSettings {
                size: 140.0,
                hardness: 0.0,
                flow: 0.35,
                spacing: 0.12,
                wet_edges: true,
                shape_dynamics: size_dyn(pressure_min(0.35)),
                transfer: transfer(Dynamic::default(), Dynamic { jitter: 0.2, ..pressure() }),
                texture: Texture { scale: 2.0, ..paper(13, 0.25) }.with_pattern_size(256),
                ..base.clone()
            },
        ),
        g(
            "Ink Pen",
            BrushSettings {
                size: 8.0,
                hardness: 1.0,
                spacing: 0.05,
                shape_dynamics: size_dyn(pressure_min(0.15)),
                smoothing: Smoothing { amount: 0.35, ..Default::default() },
                ..base.clone()
            },
        ),
        g(
            "Oil Bristle",
            BrushSettings {
                size: 70.0,
                tip: TipShape::Sampled(bristle_tip(64, 41)),
                spacing: 0.03,
                shape_dynamics: ShapeDynamics { enabled: true, angle: Dynamic::controlled(Control::Direction), size: pressure_min(0.6), ..Default::default() },
                color_dynamics: ColorDynamics { enabled: true, per_tip: false, fg_bg: Dynamic::jitter(0.15), brightness_jitter: 0.05, ..Default::default() },
                texture: Texture {
                    enabled: true,
                    pattern: Pattern::Procedural { style: PatternStyle::Canvas, size: 64, seed: 4 },
                    depth: 0.3,
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
        g(
            "Gouache Flat",
            BrushSettings {
                size: 40.0,
                hardness: 0.9,
                roundness: 0.35,
                spacing: 0.05,
                shape_dynamics: ShapeDynamics { enabled: true, angle: Dynamic::controlled(Control::Direction), ..Default::default() },
                transfer: transfer(Dynamic::default(), Dynamic { control: Control::Fade, fade_steps: 600, minimum: 0.35, ..Default::default() }),
                ..base.clone()
            },
        ),
        g(
            "Ink Wash Sponge",
            BrushSettings {
                size: 70.0,
                tip: TipShape::Sampled(sponge_tip(96, 8)),
                spacing: 0.3,
                flow: 0.6,
                wet_edges: true,
                shape_dynamics: ShapeDynamics { enabled: true, angle: Dynamic::jitter(1.0), size: Dynamic::jitter(0.2), ..Default::default() },
                ..base.clone()
            },
        ),
        g(
            "Color Ink Glaze",
            BrushSettings {
                size: 50.0,
                hardness: 0.8,
                spacing: 0.04,
                opacity: 0.85,
                mode: BlendMode::Multiply,
                wet_edges: true,
                shape_dynamics: size_dyn(pressure_min(0.5)),
                texture: Texture { scale: 1.5, depth: 0.65, contrast: 0.45, ..cloud(5) },
                ..base.clone()
            },
        ),
        g(
            "Cloud Wash",
            BrushSettings {
                size: 180.0,
                hardness: 0.0,
                spacing: 0.1,
                flow: 0.12,
                mode: BlendMode::Multiply,
                transfer: transfer(Dynamic::default(), pressure_min(0.3)),
                texture: Texture { scale: 3.0, depth: 0.7, contrast: 0.3, ..cloud(8) },
                color_dynamics: ColorDynamics { enabled: true, per_tip: false, hue_jitter: 0.01, brightness_jitter: 0.05, ..Default::default() },
                ..base.clone()
            },
        ),
        g(
            "Dry Wash",
            BrushSettings {
                size: 60.0,
                tip: TipShape::Sampled(dry_wash_square_tip(96, 5)),
                spacing: 0.04,
                opacity: 0.8,
                flow: 0.3,
                wet_edges: true,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    angle: Dynamic { jitter: 0.02, ..Dynamic::controlled(Control::Direction) },
                    size: Dynamic { jitter: 0.05, ..pressure_min(0.6) },
                    ..Default::default()
                },
                transfer: transfer(Dynamic::default(), pressure_min(0.3)),
                texture: Texture { scale: 1.0, ..paper(21, 0.3) },
                ..base.clone()
            },
        ),
        // Dry Wash for shadow shapes (user request, 2026-11-08, after comparing their colouring with
        // the reference pages): crisp sides so each shape reads as laid down in one pass, and
        // pigment granulation mottling the inside of the shape.
        g(
            "Dry Wash Grain",
            BrushSettings {
                size: 60.0,
                tip: TipShape::Sampled(dry_wash_edged_tip(96, 5, 0.06)),
                spacing: 0.04,
                opacity: 0.8,
                flow: 0.3,
                wet_edges: true,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    angle: Dynamic { jitter: 0.02, ..Dynamic::controlled(Control::Direction) },
                    size: Dynamic { jitter: 0.05, ..pressure_min(0.6) },
                    ..Default::default()
                },
                transfer: transfer(Dynamic::default(), pressure_min(0.3)),
                texture: Texture { enabled: true, pattern: Pattern::Tile(granulation_tile(256, 4)), depth: 0.8, ..Default::default() },
                ..base.clone()
            },
        ),
        // Dry Wash Grain with one soft, streaky side (user request, 2026-11-20: "one of the edges
        // soft, maybe a little textured soft"): a shadow shape with one hard edge and one that
        // feathers out. The tip turns with the stroke, so the soft side is on the left of the
        // direction of travel (paint the other way round to put it on the right); it was on the
        // right first, and faded too quickly ("make the other side the smooth one, and make it
        // smoother"), so the fade now runs over 1.4 of the half-width. Wet Edges is off:
        // its curve peaks at half coverage and drew a darker ring partway through the soft fade;
        // Opacity 0.67 keeps the middle of the wash as dense as Dry Wash Grain's (0.8 at Wet
        // Edges' 59 %), and the grain is shallower since Wet Edges no longer flattens it.
        g(
            "Dry Wash Grain Soft Edge",
            BrushSettings {
                size: 60.0,
                tip: TipShape::Sampled(dry_wash_soft_side_tip(96, 5, 0.06, 1.4)),
                spacing: 0.04,
                opacity: 0.67,
                flow: 0.3,
                wet_edges: false,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    angle: Dynamic { jitter: 0.02, ..Dynamic::controlled(Control::Direction) },
                    size: Dynamic { jitter: 0.05, ..pressure_min(0.6) },
                    ..Default::default()
                },
                transfer: transfer(Dynamic::default(), pressure_min(0.3)),
                texture: Texture { enabled: true, pattern: Pattern::Tile(granulation_tile(256, 4)), depth: 0.35, ..Default::default() },
                ..base.clone()
            },
        ),
        // Gouache highlights laid over the colour (user request, after crops of the same pages,
        // reworked twice on their feedback): opaque light paint that piles up at the stroke's
        // edges in a bright bead, with a thinner, streaky middle; pressure swells a stroke into
        // a rounded blob and lifts it off to a fine tail.
        g(
            "Gouache Highlight",
            BrushSettings {
                size: 30.0,
                tip: TipShape::Sampled(gouache_rim_tip(96, 7)),
                spacing: 0.05,
                flow: 0.8,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    size: Dynamic { jitter: 0.08, ..pressure_min(0.08) },
                    angle: Dynamic::controlled(Control::Direction),
                    ..Default::default()
                },
                // Loaded for the first dabs (a crisp, solid start), then the low flow at which
                // the tip's side beads build up brighter than its thin, streaky middle.
                transfer: transfer(Dynamic::default(), Dynamic { fade_steps: 12, minimum: 0.25, ..Dynamic::controlled(Control::Fade) }),
                ..base.clone()
            },
        ),
        // A dry bristle-clump brush that answers pen pressure the way a real brush does (user
        // request: modelled on how a brush they like responds, with our own tip): pressing
        // harder widens and rounds the mark and turns the tip, so a light touch draws a thin
        // dragged line and a firm one a broad, split, streaky stroke; Wet Edges lets the paint
        // pile up at the edges; a dual brush of scattered drops burns small breaks into it.
        g(
            "Bristle Highlight",
            BrushSettings {
                size: 30.0,
                tip: TipShape::Sampled(bristle_clump_tip(224, 184, 3)),
                spacing: 0.04,
                angle: -73.0,
                wet_edges: true,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    size: pressure_min(0.31),
                    angle: pressure(),
                    roundness: pressure_min(0.33),
                    tilt_scale: 2.0,
                    ..Default::default()
                },
                dual_brush: DualBrush {
                    enabled: true,
                    mode: MaskMode::ColorBurn,
                    tip: TipShape::Sampled(spatter_tip(64, 41, 6)),
                    size: 66.0,
                    spacing: 1.9,
                    scatter: 0.05,
                    both_axes: true,
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
    ]
}

fn special_effects(base: &BrushSettings) -> Vec<BrushPreset> {
    let g = |n: &str, b: BrushSettings| preset("Special Effects", n, b);
    vec![
        g(
            "Spatter",
            BrushSettings {
                size: 60.0,
                tip: TipShape::Sampled(spatter_tip(64, 11, 14)),
                spacing: 0.6,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    size: Dynamic { jitter: 0.6, minimum: 0.2, ..Default::default() },
                    angle: Dynamic::jitter(1.0),
                    flip_x_jitter: true,
                    flip_y_jitter: true,
                    ..Default::default()
                },
                scattering: Scattering { enabled: true, scatter: Dynamic::jitter(1.5), both_axes: true, count: 2, count_jitter: Dynamic::jitter(0.5) },
                ..base.clone()
            },
        ),
        // A fine flicked spray of paint (mostly pin-pricks, now and then a bigger drop), for
        // white specks over colour.
        g(
            "Fine Spatter",
            BrushSettings {
                size: 160.0,
                tip: TipShape::Sampled(fine_spatter_tip(256, 5, 70)),
                spacing: 0.35,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    size: Dynamic { jitter: 0.4, minimum: 0.4, ..Default::default() },
                    angle: Dynamic::jitter(1.0),
                    flip_x_jitter: true,
                    flip_y_jitter: true,
                    ..Default::default()
                },
                transfer: transfer(Dynamic::jitter(0.3), Dynamic::default()),
                ..base.clone()
            },
        ),
        g(
            "Stipple",
            BrushSettings {
                size: 40.0,
                tip: TipShape::Sampled(spatter_tip(64, 77, 40)),
                spacing: 0.5,
                shape_dynamics: ShapeDynamics { enabled: true, angle: Dynamic::jitter(1.0), flip_x_jitter: true, ..Default::default() },
                scattering: Scattering { enabled: true, scatter: Dynamic::jitter(0.6), both_axes: true, ..Default::default() },
                transfer: transfer(pressure(), Dynamic::default()),
                ..base.clone()
            },
        ),
        g(
            "Confetti",
            BrushSettings {
                size: 24.0,
                hardness: 1.0,
                roundness: 0.5,
                spacing: 1.2,
                shape_dynamics: ShapeDynamics { enabled: true, angle: Dynamic::jitter(1.0), size: Dynamic::jitter(0.5), ..Default::default() },
                scattering: Scattering { enabled: true, scatter: Dynamic::jitter(3.0), both_axes: true, count: 3, ..Default::default() },
                color_dynamics: ColorDynamics { enabled: true, per_tip: true, hue_jitter: 1.0, saturation_jitter: 0.2, purity: 0.5, ..Default::default() },
                ..base.clone()
            },
        ),
        g(
            "Foliage",
            BrushSettings {
                size: 50.0,
                tip: TipShape::Sampled(leaf_tip(64)),
                spacing: 0.35,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    size: Dynamic { jitter: 0.5, minimum: 0.3, ..Default::default() },
                    angle: Dynamic::jitter(1.0),
                    roundness: Dynamic { jitter: 0.4, minimum: 0.4, ..Default::default() },
                    flip_y_jitter: true,
                    ..Default::default()
                },
                scattering: Scattering { enabled: true, scatter: Dynamic::jitter(1.2), both_axes: true, count: 2, count_jitter: Dynamic::jitter(0.5) },
                color_dynamics: ColorDynamics {
                    enabled: true,
                    per_tip: true,
                    fg_bg: Dynamic::jitter(0.6),
                    hue_jitter: 0.04,
                    brightness_jitter: 0.12,
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
        g(
            "Grass",
            BrushSettings {
                size: 70.0,
                tip: TipShape::Sampled(grass_tip(96, 6, 11)),
                spacing: 0.25,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    size: Dynamic { jitter: 0.4, minimum: 0.4, ..pressure() },
                    angle: Dynamic::jitter(0.06),
                    flip_x_jitter: true,
                    ..Default::default()
                },
                scattering: Scattering { enabled: true, scatter: Dynamic::jitter(0.3), ..Default::default() },
                color_dynamics: ColorDynamics { enabled: true, per_tip: true, fg_bg: Dynamic::jitter(0.7), hue_jitter: 0.03, ..Default::default() },
                ..base.clone()
            },
        ),
        // Dry-brush dashes laid across the stroke; pen pressure sets each dash's length
        // (Roundness squashes the upright tip's height), its width stays put.
        g(
            "Pressure Dashes",
            BrushSettings {
                size: 40.0,
                tip: TipShape::Sampled(dash_tip(40, 128, 3)),
                spacing: 1.7,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    angle: Dynamic { jitter: 0.015, ..Dynamic::controlled(Control::Direction) },
                    roundness: pressure_min(0.35),
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
        g(
            "Sparkle",
            BrushSettings {
                size: 40.0,
                tip: TipShape::Sampled(star_tip(64)),
                spacing: 1.5,
                shape_dynamics: ShapeDynamics { enabled: true, size: Dynamic { jitter: 0.8, minimum: 0.15, ..Default::default() }, ..Default::default() },
                scattering: Scattering { enabled: true, scatter: Dynamic::jitter(2.5), both_axes: true, ..Default::default() },
                transfer: transfer(Dynamic::jitter(0.5), Dynamic::default()),
                ..base.clone()
            },
        ),
        g(
            "Sponge Texture",
            BrushSettings {
                size: 60.0,
                tip: TipShape::Sampled(sponge_tip(64, 19)),
                spacing: 0.45,
                shape_dynamics: ShapeDynamics { enabled: true, angle: Dynamic::jitter(1.0), ..Default::default() },
                transfer: transfer(Dynamic::jitter(0.3), Dynamic::default()),
                ..base.clone()
            },
        ),
        g(
            "Canvas Texture",
            BrushSettings {
                size: 50.0,
                hardness: 0.8,
                texture: Texture {
                    enabled: true,
                    pattern: Pattern::Procedural { style: PatternStyle::Canvas, size: 64, seed: 1 },
                    mode: MaskMode::Subtract,
                    invert: true,
                    depth: 0.7,
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
        g(
            "Halftone Dots",
            BrushSettings {
                size: 80.0,
                hardness: 0.5,
                texture: Texture {
                    enabled: true,
                    pattern: Pattern::Procedural { style: PatternStyle::Dots, size: 64, seed: 1 },
                    mode: MaskMode::HardMix,
                    depth: 1.0,
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
        g(
            "Dual Grain",
            BrushSettings {
                size: 50.0,
                hardness: 0.6,
                noise: true,
                dual_brush: DualBrush {
                    enabled: true,
                    tip: TipShape::Sampled(spatter_tip(48, 23, 20)),
                    size: 30.0,
                    spacing: 0.3,
                    scatter: 1.0,
                    both_axes: true,
                    count: 2,
                    flip: true,
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
    ]
}

impl Texture {
    fn with_pattern_size(mut self, n: u32) -> Self {
        if let Pattern::Procedural { size, .. } = &mut self.pattern {
            *size = n;
        }
        self
    }
}

/// The built-in preset set, in display order (grouped by [`GROUPS`]).
pub fn builtin() -> Vec<BrushPreset> {
    let base = BrushSettings { pressure_size: false, spacing: 0.25, ..Default::default() };
    let mut v = general(&base);
    v.extend(dry_media(&base));
    v.extend(wet_media(&base));
    v.extend(special_effects(&base));
    v
}

/// Find a preset by name (case-insensitive).
pub fn find<'a>(presets: &'a [BrushPreset], name: &str) -> Option<&'a BrushPreset> {
    presets.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_grouped_and_unique() {
        let v = builtin();
        assert!((30..=48).contains(&v.len()), "{}", v.len());
        let mut names: Vec<&str> = v.iter().map(|p| p.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), v.len(), "duplicate preset names");
        for g in GROUPS {
            assert!(v.iter().filter(|p| p.group == g).count() >= 6, "{g}");
        }
        assert!(v.iter().all(|p| p.builtin && GROUPS.contains(&p.group.as_str())));
        // Groups are contiguous, in panel order.
        let order: Vec<usize> = v.iter().filter_map(|p| GROUPS.iter().position(|g| *g == p.group)).collect();
        assert!(order.windows(2).all(|w| w[0] <= w[1]));
        // Pressure variants respond to pressure.
        let ps = find(&v, "Hard Round Pressure Size").map(|p| p.brush.shape_dynamics.size.control);
        assert_eq!(ps, Some(Control::PenPressure));
    }

    /// Paints a horizontal "Pressure Dashes" stroke at `pressure`: the painted rows' extent
    /// (the dash length, across the stroke) and how many separate dashes the centre row crosses.
    fn dashes(pressure: f32) -> (i32, usize) {
        use photocraft_color::PixelFormat;
        use photocraft_raster::Surface;
        let b = find(&builtin(), "Pressure Dashes").map(|p| p.brush.clone()).unwrap_or_default();
        let pts: Vec<_> =
            (0..=150).map(|i| crate::StrokePoint { time: f64::from(i) * 4.0, ..crate::StrokePoint::new(20.0 + f64::from(i) * 2.0, 60.0, pressure) }).collect();
        let mut s = Surface::new(PixelFormat::RGBA32F);
        s.fill_rect(photocraft_geom::Rect::new(0, 0, 340, 120), &[1.0, 1.0, 1.0, 0.0]);
        crate::render::render_stroke(&mut s, &b, &pts, None, false, 1.0);
        let painted = |x: i32, y: i32| s.rgba(x, y)[3] > 0.25;
        let rows: Vec<i32> = (0..120).filter(|&y| (100..240).any(|x| painted(x, y))).collect();
        let len = rows.last().zip(rows.first()).map_or(0, |(a, b)| a - b + 1);
        let runs = (100..240).filter(|&x| painted(x, 60) && !painted(x - 1, 60)).count();
        (len, runs)
    }

    #[test]
    fn pressure_dashes_get_longer_with_pressure_not_wider() {
        let (full, full_runs) = dashes(1.0);
        let (light, light_runs) = dashes(0.2);
        // Size 40: a full-pressure dash spans about the size across the stroke; at 20 % pressure
        // Roundness is 0.35 + 0.65·0.2 ≈ 0.48 of it.
        assert!((33..=42).contains(&full), "full-pressure dash {full} px long");
        assert!((15..=24).contains(&light), "light dash {light} px long");
        // Separate dashes, about the same count either way (pressure doesn't touch spacing).
        // Spacing 170 % of the 12.5 px dash width: a dash every 21 px, gaps a little narrower.
        assert!((6..=8).contains(&full_runs), "{full_runs} dashes over 140 px");
        // (A dash cut by the measured window's edge may count once more on one of them.)
        assert!(full_runs.abs_diff(light_runs) <= 1, "{full_runs} vs {light_runs}");
    }

    /// Paints `strokes` of the preset `name` in cyan on white paper (each a list of x positions
    /// along the row y = 60, at full pressure); the red channel along y = 60 for x in 60..200.
    fn glazed(name: &str, strokes: &[&[f64]]) -> Vec<f32> {
        use photocraft_color::PixelFormat;
        use photocraft_raster::Surface;
        let b = find(&builtin(), name).map(|p| BrushSettings { color: [0.5, 0.85, 0.9, 1.0], ..p.brush.clone() }).unwrap_or_default();
        let mut s = Surface::new(PixelFormat::RGBA32F);
        s.fill_rect(photocraft_geom::Rect::new(0, 0, 260, 120), &[1.0, 1.0, 1.0, 1.0]);
        for (k, xs) in strokes.iter().enumerate() {
            let pts: Vec<_> = xs.iter().map(|&x| crate::StrokePoint::new(x, 60.0, 1.0)).collect();
            crate::render::render_stroke(&mut s, &BrushSettings { seed: k as u64 + 1, ..b.clone() }, &pts, None, false, 1.0);
        }
        (60..200).map(|x| s.rgba(x, 60)[0]).collect()
    }

    fn mean(v: &[f32]) -> f32 {
        v.iter().sum::<f32>() / v.len() as f32
    }

    /// The colouring brushes (user request, after the colour pages of a manga): see-through
    /// washes that glaze darker stroke over stroke but stay even within one stroke, mottled.
    #[test]
    fn color_ink_glaze_layers_between_strokes_not_within_one() {
        let once = glazed("Color Ink Glaze", &[&[20.0, 240.0]]);
        let there_and_back = glazed("Color Ink Glaze", &[&[20.0, 240.0, 20.0]]);
        let twice = glazed("Color Ink Glaze", &[&[20.0, 240.0], &[20.0, 240.0]]);
        let (m1, mb, m2) = (mean(&once), mean(&there_and_back), mean(&twice));
        // See-through: one stroke tints white paper (red 1 → about 0.6), not to the colour's 0.5.
        assert!((0.55..0.85).contains(&m1), "one stroke {m1}");
        assert!((mb - m1).abs() < 0.01, "a stroke doesn't build up over itself: {m1} vs {mb}");
        assert!(m2 < m1 - 0.08, "a second stroke glazes darker: {m1} → {m2}");
        // The cloudy texture mottles the wash.
        let (lo, hi) = once.iter().fold((1.0f32, 0.0f32), |(a, b), &v| (a.min(v), b.max(v)));
        assert!(hi - lo > 0.03, "flat wash {lo}..{hi}");
        let all = builtin();
        for name in ["Color Ink Glaze", "Cloud Wash"] {
            let p = find(&all, name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!((p.group.as_str(), p.brush.mode), ("Wet Media", BlendMode::Multiply), "{name}");
        }
        // Dry Wash paints in Normal mode (the user's call, 2026-11-08).
        assert_eq!(find(&all, "Dry Wash").map(|p| (p.group.as_str(), p.brush.mode)), Some(("Wet Media", BlendMode::Normal)));
    }

    /// Dry Wash across a horizontal stroke at `pressure`: the red channel down the column x = 130
    /// (cyan on white, as in `glazed`).
    fn dry_wash_across(pressure: f32) -> Vec<f32> {
        use photocraft_color::PixelFormat;
        use photocraft_raster::Surface;
        let b = find(&builtin(), "Dry Wash").map(|p| BrushSettings { color: [0.5, 0.85, 0.9, 1.0], seed: 3, ..p.brush.clone() }).unwrap_or_default();
        let mut s = Surface::new(PixelFormat::RGBA32F);
        s.fill_rect(photocraft_geom::Rect::new(0, 0, 260, 120), &[1.0, 1.0, 1.0, 1.0]);
        let pts: Vec<_> = (0..=110).map(|i| crate::StrokePoint::new(20.0 + f64::from(i) * 2.0, 60.0, pressure)).collect();
        crate::render::render_stroke(&mut s, &b, &pts, None, false, 1.0);
        (0..120).map(|y| s.rgba(130, y)[0]).collect()
    }

    /// The Dry Wash the user wanted refined (2026-11-08), squared off after their back-and-forth
    /// test showed hard stacked arcs at the edges: a flat wash, wider when pressed, whose sides
    /// fade out over a few pixels with no lone streak standing outside the body.
    #[test]
    fn dry_wash_is_a_flat_wash_with_soft_even_sides() {
        let (light, full) = (dry_wash_across(0.1), dry_wash_across(1.0));
        let painted = |v: &[f32]| v.iter().filter(|&&r| r < 0.97).count();
        assert!(painted(&full) as f32 > painted(&light) as f32 * 1.3, "pressing widens: {} vs {}", painted(&full), painted(&light));
        for (v, p) in [(&light, 0.1), (&full, 1.0)] {
            let rows: Vec<usize> = (0..120).filter(|&y| v[y] < 0.97).collect();
            let (top, bottom) = (rows[0], rows[rows.len() - 1]);
            assert_eq!(rows.len(), bottom - top + 1, "a gap across the wash at pressure {p}");
            let body = &v[top + 6..=bottom - 6];
            assert!(mean(body) < 0.85, "a pale wash at pressure {p}: {}", mean(body));
            // Paint thins out within a few pixels of each side, rather than ending in a hard line.
            assert!(v[top] > mean(body) + 0.05 && v[bottom] > mean(body) + 0.05, "hard sides at pressure {p}: {} {} vs {}", v[top], v[bottom], mean(body));
        }
    }

    /// One horizontal stroke of `name` in brown over pale skin at full pressure.
    fn shadow_stroke(name: &str) -> photocraft_raster::Surface {
        use photocraft_color::PixelFormat;
        use photocraft_raster::Surface;
        let b = find(&builtin(), name).map(|p| BrushSettings { color: [0.6, 0.3, 0.15, 1.0], seed: 3, ..p.brush.clone() }).unwrap_or_default();
        let mut s = Surface::new(PixelFormat::RGBA32F);
        s.fill_rect(photocraft_geom::Rect::new(0, 0, 260, 120), &[1.0, 0.9, 0.8, 1.0]);
        let pts: Vec<_> = (0..=110).map(|i| crate::StrokePoint::new(20.0 + f64::from(i) * 2.0, 60.0, 1.0)).collect();
        crate::render::render_stroke(&mut s, &b, &pts, None, false, 1.0);
        s
    }

    /// Dry Wash Grain, for shadow shapes (user request after comparing their colouring with the
    /// reference pages, 2026-11-08): sides that go from paper to the full wash in fewer pixels
    /// than Dry Wash, and pigment mottling inside the stroke.
    #[test]
    fn dry_wash_grain_has_crisp_sides_and_a_mottled_inside() {
        let (soft, grain) = (shadow_stroke("Dry Wash"), shadow_stroke("Dry Wash Grain"));
        assert_eq!(find(&builtin(), "Dry Wash Grain").map(|p| (p.group.as_str(), p.brush.mode)), Some(("Wet Media", BlendMode::Normal)));
        // Across the stroke, averaged over several columns: rows partway between paper and wash.
        let ramp = |s: &photocraft_raster::Surface| {
            (100..160)
                .step_by(6)
                .map(|x| {
                    let col: Vec<f32> = (0..120).map(|y| 1.0 - s.rgba(x, y)[2] / 0.8).collect();
                    let full = col[50..70].iter().sum::<f32>() / 20.0;
                    col.iter().filter(|&&c| c > 0.1 * full && c < 0.7 * full).count()
                })
                .sum::<usize>()
        };
        assert!(ramp(&grain) * 10 < ramp(&soft) * 9, "crisper sides: {} vs {} partial rows", ramp(&grain), ramp(&soft));
        // Along the middle of the stroke: the paint varies more from pixel to pixel.
        let spread = |s: &photocraft_raster::Surface| {
            let v: Vec<f32> = (50..70).flat_map(|y| (60..200).map(move |x| (x, y))).map(|(x, y)| s.rgba(x, y)[2]).collect();
            let m = v.iter().sum::<f32>() / v.len() as f32;
            (v.iter().map(|a| (a - m) * (a - m)).sum::<f32>() / v.len() as f32).sqrt()
        };
        assert!(spread(&grain) > 1.5 * spread(&soft), "mottled: {} vs {}", spread(&grain), spread(&soft));
    }

    /// One horizontal stroke of `name` in pale pink over orange at `pressure` (or a pressure
    /// ramp from 1 to 0 with `None`).
    fn highlight(name: &str, pressure: Option<f32>, seed: u64) -> photocraft_raster::Surface {
        use photocraft_color::PixelFormat;
        use photocraft_raster::Surface;
        let b = find(&builtin(), name).map(|p| BrushSettings { color: [1.0, 0.8, 0.75, 1.0], seed, ..p.brush.clone() }).unwrap_or_default();
        let mut s = Surface::new(PixelFormat::RGBA32F);
        s.fill_rect(photocraft_geom::Rect::new(0, 0, 300, 120), &[1.0, 0.55, 0.05, 1.0]);
        let pts: Vec<_> = (0..=100).map(|i| crate::StrokePoint::new(30.0 + f64::from(i) * 2.4, 60.0, pressure.unwrap_or(1.0 - i as f32 / 100.0))).collect();
        crate::render::render_stroke(&mut s, &b, &pts, None, false, 1.0);
        s
    }

    /// Coverage of the pink over orange at a pixel (blue goes 0.05 → 0.75).
    fn pink(s: &photocraft_raster::Surface, x: i32, y: i32) -> f32 {
        (s.rgba(x, y)[2] - 0.05) / 0.7
    }

    /// Painted highlights (user request, 2026-11-08, after crops of the same pages, reworked on
    /// their feedback: "depth near the edges where the paint piles up"): light paint over the
    /// colour whose edges build up to a bright bead around a thinner, streaky middle; loaded and
    /// solid at the start; swelling to a blob and lifting off to a fine tail with pressure.
    #[test]
    fn gouache_highlight_piles_up_at_its_edges() {
        let all = builtin();
        let p = find(&all, "Gouache Highlight").expect("preset");
        assert_eq!((p.group.as_str(), p.brush.mode), ("Wet Media", BlendMode::Normal));
        let width = |s: &photocraft_raster::Surface, x: i32| (20..100).filter(|&y| pink(s, x, y) > 0.3).count();
        let full = highlight("Gouache Highlight", Some(1.0), 3);
        let light = highlight("Gouache Highlight", Some(0.15), 3);
        let (wf, wl) = (width(&full, 150), width(&light, 150));
        assert!((22..=34).contains(&wf), "full width {wf}");
        assert!(wl * 3 < wf && wl >= 2, "light pressure gives a thin line: {wl} vs {wf}");
        // Across the stroke: the paint near both edges is thicker than in the middle.
        let across: Vec<f32> = (20..100).map(|y| pink(&full, 150, y)).filter(|&v| v > 0.3).collect();
        let n = across.len();
        let edge = |r: std::ops::Range<usize>| across[r].iter().fold(0.0f32, |m, &v| m.max(v));
        let middle = mean(&across[n * 3 / 10..n * 7 / 10]);
        let (top, bottom) = (edge(0..n / 4), edge(n * 3 / 4..n));
        // (Measured 0.84 and 0.83 at the edges, 0.72 in the middle.)
        assert!(top > middle + 0.08 && bottom > middle + 0.08, "edges {top} {bottom} vs middle {middle}");
        assert!(middle > 0.4 && top > 0.75, "too thin: middle {middle}, edge {top}");
        // Loaded at the start: the first dabs paint solid.
        let start = mean(&(32..40).map(|x| pink(&full, x, 60)).collect::<Vec<_>>());
        assert!(start > 0.85 && start > pink(&full, 150, 60) + 0.1, "start {start}");
        // A pressure ramp from 1 to 0 tapers to a tail.
        let ramp = highlight("Gouache Highlight", None, 4);
        let (head, tail) = (width(&ramp, 40), width(&ramp, 240));
        assert!(head > 20 && tail < 8, "taper {head} → {tail}");
    }

    #[test]
    fn fine_spatter_flicks_sparse_specks() {
        let all = builtin();
        assert_eq!(find(&all, "Fine Spatter").map(|p| p.group.as_str()), Some("Special Effects"));
        let s = highlight("Fine Spatter", Some(1.0), 5);
        // Specks spread well beyond a stroke's width, covering only a little of it.
        let hit: Vec<(i32, i32)> = (0..120).flat_map(|y| (0..300).map(move |x| (x, y))).filter(|&(x, y)| pink(&s, x, y) > 0.5).collect();
        let frac = hit.len() as f32 / (300.0 * 120.0);
        assert!((0.002..0.06).contains(&frac), "coverage {frac}");
        let (ymin, ymax) = hit.iter().fold((120, 0), |(a, b), &(_, y)| (a.min(y), b.max(y)));
        assert!(ymax - ymin > 60, "specks spread {ymin}..{ymax}");
    }

    /// A brush that answers pressure like a real one (user request, modelled on the dynamics of a
    /// brush they like, with our own tip): light pressure draws a thin dragged line, firm
    /// pressure a broad stroke.
    #[test]
    fn bristle_highlight_widens_with_pressure() {
        let all = builtin();
        let p = find(&all, "Bristle Highlight").expect("preset");
        assert_eq!(p.group, "Wet Media");
        let b = &p.brush;
        assert!(b.wet_edges && b.dual_brush.enabled && b.dual_brush.mode == MaskMode::ColorBurn);
        assert_eq!((b.shape_dynamics.angle.control, b.shape_dynamics.roundness.control), (Control::PenPressure, Control::PenPressure));
        let painted = |pr: f32| {
            let s = highlight("Bristle Highlight", Some(pr), 3);
            (60..240).map(|x| (10..110).filter(|&y| pink(&s, x, y) > 0.2).count()).sum::<usize>() as f32 / 180.0
        };
        let (light, firm) = (painted(0.2), painted(1.0));
        assert!(light > 1.0 && firm > light * 1.8, "mean painted height: light {light}, firm {firm}");
    }
}
