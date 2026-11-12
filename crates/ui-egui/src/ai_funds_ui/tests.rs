use std::sync::{Arc, Mutex};

use egui_kittest::kittest::Queryable;
use serde_json::json;

use super::*;

/// A ledger with Norges Bank's rate fetched today (so nothing goes to the network) and,
/// optionally, a typed balance.
fn ledger(rate: f64, balance: Option<f64>) -> Ledger {
    Ledger {
        rate: Some(rate),
        rate_date: "2026-11-06".into(),
        rate_day: Some(today()),
        balance_kr: balance,
        balance_day: balance.map(|_| today()),
        ..Ledger::default()
    }
}

fn app_with(l: &Ledger) -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    l.save(&mut app);
    app
}

fn ended(label: &str, done: bool, google: bool) -> Ended {
    Ended { label: label.into(), done, google }
}

#[test]
fn amounts_dates_and_typed_balances() {
    assert_eq!(kr_text(12.345), "12.35 kr");
    assert_eq!(date_text(0), "1970-01-01");
    assert_eq!(date_text(11_016), "2000-02-29");
    assert_eq!(date_text(20_764), "2026-11-07");
    assert_eq!(date_text(i64::MAX), "", "a corrupt saved day doesn't crash");
    assert_eq!(parse_kr("250"), Some(250.0));
    assert_eq!(parse_kr(" 1 250,50 kr "), Some(1250.5));
    assert_eq!(parse_kr("99.9kr"), Some(99.9));
    assert_eq!(parse_kr("-5"), None);
    assert_eq!(parse_kr("lots"), None);
    assert_eq!(parse_kr(""), None);
}

/// Costs show in kr at the rate, and in $ before any rate was fetched (the user's choice: no
/// built-in rate).
#[test]
fn costs_show_in_kroner_at_the_rate_or_else_in_dollars() {
    let l = ledger(10.0, Some(100.0));
    assert_eq!(l.money(0.0504), "0.50 kr");
    let none = Ledger::default();
    assert_eq!(none.money(0.0336), "$0.034");
    assert_eq!(none.left_kr(), None, "no rate: what is left isn't known");
    let mut spent = l.clone();
    spent.spend(0.5);
    assert_eq!(spent.left_kr(), Some(95.0));
    // Spend before a balance was typed counts in all, not against it.
    let mut before = ledger(10.0, None);
    before.spend(0.5);
    before.set_balance(Some(80.0), today());
    assert_eq!((before.total_usd, before.left_kr()), (0.5, Some(80.0)));
}

/// User request: when a generation completes, the status bar says what it cost, in kr; the box's
/// balance goes down by it.
#[test]
fn a_finished_generation_shows_its_cost_in_kroner() {
    let mut app = app_with(&ledger(10.0, Some(100.0)));
    app.ui.status = "Remove".into();
    after_job(&mut app, &ended("Remove", true, true), 0.0504);
    assert_eq!(app.ui.status, "Remove · Nano Banana 2.1 cost 0.50 kr");
    let l = Ledger::of(&app);
    assert_eq!((l.generations, l.generations_since, l.last_usd), (1, 1, Some(0.0504)));
    assert!((l.left_kr().unwrap() - 99.496).abs() < 1e-9);
    // A job Google had nothing to do with says nothing about cost.
    after_job(&mut app, &ended("Gaussian Blur", true, false), 0.0504);
    assert_eq!(app.ui.status, "Remove · Nano Banana 2.1 cost 0.50 kr");
    assert_eq!(Ledger::of(&app).generations, 1);
}

/// An answer the removal couldn't use is billed all the same: the failure says what it cost.
#[test]
fn a_failed_generation_still_says_what_google_billed() {
    let mut app = app_with(&ledger(10.0, None));
    app.ui.status = "Remove: Google's picture doesn't line up with the page".into();
    after_job(&mut app, &ended("Remove", false, false), 0.0336);
    assert_eq!(app.ui.status, "Remove: Google's picture doesn't line up with the page (Nano Banana 2.1 cost 0.34 kr)");
    let l = Ledger::of(&app);
    assert_eq!((l.generations, l.generations_since), (1, 0), "no balance typed: counted in all only");
    assert!((l.total_usd - 0.0336).abs() < 1e-12);
}

/// Each answer is spent as it arrives (in [`tick`]), and a cancelled request's late answer is
/// shown with the next generation's cost.
#[test]
fn answers_count_as_they_arrive_and_late_ones_join_the_next_generation() {
    let mut app = app_with(&ledger(10.0, Some(50.0)));
    assert!((absorb(&mut app, 0.05) - 0.05).abs() < 1e-12);
    assert_eq!(absorb(&mut app, 0.05), 0.0, "nothing new");
    assert!((Ledger::of(&app).left_kr().unwrap() - 49.5).abs() < 1e-9);
    after_job(&mut app, &ended("Generative Fill", true, true), 0.10);
    assert_eq!(app.ui.status, "Generative Fill · Nano Banana 2.1 cost 1.00 kr");
    assert!((Ledger::of(&app).left_kr().unwrap() - 49.0).abs() < 1e-9);
}

