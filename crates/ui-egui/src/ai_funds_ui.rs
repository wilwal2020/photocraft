//! The title bar's generative AI funds button (user request), next to the timelapse buttons: what
//! is left of the user's Google AI Studio balance for Nano Banana 2.1, and what each generation
//! cost, in Norwegian kroner.
//!
//! Google gives an API key no way to read the balance, and bills in USD. So the box keeps a
//! ledger ([`Ledger`], saved in the preferences' `aiFunds`): the balance the user typed from AI
//! Studio's billing page, less what PhotoCraft worked out each answer cost since (its token usage
//! at Google's list prices, `photocraft_ml::gemini::answer_cost`), converted at Norges Bank's
//! latest USD → NOK rate. The rate is fetched at most once a day, when the box opens or a
//! generation was billed; offline the last one fetched is used, and before any was fetched costs
//! show in $. When a generation ends, the status bar says what it cost.

use std::sync::mpsc;

use egui::{RichText, Sense, Ui, vec2};
use photocraft_engine::generative::nano_banana::{self, Rate};
use photocraft_engine::jobs::{JobEvent, JobOutcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// Where the user sees and tops up the balance.
pub const BILLING_URL: &str = "https://aistudio.google.com/billing";

/// The button's width and the space kept between it and the timelapse buttons.
pub const BUTTON: f32 = 28.0;
pub const GAP: f32 = 10.0;

/// What the box keeps across restarts (`Preferences::ai_funds`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Ledger {
    /// The AI Studio balance the user typed, kr; `None` until they do.
    pub balance_kr: Option<f64>,
    /// The day (since 1970-01-01, UTC) it was typed.
    pub balance_day: Option<i64>,
    /// What Google billed since, USD, and for how many generations.
    pub spent_since_usd: f64,
    pub generations_since: u64,
    /// What Google billed in all, USD, and for how many generations.
    pub total_usd: f64,
    pub generations: u64,
    /// The last generation's cost, USD.
    pub last_usd: Option<f64>,
    /// Norges Bank's last rate (kr per $), the day it is for, and the day it was fetched.
    pub rate: Option<f64>,
    pub rate_date: String,
    pub rate_day: Option<i64>,
}

impl Ledger {
    /// The ledger saved in the preferences (an empty one when there is none or it doesn't read).
    pub fn of(app: &PhotocraftApp) -> Self {
        serde_json::from_value(app.session.prefs().ai_funds.clone()).unwrap_or_default()
    }

    /// Save it in the preferences when it changed.
    pub fn save(&self, app: &mut PhotocraftApp) {
        if Self::of(app) != *self {
            let v = serde_json::to_value(self).unwrap_or(Value::Null);
            app.session.prefs.edit(|p| p.ai_funds = v);
        }
    }

    /// `usd` in kr at the rate, when there is one.
    pub fn kr(&self, usd: f64) -> Option<f64> {
        self.rate.filter(|r| r.is_finite() && *r > 0.0).map(|r| usd * r)
    }

    /// What is left of the typed balance after what was spent since, kr (`None` without a
    /// balance or a rate).
    pub fn left_kr(&self) -> Option<f64> {
        Some(self.balance_kr? - self.kr(self.spent_since_usd)?)
    }

    /// `usd` as the box and the status bar show it: kr when there is a rate, else $.
    pub fn money(&self, usd: f64) -> String {
        match self.kr(usd) {
            Some(kr) => kr_text(kr),
            None => format!("${}", decimal(usd, 3)),
        }
    }

    /// Count `usd` Google billed (an answer).
    fn spend(&mut self, usd: f64) {
        self.total_usd += usd;
        if self.balance_kr.is_some() {
            self.spent_since_usd += usd;
        }
    }

    /// Count a generation that cost `usd`.
    fn generation(&mut self, usd: f64) {
        self.generations += 1;
        self.last_usd = Some(usd);
        if self.balance_kr.is_some() {
            self.generations_since += 1;
        }
    }

