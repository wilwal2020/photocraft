//! Conservative preflight for the main PSD pixel paths. This bounds decoded layer/mask/
//! merged and pattern pixels, not parser metadata or smart-filter caches (separate limits).
//!
//! Reserve the sum of all possible retained tiles and the largest workspace, without credit
//! for pruning or tile sharing. Reserving before any worker starts makes scheduling irrelevant.
//!
//! While idle tiles are kept compressed ([`photocraft_raster::spill::set_compression`], on in
//! the app) the tile pixels are counted at the size they are expected to compress to, judged by
//! how well the file's own encoding shrank each layer, plus the layers decoding at once
//! uncompressed. A colouring PSD of hundreds of full-canvas layers of flat colour and line art
//! needs a tenth of its pixels' size or less; that is how Photoshop opens it with little RAM.

use photocraft_geom::TILE_SIZE;
use photocraft_psd::{
    PsdFile,
    layer::{CHANNEL_REAL_USER_MASK, CHANNEL_USER_MASK, Rect},
};

use crate::IoError;

/// Default allowance for PSD layer, mask and merged-image pixels, including conversion
/// workspace and tile padding. Other format/parser limits still apply. Callers may lower or raise it
/// ([`crate::ImportOptions::psd_max_pixel_bytes`]).
pub const DEFAULT_PSD_IMPORT_PIXEL_BYTES: u64 = if cfg!(target_arch = "wasm32") { 1 << 30 } else { 2 << 30 };

/// Structured memory estimate for a PSD/PSB import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PsdMemoryEstimate {
    /// Bytes required for all possible retained layer, mask, and merged surfaces.
    pub retained_bytes: u64,
    /// Largest scratch/conversion workspace required during decoding.
    pub workspace_bytes: u64,
    /// Total peak requirement (retained + workspace).
    pub total_bytes: u64,
    /// The part of `retained_bytes` that is tile pixels (layer, mask, channel, fallback and
    /// pattern surfaces). Tiles can be spilled to the scratch disk while it is on, so admission
    /// does not bound them by physical RAM; the merged planes, pattern decodes and the workspace
    /// are ordinary memory.
    pub tile_bytes: u64,
    /// Tile pixels uncompressed: `tile_bytes` before compression credit. Equal to it while idle
    /// tiles are not compressed.
    pub raw_tile_bytes: u64,
}

#[derive(Default)]
struct ImportBudget {
    retained: u64,
    workspace: u64,
    tiles: u64,
    /// Expected compressed size of `tiles`, and each surface's uncompressed tile bytes.
    packed: u64,
    surfaces: Vec<u64>,
}

/// How a surface's tiles are expected to compress, from how well the file encoded the same
/// pixels: `(expected, raw)` byte weights summed over its stored channels.
#[derive(Clone, Copy, Default)]
struct Shrink {
    expected: u64,
    raw: u64,
}

impl Shrink {
    /// Uncompressed: no evidence it shrinks (raw or unknown channel data, merged images).
    const NONE: Shrink = Shrink { expected: 1, raw: 1 };

    /// From the channels of one surface, each `plane` bytes decoded. LZ4 on whole interleaved
    /// tiles does about as well as PackBits rows on flat colour and line art and far better on
    /// empty areas; allow 1.5x the RLE size for planes where PackBits wins (one flat channel
    /// beside noisy ones). zlib packs 2 to 4 times tighter than LZ4, so its size counts 4x.
    fn of<'a>(channels: impl Iterator<Item = &'a photocraft_psd::layer::ChannelData>, plane: u64) -> Shrink {
        use photocraft_psd::Compression;
        let mut s = Shrink::default();
        for ch in channels {
            let len = ch.encoded_len();
            let expected = match ch.compression {
                Some(Compression::Rle) => len.saturating_mul(3) / 2,
                Some(Compression::Zip | Compression::ZipPrediction) => len.saturating_mul(4),
                _ => plane,
            };
            s.expected = s.expected.saturating_add(expected.min(plane));
            s.raw = s.raw.saturating_add(plane);
        }
        // Tiles that don't shrink to 3/4 are kept uncompressed (spill's PACK_RATIO).
        if s.raw == 0 || s.expected.saturating_mul(4) > s.raw.saturating_mul(3) { Shrink::NONE } else { s }
    }

    /// The expected compressed size of `bytes` of tiles.
    fn apply(self, bytes: u64) -> u64 {
        if self.raw == 0 {
            return bytes;
        }
        (u128::from(bytes) * u128::from(self.expected)).div_ceil(u128::from(self.raw)).min(u128::from(bytes)) as u64
    }
}