/// Norges Bank's rate is kept with the day it was fetched; when a fetch fails (offline) the last
/// one stays in use.
#[test]
fn a_fetched_rate_is_kept_and_a_failed_fetch_keeps_the_last() {
    let mut app = app_with(&Ledger::default());
    took_rate(&mut app, Ok(Rate { nok_per_usd: 9.4277, date: "2026-11-06".into() }), 20_764);
    let l = Ledger::of(&app);
    assert_eq!((l.rate, l.rate_date.as_str(), l.rate_day), (Some(9.4277), "2026-11-06", Some(20_764)));
    assert!(!l.needs_rate(20_764) && l.needs_rate(20_765), "fetched again the next day");
    took_rate(&mut app, Err("couldn't reach Norges Bank".into()), 20_765);
    assert_eq!(Ledger::of(&app).rate, Some(9.4277), "offline: the last rate");
    assert_eq!(app.ai_funds.rate_error.as_deref(), Some("couldn't reach Norges Bank"));
}

/// The ledger is saved with the preferences and comes back after a restart.
#[test]
fn the_ledger_survives_a_restart() {
    let store: Arc<Mutex<Option<String>>> = Arc::default();
    let services = |store: &Arc<Mutex<Option<String>>>| {
        let (a, b) = (store.clone(), store.clone());
        crate::Services {
            load_prefs: Some(Box::new(move || a.lock().unwrap().clone())),
            save_prefs: Some(Box::new(move |s: &str| {
                *b.lock().unwrap() = Some(s.to_string());
                Ok(())
            })),
            ..Default::default()
        }
    };
    let ctx = egui::Context::default();
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services(&store));
    let mut l = ledger(9.5, Some(200.0));
    l.spend(1.0);
    l.save(&mut app);
    crate::prefs_ui::tick(&mut app, &ctx);
    let saved: Value = serde_json::from_str(store.lock().unwrap().as_deref().unwrap()).unwrap();
    assert_eq!(saved["aiFunds"]["balanceKr"], json!(200.0));
    let app2 = PhotocraftApp::new(photocraft_engine::Session::new(), services(&store));
    assert_eq!(Ledger::of(&app2), l);
    assert_eq!(Ledger::of(&app2).left_kr(), Some(190.5));
}

fn harness(l: Ledger) -> egui_kittest::Harness<'static, PhotocraftApp> {
    egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app_with(&l)
    })
}

/// User request: a button in the title bar, a little apart from the timelapse buttons, shows how
/// much is left for generative AI; the box takes the balance AI Studio shows.
#[test]
fn the_title_bar_button_sits_beside_the_timelapse_buttons_and_shows_whats_left() {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 32, "height": 24})).unwrap();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut app = PhotocraftApp::new(s, crate::Services::default());
        ledger(10.0, None).save(&mut app);
        app
    });
    h.run_steps(4);
    let funds = h.get_by_label("Generative AI funds").rect();
    let record = h.get_by_label("Timelapse recording").rect();
    let export = h.get_by_label("Export timelapse").rect();
    // The bar runs from its far end: the timelapse buttons, then the funds button past them.
    let nearest = record.left().min(export.left()).min(record.right()).min(export.right());
    let gap = if funds.center().x < record.center().x { record.left().min(export.left()) - funds.right() } else { funds.left() - nearest };
    assert!(gap >= GAP, "a little space from the timelapse buttons: {gap}");
    assert!((funds.center().y - record.center().y).abs() < 1.0, "on the same row");

    h.get_by_label("Generative AI funds").click();
    h.run_steps(3);
    assert!(
        h.query_by_label(
            "Google doesn't let PhotoCraft read your balance: type the one AI Studio's billing page shows, and PhotoCraft subtracts what it spends from it."
        )
        .is_some(),
        "says why it must be typed"
    );
    h.state_mut().ai_funds.balance_text = "250,50".into();
    h.run_steps(2);
    h.get_by_label("Set balance").click();
    h.run_steps(3);
    let l = Ledger::of(h.state());
    assert_eq!((l.balance_kr, l.balance_day), (Some(250.5), Some(today())));
    assert!(h.query_by_label("About 250.50 kr left").is_some());
    assert!(h.query_by_label("10.0000 kr per $ (Norges Bank, 2026-11-06)").is_some());
    assert!(h.query_by_label("Open AI Studio billing").is_some());
    if let Ok(path) = std::env::var("PHOTOCRAFT_AI_FUNDS_SCREENSHOT") {
        h.render().unwrap().save(path).unwrap();
    }
}

/// Before a balance is typed the box still says what was spent, in $ until a rate comes.
#[test]
fn without_a_rate_the_box_shows_dollars() {
    let l = Ledger { total_usd: 0.0840, generations: 2, last_usd: Some(0.0504), rate_day: Some(today()), ..Ledger::default() };
    let mut h = harness(l);
    // No fetch: one was "tried" today.
    h.state_mut().ai_funds.tried_day = Some(today());
    h.run_steps(4);
    h.get_by_label("Generative AI funds").click();
    h.run_steps(3);
    assert!(h.query_by_label("Spent in all: $0.084 (2 generations)").is_some());
    assert!(h.query_by_label("Last generation: $0.050").is_some());
    assert!(h.query_by_label("No exchange rate yet, so costs show in $.").is_some());
    assert!(h.state().ai_funds.fetch.is_none(), "nothing fetched in a test");
}