    /// Start over from the balance the user typed today.
    fn set_balance(&mut self, kr: Option<f64>, today: i64) {
        self.balance_kr = kr;
        self.balance_day = kr.map(|_| today);
        self.spent_since_usd = 0.0;
        self.generations_since = 0;
    }

    /// Whether today's rate still has to be fetched.
    fn needs_rate(&self, today: i64) -> bool {
        self.rate.is_none() || self.rate_day != Some(today)
    }
}

/// The box's runtime state.
#[derive(Default)]
pub struct AiFundsUi {
    /// [`nano_banana::billed`] as last taken into the ledger.
    seen: f64,
    /// Billed since a finished generation last reported it.
    unreported: f64,
    /// The balance field while the box is open.
    pub balance_text: String,
    /// A rate fetch on its way, and the day one was last tried (so a failure isn't retried every
    /// frame).
    fetch: Option<mpsc::Receiver<Result<Rate, String>>>,
    tried_day: Option<i64>,
    /// Why the last fetch failed.
    pub rate_error: Option<String>,
}

/// Days since 1970-01-01 (UTC).
pub fn today() -> i64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| (d.as_secs() / 86_400) as i64)
}

/// Day `days` since 1970-01-01 as `YYYY-MM-DD` (empty when out of chrono's range).
pub fn date_text(days: i64) -> String {
    days.checked_mul(86_400).and_then(|secs| chrono::DateTime::from_timestamp(secs, 0)).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// `v` with `places` decimals and the system's decimal separator.
fn decimal(v: f64, places: usize) -> String {
    let s = format!("{v:.places$}");
    match crate::i18n::decimal_separator() {
        Some(',') => s.replace('.', ","),
        _ => s,
    }
}

/// An amount in kroner (`12.34 kr`).
pub fn kr_text(kr: f64) -> String {
    format!("{} kr", decimal(kr, 2))
}

/// A typed amount: digits with `.` or `,` as the decimal separator, an optional `kr`; `None` for
/// anything else or a negative amount.
pub fn parse_kr(text: &str) -> Option<f64> {
    let t = text.trim().trim_end_matches("kr").trim().replace([' ', '\u{a0}'], "").replace(',', ".");
    t.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

/// Take what Google billed since the last look (`billed`, [`nano_banana::billed`]) into the
/// ledger's spend. Returns what was new, USD.
pub fn absorb(app: &mut PhotocraftApp, billed: f64) -> f64 {
    let new = billed - app.ai_funds.seen;
    if !(new.is_finite() && new > 1e-12) {
        return 0.0;
    }
    app.ai_funds.seen = billed;
    app.ai_funds.unreported += new;
    let mut ledger = Ledger::of(app);
    ledger.spend(new);
    ledger.save(app);
    new
}

/// What [`after_job`] needs of a finished job.
pub struct Ended {
    pub label: String,
    /// It finished (not failed or cancelled).
    pub done: bool,
    /// Its result says Google drew it (`"device": "Google"`).
    pub google: bool,
}

impl Ended {
    pub fn of(e: &JobEvent) -> Self {
        let google =
            matches!(&e.outcome, JobOutcome::Done(v) if v.get("device").and_then(Value::as_str) == Some(photocraft_engine::generative::Device::Google.name()));
        Self { label: e.label.clone(), done: matches!(e.outcome, JobOutcome::Done(_)), google }
    }
}

/// After a finished job was handled: when Google billed for it, count the generation and say in
/// the status bar what it cost. The cost is a job's when Google drew it, or when it failed or was
/// cancelled with no other job running (Google bills an answer the removal couldn't use); any
/// other job leaves it to the generation that is still running.
pub fn after_job(app: &mut PhotocraftApp, e: &Ended, billed: f64) {
    absorb(app, billed);
    let usd = app.ai_funds.unreported;
    if usd <= 0.0 || !(e.google || (!e.done && !app.session.jobs().iter().any(|j| j.state == "running"))) {
        return;
    }
    app.ai_funds.unreported = 0.0;
    let mut ledger = Ledger::of(app);
    ledger.generation(usd);
    let cost = ledger.money(usd);
    ledger.save(app);
    let what = crate::i18n::fmt(tl!("{model} cost {cost}"), &[("model", nano_banana::NAME), ("cost", &cost)]);
    app.ui.status = if e.done {
        format!("{} · {what}", e.label)
    } else if app.ui.status.is_empty() {
        what
    } else {
        // The failure stays in the status bar, with what Google billed for it anyway.
        format!("{} ({what})", app.ui.status)
    };
    want_rate(app);
}

/// Per frame: take in what was billed (an answer that came after a cancel too: its cost is shown
/// with the next generation's) and a finished rate fetch.
pub fn tick(app: &mut PhotocraftApp) {
    absorb(app, nano_banana::billed());
    let Some(rx) = app.ai_funds.fetch.as_ref() else { return };
    let answer = match rx.try_recv() {
        Ok(answer) => answer,
        Err(mpsc::TryRecvError::Empty) => return,
        Err(mpsc::TryRecvError::Disconnected) => Err(tl!("the rate fetch stopped").to_string()),
    };
    app.ai_funds.fetch = None;
    took_rate(app, answer, today());
}

/// A rate fetch's answer, on day `today`.
pub fn took_rate(app: &mut PhotocraftApp, answer: Result<Rate, String>, today: i64) {
    match answer {
        Ok(rate) => {
            let mut ledger = Ledger::of(app);
            ledger.rate = Some(rate.nok_per_usd);
            ledger.rate_date = rate.date;
            ledger.rate_day = Some(today);
            ledger.save(app);
            app.ai_funds.rate_error = None;
        }
        Err(e) => {
            log::info!("[ai funds] Norges Bank's rate: {e}");
            app.ai_funds.rate_error = Some(e);
        }
    }
}

/// Fetch today's rate in the background unless the ledger has it, one is on its way, or one was
/// already tried today.
fn want_rate(app: &mut PhotocraftApp) {
    let day = today();
    if app.ai_funds.fetch.is_some() || app.ai_funds.tried_day == Some(day) || !Ledger::of(app).needs_rate(day) {
        return;
    }
    app.ai_funds.tried_day = Some(day);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("norges-bank-rate".into()).spawn(move || {
            let _ = tx.send(nano_banana::usd_nok());
        });
        match spawned {
            Ok(_) => app.ai_funds.fetch = Some(rx),
            Err(e) => app.ai_funds.rate_error = Some(e.to_string()),
        }
    }
}

/// The title-bar button and its box.
pub fn title_button(app: &mut PhotocraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let ledger = Ledger::of(app);
    let tip = match ledger.left_kr() {
        Some(left) => crate::i18n::fmt(tl!("Generative AI funds: about {left} left"), &[("left", &kr_text(left))]),
        None => tl!("Generative AI funds").to_string(),
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(BUTTON, BUTTON), Sense::click());
    let resp = resp.on_hover_text(tip);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Generative AI funds")));
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let color = crate::icons::button_chrome(ui, rect, open, resp.hovered());
    let low = ledger.left_kr().is_some_and(|left| left < 10.0);
    let color = if low { t.warning } else { color };
    let galley = ui.painter().layout_no_wrap("kr".into(), egui::FontId::proportional(11.5), color);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    if resp.clicked() && !open {
        app.ai_funds.balance_text = ledger.balance_kr.map(|b| decimal(b, 2)).unwrap_or_default();
        want_rate(app);
    }
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| funds_box(app, ui, &t));
}

