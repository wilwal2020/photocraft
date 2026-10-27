//! Slow frames in the log (user report: painting is smooth, then lags for about a second, panning
//! too, then is smooth again). A frame whose UI build takes [`SLOW_BUILD_MS`] or more, or that
//! starts [`SLOW_GAP_MS`] or more after the one before while a button is held and the pointer
//! moves (a stroke or a pan going on: a pointer held still draws no frames, which is no lag), is
//! logged as a warning with where its time went: the canvas refresh, eframe's CPU
//! time for the frame before (tessellation, GPU submission), and the perf spans panel by panel. At
//! most one line every [`EVERY_MS`], so a run of slow frames doesn't flood the log. The log file
//! (`photocraft.log` in the settings folder's `logs`; on Windows `%APPDATA%\Photocraft\logs`) then
//! names what a spike was without a trace running.

/// A UI build this long is logged (ms).
pub(crate) const SLOW_BUILD_MS: f64 = 150.0;
/// A gap between frames this long, with a button held, is logged (ms).
pub(crate) const SLOW_GAP_MS: f64 = 250.0;
/// The fewest ms between two logged frames.
const EVERY_MS: f64 = 2000.0;

/// What one frame took.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Frame {
    /// When it started, ms ([`crate::gpu_canvas::now_ms`]).
    pub start: f64,
    /// Building the UI.
    pub build: f64,
    /// The canvas refresh within it, and what kind it was.
    pub refresh: f64,
    pub refresh_kind: &'static str,
    /// eframe's CPU time for the frame before, ms.
    pub cpu_prev: Option<f64>,
    /// A pointer button is held.
    pub held: bool,
    /// The pointer moved since the frame before.
    pub moved: bool,
    /// The tool and the document's size, for context.
    pub tool: String,
    pub document: Option<(u32, u32)>,
}

/// The log line for `f` (`prev_start`: when the frame before started), or `None` when it isn't
/// slow or one was logged less than [`EVERY_MS`] ago (`last_logged`, updated).
pub(crate) fn report(f: &Frame, prev_start: f64, last_logged: &mut f64, spans: &std::collections::BTreeMap<&'static str, f64>) -> Option<String> {
    let gap = if prev_start > 0.0 { f.start - prev_start } else { 0.0 };
    let slow_build = f.build >= SLOW_BUILD_MS;
    let slow_gap = f.held && f.moved && gap >= SLOW_GAP_MS;
    if !(slow_build || slow_gap) || !f.build.is_finite() || f.start - *last_logged < EVERY_MS {
        return None;
    }
    *last_logged = f.start;
    // The biggest spans first: they are what to look at.
    let mut parts: Vec<(&str, f64)> = spans.iter().map(|(k, v)| (*k, *v)).filter(|(_, v)| *v >= 1.0).collect();
    parts.sort_by(|a, b| b.1.total_cmp(&a.1));
    let spans = parts.iter().take(8).map(|(k, v)| format!("{k} {v:.0}")).collect::<Vec<_>>().join(", ");
    let cpu = f.cpu_prev.map_or_else(|| "?".to_string(), |c| format!("{c:.0}"));
    let doc = f.document.map_or_else(|| "no document".to_string(), |(w, h)| format!("{w}x{h}"));
    Some(format!(
        "slow frame: {:.0} ms since the last frame{}, UI build {:.0} ms (canvas {} {:.0} ms; spans: {}), previous frame's CPU {cpu} ms; tool {}, {doc}",
        gap,
        if f.held { " with a button held" } else { "" },
        f.build,
        if f.refresh_kind.is_empty() { "refresh" } else { f.refresh_kind },
        f.refresh,
        if spans.is_empty() { "none over 1 ms" } else { &spans },
        f.tool,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans() -> std::collections::BTreeMap<&'static str, f64> {
        [("ui: dock", 3.0), ("layers", 412.5), ("ui: document", 20.0), ("color", 0.2)].into_iter().collect()
    }

    /// A slow build and a long gap during a stroke are logged with the biggest spans first; quick
    /// frames, an idle gap (nothing held) and a second slow frame within 2 s are not.
    #[test]
    fn slow_frames_are_logged_with_where_the_time_went() {
        let mut last = -1e9;
        let quick = Frame { start: 10_000.0, build: 12.0, held: true, ..Default::default() };
        assert_eq!(report(&quick, 9_990.0, &mut last, &spans()), None);
        let idle = Frame { start: 20_000.0, build: 5.0, held: false, ..Default::default() };
        assert_eq!(report(&idle, 10_000.0, &mut last, &spans()), None, "an idle app draws no frames");
        let slow = Frame {
            start: 30_000.0,
            build: 460.0,
            refresh: 30.0,
            refresh_kind: "gpu tiles",
            cpu_prev: Some(8.0),
            held: true,
            moved: true,
            tool: "Brush".into(),
            document: Some((5217, 4200)),
        };
        let line = report(&slow, 29_990.0, &mut last, &spans()).unwrap();
        assert!(line.contains("UI build 460 ms") && line.contains("canvas gpu tiles 30 ms"), "{line}");
        assert!(line.find("layers 412").unwrap() < line.find("ui: document 20").unwrap(), "biggest first: {line}");
        assert!(!line.contains("color"), "spans under 1 ms are left out: {line}");
        assert!(line.contains("tool Brush, 5217x4200"), "{line}");
        let again = Frame { start: 31_000.0, ..slow.clone() };
        assert_eq!(report(&again, 30_500.0, &mut last, &spans()), None, "at most one line every 2 s");
        let still = Frame { start: 40_000.0, build: 6.0, held: true, moved: false, ..Default::default() };
        assert_eq!(report(&still, 39_000.0, &mut last, &Default::default()), None, "a button held still draws no frames");
        let gap = Frame { moved: true, ..still };
        let line = report(&gap, 39_000.0, &mut last, &Default::default()).unwrap();
        assert!(line.starts_with("slow frame: 1000 ms since the last frame with a button held"), "{line}");
        let nan = Frame { start: 50_000.0, build: f64::NAN, held: true, ..Default::default() };
        assert_eq!(report(&nan, 49_990.0, &mut last, &spans()), None);
    }
}
