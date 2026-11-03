//! The pointer moves the system merged while a frame was busy, put back into a stroke (user
//! report: with a heavy brush a circle came out a polygon, where Photoshop's stayed round).
//!
//! Windows keeps one `WM_MOUSEMOVE` in a busy window's queue, at the latest position, so a slow
//! frame hands egui a single move however far the pen went. It does remember the moves
//! ([`crate::Services::mouse_history`], `GetMouseMovePointsEx`): those after the last one the
//! stroke took, up to the latest egui reported, are this frame's stroke positions. The
//! history's screen pixels are mapped through the window's place and its points per pixel, and
//! that mapping is checked against egui's own position: the moves are used only where the
//! newest egui move is one of them and the last one taken is still in the history, so a pen
//! that sends pointer messages instead (no mouse history), another display's scaling or a gap
//! longer than the history leaves egui's moves as they are.

use egui::Pos2;

/// A move from the system history: screen pixels and its time (ms).
pub type ScreenMove = (i32, i32, u32);

/// How far (egui points) a history move may be from egui's position and still be that move.
const SAME: f32 = 1.5;

fn last_id() -> egui::Id {
    egui::Id::unique("photocraft-merged-moves-last")
}

/// `m` in the window's egui points: `origin` is the window's client area on the screen (points)
/// and `ppp` the points per pixel.
fn to_points(m: ScreenMove, ppp: f32, origin: Pos2) -> Pos2 {
    Pos2::new(m.0 as f32 / ppp - origin.x, m.1 as f32 / ppp - origin.y)
}

/// The moves of `history` (newest first) after `last`, up to the newest one at `anchor` (egui's
/// latest position), oldest first and in egui points, and the move to continue from next frame.
/// `None` for the moves when they can't be told for sure (see the module docs).
pub(super) fn recover(history: &[ScreenMove], last: Option<ScreenMove>, anchor: Pos2, ppp: f32, origin: Pos2) -> (Option<Vec<Pos2>>, Option<ScreenMove>) {
    if !(ppp.is_finite() && ppp > 0.0) {
        return (None, None);
    }
    let Some(at) = history.iter().position(|m| to_points(*m, ppp, origin).distance(anchor) <= SAME) else { return (None, None) };
    let newest = history.get(at).copied();
    let Some(since) = last.and_then(|l| history.iter().position(|m| *m == l)) else { return (None, newest) };
    if since <= at {
        return (None, newest);
    }
    let moves = history.get(at..since).unwrap_or_default().iter().rev().map(|m| to_points(*m, ppp, origin)).collect();
    (Some(moves), newest)
}

/// This frame's merged moves for a stroke under way (`continuing`: the press came before this
/// frame), from the history the platform keeps; `None` keeps egui's own moves. A press starts
/// over, so no move from before the stroke comes back. `anchor` is egui's latest pointer
/// position this frame (a move or the press).
pub(super) fn frame_moves(app: &crate::PhotocraftApp, ctx: &egui::Context, anchor: Option<Pos2>, continuing: bool) -> Option<Vec<Pos2>> {
    let history = app.services.mouse_history.as_ref()?;
    let id = last_id();
    let last = if continuing { ctx.data(|d| d.get_temp::<Option<ScreenMove>>(id)).flatten() } else { None };
    let Some(anchor) = anchor else {
        // A still pointer: nothing new, and where the stroke is stays.
        if !continuing {
            ctx.data_mut(|d| d.remove::<Option<ScreenMove>>(id));
        }
        return None;
    };
    let origin = ctx.input(|i| i.viewport().inner_rect.map(|r| r.min))?;
    let (moves, next) = recover(&history(), last, anchor, ctx.pixels_per_point(), origin);
    ctx.data_mut(|d| d.insert_temp(id, next));
    moves.filter(|m| !m.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_merged_moves_between_two_frames_come_back_in_order() {
        // 2 px per point, the window's client area at (100, 50) points on the screen.
        let (ppp, origin) = (2.0, Pos2::new(100.0, 50.0));
        let m = |x: i32, time: u32| (200 + 2 * x, 100, time);
        let history = [m(40, 5), m(30, 4), m(20, 3), m(10, 2), m(0, 1)];
        // Last frame took the move at x = 10; egui's latest is x = 30 (the move at 40 came after).
        let (moves, next) = recover(&history, Some(m(10, 2)), Pos2::new(30.0, 0.0), ppp, origin);
        assert_eq!(moves, Some(vec![Pos2::new(20.0, 0.0), Pos2::new(30.0, 0.0)]));
        assert_eq!(next, Some(m(30, 4)), "next frame goes on from egui's latest move");
        let (moves, next) = recover(&history, next, Pos2::new(40.0, 0.0), ppp, origin);
        assert_eq!((moves, next), (Some(vec![Pos2::new(40.0, 0.0)]), Some(m(40, 5))));
    }

    #[test]
    fn moves_that_cant_be_told_for_sure_are_left_to_egui() {
        let (ppp, origin) = (1.0, Pos2::ZERO);
        let history = [(30, 0, 3), (20, 0, 2), (10, 0, 1)];
        // egui's position isn't in the history: a pen's pointer messages, or another mapping.
        assert_eq!(recover(&history, Some((10, 0, 1)), Pos2::new(90.0, 9.0), ppp, origin), (None, None));
        // The stroke's last move fell out of the history (or a stroke just began).
        assert_eq!(recover(&history, Some((5, 0, 0)), Pos2::new(30.0, 0.0), ppp, origin), (None, Some((30, 0, 3))));
        assert_eq!(recover(&history, None, Pos2::new(30.0, 0.0), ppp, origin), (None, Some((30, 0, 3))));
        // Nothing newer than the last move.
        assert_eq!(recover(&history, Some((30, 0, 3)), Pos2::new(30.0, 0.0), ppp, origin), (None, Some((30, 0, 3))));
        assert_eq!(recover(&history, None, Pos2::ZERO, f32::NAN, origin), (None, None));
    }
}