#[cfg(any(test, feature = "testgen"))]
thread_local! {
    /// Extra bytes added to every estimate on this thread by [`with_test_estimate_charge`].
    static TEST_ESTIMATE_CHARGE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Extra tile-pixel bytes added by [`with_test_tile_charge`].
    static TEST_TILE_CHARGE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Adds `bytes` to every PSD memory estimate made on this thread while `f` runs.
///
/// A test seam: an admission-threshold test can use a tiny fixture, so nothing near a gigabyte
/// is ever allocated, while still exercising the real import path with its approval flags and
/// gate ordering. Scoped to the thread and restored on return or panic.
///
/// The charge lives on the calling thread. A real import that runs on another thread (or a
/// worker the engine starts for it) does not see it unless the caller propagates an explicitly
/// captured scope: [`crate::memory::TestMemoryScope`] captures this charge together with the
/// fixed mock snapshot, so an engine worker that enters that scope makes the same estimates as
/// the test thread. This is the seam engine and UI dev-dependency tests use through the
/// `testgen` feature; it is never process-global.
#[cfg(any(test, feature = "testgen"))]
pub fn with_test_estimate_charge<R>(bytes: u64, f: impl FnOnce() -> R) -> R {
    struct Reset(u64);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_ESTIMATE_CHARGE.with(|c| c.set(self.0));
        }
    }
    let prev = TEST_ESTIMATE_CHARGE.with(|c| c.replace(bytes));
    let _reset = Reset(prev);
    f()
}

/// [`with_test_estimate_charge`] for tile pixels: `bytes` are added to every estimate made on
/// this thread while `f` runs, as spillable tile bytes ([`PsdMemoryEstimate::tile_bytes`]), so a
/// scratch-aware admission test needs no huge fixture. Captured by
/// [`crate::memory::TestMemoryScope`] like the plain charge.
#[cfg(any(test, feature = "testgen"))]
pub fn with_test_tile_charge<R>(bytes: u64, f: impl FnOnce() -> R) -> R {
    struct Reset(u64);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_TILE_CHARGE.with(|c| c.set(self.0));
        }
    }
    let prev = TEST_TILE_CHARGE.with(|c| c.replace(bytes));
    let _reset = Reset(prev);
    f()
}

/// The calling thread's current (plain, tile) estimate charges, for an explicit scope to capture.
#[cfg(any(test, feature = "testgen"))]
pub(crate) fn test_estimate_charge() -> (u64, u64) {
    (TEST_ESTIMATE_CHARGE.with(|c| c.get()), TEST_TILE_CHARGE.with(|c| c.get()))
}

/// Enters captured estimate charges (an explicit [`crate::memory::TestMemoryScope`] worker) and
/// returns the charges they replaced, to restore after the scope ends.
#[cfg(any(test, feature = "testgen"))]
pub(crate) fn enter_test_estimate_charge(bytes: u64, tiles: u64) -> (u64, u64) {
    (TEST_ESTIMATE_CHARGE.with(|c| c.replace(bytes)), TEST_TILE_CHARGE.with(|c| c.replace(tiles)))
}

/// The test seam's extra (plain, tile) charges; zero in every other build.
fn test_estimate_charge_value() -> (u64, u64) {
    #[cfg(any(test, feature = "testgen"))]
    {
        test_estimate_charge()
    }
    #[cfg(not(any(test, feature = "testgen")))]
    {
        (0, 0)
    }
}

fn overflow() -> IoError {
    IoError::Unsupported("PSD import pixel memory estimate overflow".into())
}

// inflate_checked uses at least 8 KiB per stream, even for a one-pixel plane.
fn inflate_scratch(plane_bytes: u64, channels: u64) -> Result<u64, IoError> {
    mul(plane_bytes.saturating_add(1).clamp(8192, 1 << 20), channels)
}

fn mul(a: u64, b: u64) -> Result<u64, IoError> {
    a.checked_mul(b).ok_or_else(overflow)
}

impl ImportBudget {
    fn retain(&mut self, bytes: u64) -> Result<(), IoError> {
        self.retained = self.retained.checked_add(bytes).ok_or_else(overflow)?;
        Ok(())
    }

    fn scratch(&mut self, bytes: u64) {
        self.workspace = self.workspace.max(bytes);
    }

    fn required(&self) -> Result<u64, IoError> {
        self.retained.checked_add(self.workspace).ok_or_else(overflow)
    }

