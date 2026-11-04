use super::*;

const MB: u64 = 1 << 20;

fn mem(budget: u64, usage: u64) -> Memory {
    Memory { budget: budget * MB, usage: usage * MB }
}

/// The user's case: 1.7 GB in use against a 1.2 GB budget, with 3 GB of layer pages allowed.
/// The pages give up the overshoot and the headroom; staying over caps them further, never below
/// the floor; a bigger budget that still can't take what was wanted keeps the cap; one that can
/// recovers.
#[test]
fn over_budget_caps_the_pages_and_room_again_recovers() {
    let pages = 3072 * MB;
    let Step::Short(s) = assess(None, mem(1200, 1700), pages) else { panic!("over budget") };
    assert_eq!(s, Short { wanted: 1700 * MB, cap: pages - 500 * MB - HEADROOM });
    // Still over (other programs grew): a lower cap, still wanting 1.7 GB.
    let Step::Short(t) = assess(Some(s), mem(1000, 1500), pages) else { panic!("still over") };
    assert_eq!(t, Short { wanted: 1700 * MB, cap: s.cap - 500 * MB - HEADROOM });
    // Under the budget but without room for 1.7 GB: keep the cap.
    assert_eq!(assess(Some(t), mem(1800, 1100), pages), Step::Keep);
    // Room for what it wanted, and the headroom: back.
    assert_eq!(assess(Some(t), mem(1700 + 256, 1100), pages), Step::Recover);
    // Within the budget and not short: nothing to do.
    assert_eq!(assess(None, mem(4000, 1700), pages), Step::Keep);
    // A tiny budget never caps the pages below the floor, and a cap at the floor stays.
    let Step::Short(f) = assess(None, mem(100, 2000), 1000 * MB) else { panic!() };
    assert_eq!(f.cap, MIN_GPU_BUDGET);
    assert_eq!(assess(Some(f), mem(100, 2000), 1000 * MB), Step::Keep);
}

/// Over budget: one notice, the cap in force for `gpu_budget`; a second reading over budget posts
/// no second notice; recovering lifts the cap, asks for the budget again and says so.
#[test]
fn going_short_warns_once_and_recovering_reloads() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let notices = |app: &PhotocraftApp| app.ui.notices.len();
    let before = notices(&app);
    let s = Short { wanted: 1700 * MB, cap: 2000 * MB };
    apply(&mut app, Step::Short(s), mem(1200, 1700));
    assert_eq!(app.gpu_memory.cap(), Some(2000 * MB));
    assert_eq!(notices(&app), before + 1, "the user is told");
    apply(&mut app, Step::Short(Short { cap: 1500 * MB, ..s }), mem(1100, 1600));
    assert_eq!(app.gpu_memory.cap(), Some(1500 * MB));
    assert_eq!(notices(&app), before + 1, "once per shortage");
    apply(&mut app, Step::Recover, mem(4000, 1200));
    assert_eq!(app.gpu_memory.cap(), None);
    assert_eq!(app.perf.gpu_budget_allowance, (u64::MAX, u64::MAX), "the budget is sized again");
    assert!(!app.ui.status_error && app.ui.status.contains("reloaded"), "{}", app.ui.status);
}

/// Without a GPU canvas or a platform reading (the default services), the watch does nothing.
#[test]
fn without_a_reading_nothing_changes() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.services.video_memory = Some(Box::new(|_, _| Some((100 * MB, 4000 * MB))));
    let ctx = egui::Context::default();
    watch(&mut app, &ctx);
    assert_eq!(app.gpu_memory.cap(), None, "no GPU canvas to read for");
}
