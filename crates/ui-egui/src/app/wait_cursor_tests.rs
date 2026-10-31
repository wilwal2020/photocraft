use super::*;
use std::sync::{Arc, Mutex};

/// A command that held the window the last time it ran shows the busy cursor around its next
/// run (and puts the cursor back after); a quick one doesn't, nor does one run inside another.
#[test]
fn a_command_that_was_slow_shows_the_wait_cursor_next_time() {
    let calls: Arc<Mutex<Vec<bool>>> = Arc::default();
    let seen = calls.clone();
    let services = Services { wait_cursor: Some(Box::new(move |on| seen.lock().unwrap().push(on))), ..Default::default() };
    let mut app = PhotocraftApp::new(Session::new(), services);
    app.session.execute("file.new", serde_json::json!({"width": 64, "height": 64})).unwrap();
    app.sync_views();
    app.run("select.all", serde_json::json!({})).unwrap();
    assert!(calls.lock().unwrap().is_empty(), "quick: no busy cursor");
    assert!(app.perf.command_cost.get("select.all").is_some_and(|ms| *ms < 100.0));
    // As if Select All had taken half a second.
    app.perf.command_cost.insert("select.all".into(), 500.0);
    app.run("select.all", serde_json::json!({})).unwrap();
    assert_eq!(*calls.lock().unwrap(), [true, false]);
    assert!(!app.perf.waiting);
    // Quick this time: no busy cursor the next time.
    app.run("select.all", serde_json::json!({})).unwrap();
    assert_eq!(calls.lock().unwrap().len(), 2);
    // Inside a command that shows it already: left to that one.
    app.perf.command_cost.insert("select.all".into(), 500.0);
    app.perf.waiting = true;
    app.run("select.all", serde_json::json!({})).unwrap();
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert!(app.perf.waiting, "the outer command puts the cursor back");
}