    fn surface(&mut self, rect: Rect, channels: u64, sample: u64, shrink: Shrink) -> Result<(), IoError> {
        if rect.is_empty() {
            return Ok(());
        }
        let tile = i64::from(TILE_SIZE);
        // Division must round down for negative offsets: even a 2x2 rect straddling the
        // origin occupies four tiles. Widen first, including the exclusive right/bottom edge.
        let nx = (i64::from(rect.right) - 1).div_euclid(tile) - i64::from(rect.left).div_euclid(tile) + 1;
        let ny = (i64::from(rect.bottom) - 1).div_euclid(tile) - i64::from(rect.top).div_euclid(tile) + 1;
        let bytes = mul(mul(mul(nx as u64, ny as u64)?, (TILE_SIZE as u64).pow(2))?, mul(channels, sample)?)?;
        self.packed = self.packed.saturating_add(shrink.apply(bytes));
        self.surfaces.push(bytes);
        self.tile(bytes)
    }

    /// Tile memory once idle tiles are compressed: the expected compressed size, plus the
    /// surfaces decoding at once (the import decodes a layer per core, largest first), which are
    /// whole until the residency sweep compresses them. Never more than the pixels themselves.
    fn compressed_tiles(&mut self) -> u64 {
        let workers = if cfg!(target_arch = "wasm32") { 1 } else { std::thread::available_parallelism().map_or(1, |v| v.get()).clamp(1, 32) };
        self.surfaces.sort_unstable_by(|a, b| b.cmp(a));
        let decoding = self.surfaces.iter().take(workers).fold(0u64, |n, &b| n.saturating_add(b));
        self.packed.saturating_add(decoding).min(self.tiles)
    }

    /// Retained tile pixels: spillable to the scratch disk.
    fn tile(&mut self, bytes: u64) -> Result<(), IoError> {
        self.retain(bytes)?;
        self.tiles = self.tiles.checked_add(bytes).ok_or_else(overflow)?;
        Ok(())
    }
}