/// The box: what is left, what was spent, the rate, and the balance field.
fn funds_box(app: &mut PhotocraftApp, ui: &mut Ui, t: &Tokens) {
    ui.set_width(260.0);
    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
    let mut ledger = Ledger::of(app);
    let dim = |ui: &mut Ui, text: &str| {
        ui.add(egui::Label::new(RichText::new(text).size(10.0).color(t.text_dim)).wrap());
    };
    ui.label(RichText::new(tl!("Generative AI funds").to_uppercase()).size(9.5).strong().color(t.text_faint));
    dim(ui, &crate::i18n::fmt(tl!("{model} (Google), billed to your Google account."), &[("model", nano_banana::NAME)]));
    if nano_banana::key_source().is_none() {
        dim(ui, tl!("No Google API key is set: add one in the Remove tool's options."));
    }
    match (ledger.balance_kr, ledger.left_kr()) {
        (Some(balance), left) => {
            let left = left
                .map_or_else(|| tl!("Unknown until a rate is fetched").to_string(), |l| crate::i18n::fmt(tl!("About {left} left"), &[("left", &kr_text(l))]));
            ui.label(RichText::new(left).size(15.0).strong().color(t.text));
            let since = ledger.balance_day.map(date_text).unwrap_or_default();
            dim(
                ui,
                &crate::i18n::fmt(
                    &crate::i18n::trn(
                        crate::i18n::current(),
                        ledger.generations_since,
                        "{balance} on {date}, less {spent} spent since ({n} generation).",
                        "{balance} on {date}, less {spent} spent since ({n} generations).",
                    ),
                    &[("balance", &kr_text(balance)), ("date", &since), ("spent", &ledger.money(ledger.spent_since_usd))],
                ),
            );
        }
        (None, _) => dim(
            ui,
            tl!(
                "Google doesn't let PhotoCraft read your balance: type the one AI Studio's billing page shows, and PhotoCraft subtracts what it spends from it."
            ),
        ),
    }
    let mut set = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Balance")).size(10.5));
        let field = ui.add(egui::TextEdit::singleline(&mut app.ai_funds.balance_text).desired_width(90.0).hint_text("kr"));
        let typed = parse_kr(&app.ai_funds.balance_text);
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.add_enabled(typed.is_some(), egui::Button::new(tl!("Set balance"))).clicked() || enter) && typed.is_some() {
            set = Some(typed);
        }
        if ledger.balance_kr.is_some() && ui.button(tl!("Clear")).clicked() {
            set = Some(None);
        }
    });
    if let Some(kr) = set {
        ledger.set_balance(kr, today());
        ledger.save(app);
        app.ai_funds.balance_text = kr.map(|b| decimal(b, 2)).unwrap_or_default();
    }
    ui.separator();
    if let Some(last) = ledger.last_usd {
        dim(ui, &crate::i18n::fmt(tl!("Last generation: {cost}"), &[("cost", &ledger.money(last))]));
    }
    let all = crate::i18n::trn(crate::i18n::current(), ledger.generations, "Spent in all: {spent} ({n} generation)", "Spent in all: {spent} ({n} generations)");
    dim(ui, &crate::i18n::fmt(&all, &[("spent", &ledger.money(ledger.total_usd))]));
    let rate = match (ledger.rate, app.ai_funds.fetch.is_some()) {
        (Some(r), _) => crate::i18n::fmt(tl!("{rate} kr per $ (Norges Bank, {date})"), &[("rate", &decimal(r, 4)), ("date", &ledger.rate_date)]),
        (None, true) => tl!("Fetching today's rate from Norges Bank…").to_string(),
        (None, false) => tl!("No exchange rate yet, so costs show in $.").to_string(),
    };
    dim(ui, &rate);
    if let Some(e) = app.ai_funds.rate_error.clone() {
        dim(ui, &crate::i18n::fmt(tl!("Couldn't fetch today's rate: {error}"), &[("error", &e)]));
    }
    dim(ui, tl!("Costs are worked out from each answer's token usage at Google's list prices (USD); Google's own figure can differ a little."));
    if ui.button(tl!("Open AI Studio billing")).clicked() {
        let opened = match app.services.open_url.as_ref() {
            Some(open) => open(BILLING_URL),
            None => Err(tl!("Opening links isn't available here").into()),
        };
        if let Err(e) = opened {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
}

#[cfg(test)]
mod tests;
