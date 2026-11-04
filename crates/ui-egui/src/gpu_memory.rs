//! The graphics memory other programs leave PhotoCraft. User report: with Photoshop holding 4.7 GB
//! of an 8 GB card, a document reopened after an update got 222 MB of the card, Windows kept the
//! other 1.5 GB of its textures in system memory, and every redraw took ten times as long.
//!
//! [`crate::Services::video_memory`] reports this process's budget (what the system lets it keep
//! on the card) and usage on the canvas's adapter. Past the budget PhotoCraft says so once, keeps
//! fewer layer pages on the card (the compositor's budget, [`crate::gpu_canvas::memory_budget`])
//! and makes its textures again inside that; once the card has room for what it wanted, it lifts
//! the cap and reloads the canvas, so its textures move back into the card's memory without a
//! restart.

use crate::PhotocraftApp;
use crate::gpu_canvas::MIN_GPU_BUDGET;

/// How often the budget is read, in seconds (each read builds a DXGI factory: about a millisecond).
const CHECK_EVERY: f64 = 2.0;

/// Room kept free under the budget, so the next brush dab or panel doesn't push it over again.
const HEADROOM: u64 = 256 << 20;

/// This process's share of the card's memory: what the system lets it keep there and what it
/// keeps (bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory {
    pub budget: u64,
    pub usage: u64,
}

/// While over budget: what PhotoCraft used when it went over (what must fit again before the
/// canvas reloads) and the layer-page budget it keeps to meanwhile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Short {
    pub wanted: u64,
    pub cap: u64,
}

/// The watch's state, kept on the app.
#[derive(Debug, Default)]
pub struct Watch {
    /// When the budget is read next (egui time, seconds).
    next: f64,
    short: Option<Short>,
}

impl Watch {
    /// The layer-page budget in force while the card is short of memory.
    pub fn cap(&self) -> Option<u64> {
        self.short.map(|s| s.cap)
    }
}

/// What a reading calls for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Keep,
    /// Over budget (the first time, or still over after the last cap): keep to this.
    Short(Short),
    /// The card has room for what PhotoCraft wanted again.
    Recover,
}

/// The step for reading `m` in state `short`, with the compositor allowed `pages` bytes of layer
/// pages: over the budget, the pages give up the overshoot and [`HEADROOM`] (never below
/// [`MIN_GPU_BUDGET`]); back under it with room for what was wanted, recover.
pub fn assess(short: Option<Short>, m: Memory, pages: u64) -> Step {
    let over = m.usage.saturating_sub(m.budget);
    match short {
        None if over > 0 => Step::Short(Short { wanted: m.usage, cap: pages.saturating_sub(over + HEADROOM).max(MIN_GPU_BUDGET) }),
        Some(s) if m.budget >= s.wanted.saturating_add(HEADROOM) => Step::Recover,
        Some(s) if over > 0 => {
            let cap = s.cap.saturating_sub(over + HEADROOM).max(MIN_GPU_BUDGET);
            if cap < s.cap { Step::Short(Short { wanted: s.wanted.max(m.usage), cap }) } else { Step::Keep }
        }
        _ => Step::Keep,
    }
}

/// Read the budget every [`CHECK_EVERY`] seconds while there is a GPU canvas and the platform
/// reports it (Windows), and act on it ([`apply`]).
pub fn watch(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    if now < app.gpu_memory.next {
        return;
    }
    app.gpu_memory.next = now + CHECK_EVERY;
    let (Some(gpu), Some(read)) = (app.gpu.as_ref(), app.services.video_memory.as_ref()) else { return };
    let info = gpu.adapter_info();
    let Some((budget, usage)) = read(info.vendor, info.device) else { return };
    let pages = gpu.memory_budget().unwrap_or(MIN_GPU_BUDGET);
    let m = Memory { budget, usage };
    let step = assess(app.gpu_memory.short, m, pages);
    apply(app, step, m);
    if app.gpu_memory.short.is_some() {
        // Notice the card's memory coming back without waiting for input.
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(CHECK_EVERY));
    }
}

/// Carry out `step`: tell the user (once per shortage), set the compositor's cap and make the
/// canvas's textures again within it, or lift the cap and reload the canvas into the card.
pub fn apply(app: &mut PhotocraftApp, step: Step, m: Memory) {
    let mb = |b: u64| b >> 20;
    match step {
        Step::Keep => {}
        Step::Short(s) => {
            let first = app.gpu_memory.short.is_none();
            app.gpu_memory.short = Some(s);
            log::warn!("graphics memory short: using {} MB of a {} MB budget; layer pages capped at {} MB", mb(m.usage), mb(m.budget), mb(s.cap));
            if let Some(gpu) = &app.gpu {
                gpu.set_memory_budget(s.cap.min(gpu.memory_budget().unwrap_or(u64::MAX)));
            }
            app.perf.gpu_budget_mb = mb(s.cap);
            if first {
                reload(app);
                crate::notices::post(
                    app,
                    tl!("Graphics memory is full"),
                    vec![
                        crate::i18n::fmt(
                            tl!("Other programs are using most of the graphics card's memory: PhotoCraft may keep {budget} there and needs {usage}, so Windows moved the rest to system memory and the canvas is slower."),
                            &[("budget", &format!("{} MB", mb(m.budget))), ("usage", &format!("{} MB", mb(m.usage)))],
                        ),
                        tl!("PhotoCraft now keeps fewer layers on the graphics card. Close programs that use a lot of graphics memory, such as Photoshop or a live wallpaper: the canvas goes back to full speed by itself once there is room.").into(),
                    ],
                    false,
                    None,
                );
            }
        }
        Step::Recover => {
            app.gpu_memory.short = None;
            log::info!("graphics memory back: a {} MB budget, {} MB in use; reloading the canvas", mb(m.budget), mb(m.usage));
            // `gpu_budget` sizes the compositor again without the cap on the next refresh.
            app.perf.gpu_budget_allowance = (u64::MAX, u64::MAX);
            reload(app);
            app.ui.status = tl!("Graphics memory is free again: the canvas was reloaded onto the graphics card.").to_string();
            app.ui.status_error = false;
        }
    }
}

/// Make the canvas's GPU textures again: free them all and redraw every view, so the new ones
/// are made where the system has room for them now.
fn reload(app: &mut PhotocraftApp) {
    if let Some(gpu) = &app.gpu {
        gpu.retain(&[]);
    }
    app.canvases.clear();
    app.proxy_uploaded = None;
}

#[cfg(test)]
mod tests;