/// Computes the conservative preflight pixel memory and workspace estimate for `file`.
///
/// No decoding here: even a compact ZIP stream must be refused before inflation. The sum
/// deliberately counts full decoded planes for every compression method, plus their row
/// buffers, interleave output and float conversion workspace.
pub fn estimate(file: &PsdFile) -> Result<PsdMemoryEstimate, IoError> {
    let mut budget = ImportBudget::default();
    let h = &file.header;
    h.validate()?;
    let sample = u64::from(h.depth.div_ceil(8));
    let cc = match h.color_mode {
        // Unknown modes use the RGB fallback, including its alpha channel.
        photocraft_psd::ColorMode::Unknown(_) => 3,
        mode => u64::from(mode.color_channels().unwrap_or(3)),
    };
    if super::psd_import::can_recover_layers(file) {
        for rec in file.layers() {
            if !rec.rect.is_empty() && rec.channels.iter().any(|c| c.id >= 0 || c.id == -1) {
                let (w, hh) = rec.rect.size()?;
                let pixels = mul(w as u64, hh as u64)?;
                let shrink = Shrink::of(rec.channels.iter().filter(|c| c.id >= 0 || c.id == -1), mul(pixels, sample)?);
                budget.surface(rec.rect, cc + 1, sample, shrink)?;
                // Whole ZIP planes + band planes + interleave + temporary float pixels.
                // Counting a full layer for each band buffer also covers a single very wide row.
                let workspace = mul(pixels, mul(cc + 1, sample * 3 + 4)?)?.checked_add(inflate_scratch(mul(pixels, sample)?, cc + 1)?).ok_or_else(overflow)?;
                // RLE row indexes live beside decoded planes.
                budget.scratch(workspace.checked_add(mul(hh as u64 + 1, (cc + 1) * 8)?).ok_or_else(overflow)?);
            }
            if let Some(mask) = rec.layer_mask() {
                for (id, rect) in [(CHANNEL_USER_MASK, mask.rect), (CHANNEL_REAL_USER_MASK, mask.real.map_or(mask.rect, |m| m.rect))] {
                    if rec.channel(id).is_none() || rect.is_empty() {
                        continue;
                    }
                    let (w, hh) = rect.size()?;
                    let shrink = Shrink::of(rec.channel(id).into_iter(), mul(mul(w as u64, hh as u64)?, sample)?);
                    budget.surface(rect, 1, sample, shrink)?;
                    // The layer's ZIP decoders have been dropped before record_mask runs.
                    let pixels = mul(w as u64, hh as u64)?;
                    budget.scratch(mul(pixels, sample * 3 + 4)?.checked_add(inflate_scratch(mul(pixels, sample)?, 1)?).ok_or_else(overflow)?);
                }
            }
        }
    }
    if super::psd_import::merged_needed(file) {
        let pixels = mul(u64::from(h.width), u64::from(h.height))?;
        let channels = u64::from(h.channels);
        let canvas = Rect::from_xywh(0, 0, h.width, h.height);
        // Multichannel and extra channels become individual grayscale surfaces. Reserving
        // their tiles separately matters for images smaller than a tile.
        budget.surface(canvas, channels.max(cc + 1), sample.max(1), Shrink::NONE)?;
        // A fallback surface can coexist with the layers when none produced usable pixels.
        budget.surface(canvas, cc + 1, sample.max(1), Shrink::NONE)?;
        // The merged planes stay alive while layers are built.
        let merged = mul(mul(h.row_bytes() as u64, u64::from(h.height))?, channels)?;
        budget.retain(merged)?;
        // Include float expansion/readback and RGBA fallback, even for packed bitmap input.
        let converted = mul(pixels, mul(channels.max(cc + 1), sample * 3 + 12)?)?;
        budget.scratch(converted.checked_add(inflate_scratch(merged, 1)?).ok_or_else(overflow)?);
    }
    let mut patterns = photocraft_psd::patterns::PatternBudget::default();
    for block in file.global_blocks.iter().filter(|b| crate::pattern_map::PATTERN_KEYS.contains(&b.key)) {
        // Earlier records can decode before a malformed later record discards the block.
        let items = photocraft_psd::patterns::pattern_block_memory(&block.data, &mut patterns);
        for p in items {
            let pixels = mul(u64::from(p.width), u64::from(p.height))?;
            let sample = u64::from(p.depth.div_ceil(8));
            let color = if p.mode == 2 { 3 } else { photocraft_psd::patterns::mode_channels(p.mode) as u64 };
            let alpha = u64::from(p.planes > photocraft_psd::patterns::mode_channels(p.mode));
            budget.surface(Rect::from_xywh(0, 0, p.width, p.height), color + alpha, sample, Shrink::NONE)?;
            // The block parser returns all planes together. Count them as retained so
            // previous converted patterns and later blocks cannot reuse this allowance.
            budget.retain(p.decoded_bytes)?;
            // Channel clones/bitmap expansion, indexed RGB expansion and interleave.
            // Include a source plane during padded channel placement as well.
            let converted = mul(pixels, mul((p.planes as u64).max(color + alpha) + color + alpha + 3, sample)?)?;
            budget.scratch(converted.checked_add(p.decoded_bytes).ok_or_else(overflow)?);
        }
    }
    let (charge, tile_charge) = test_estimate_charge_value();
    budget.retain(charge)?;
    budget.tile(tile_charge)?;
    budget.packed = budget.packed.saturating_add(tile_charge);
    let raw_tile_bytes = budget.tiles;
    if compressing() {
        let saved = budget.tiles.saturating_sub(budget.compressed_tiles());
        budget.retained = budget.retained.saturating_sub(saved);
        budget.tiles = budget.tiles.saturating_sub(saved);
    }
    let total_bytes = budget.required()?;
    Ok(PsdMemoryEstimate { retained_bytes: budget.retained, workspace_bytes: budget.workspace, total_bytes, tile_bytes: budget.tiles, raw_tile_bytes })
}

/// Whether idle tiles are being kept compressed, so the estimate may count them compressed.
fn compressing() -> bool {
    #[cfg(any(test, feature = "testgen"))]
    if let Some(on) = TEST_COMPRESSING.with(std::cell::Cell::get) {
        return on;
    }
    photocraft_raster::spill::compression_budget() > 0
}

#[cfg(any(test, feature = "testgen"))]
thread_local! {
    /// [`with_test_compression`]'s override of [`compressing`] on this thread.
    static TEST_COMPRESSING: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Runs `f` with estimates made on this thread treating idle tiles as compressed (`on`) or not,
/// whatever the process-wide setting, so a test needn't change it for other tests.
#[cfg(any(test, feature = "testgen"))]
pub fn with_test_compression<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_COMPRESSING.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(TEST_COMPRESSING.with(|c| c.replace(Some(on))));
    f()
}

/// Checks that `file`'s estimate does not exceed `limit`.
pub fn check(file: &PsdFile, limit: u64) -> Result<PsdMemoryEstimate, IoError> {
    let est = estimate(file)?;
    if est.total_bytes > limit {
        return Err(IoError::PsdImportBudget { required: est.total_bytes, limit, free_memory: None });
    }
    Ok(est)
}

#[cfg(test)]
mod tests;
