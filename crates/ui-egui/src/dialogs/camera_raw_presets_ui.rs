//! Camera Raw › Presets: the presets in PhotoCraft's own folder and in Photoshop's Camera Raw
//! folders (see `photocraft_engine::camera_raw_presets`), listed by group as Camera Raw's Presets
//! panel lists them. Pointing at one previews it, clicking applies it (one undo step), and its
//! Amount then mixes it with the settings it was applied over. Create Preset saves chosen groups
//! of the current settings into PhotoCraft's folder, as a file Photoshop's Camera Raw reads too.

use std::sync::Arc;

use photocraft_algo::camera_raw::CameraRaw;
use photocraft_engine::camera_raw_presets::{self as store, PresetFile, Source};
use photocraft_io::camera_raw_xmp::{self as xmp, Section};
use serde_json::{Value, json};

use super::{CameraRawDialog, remember_edit};
use crate::CameraRawPresetDirs;
use crate::theme::Tokens;
use crate::widgets;

/// The Presets panel's state.
#[derive(Default)]
pub(crate) struct Presets {
    pub(crate) list: Arc<Vec<PresetFile>>,
    pending: Option<std::sync::mpsc::Receiver<Vec<PresetFile>>>,
    /// The folders the list is from.
    scanned: Option<Vec<(String, Source)>>,
    pub(crate) search: String,
    /// Adobe's presets installed with Photoshop are listed too.
    pub(crate) show_adobe: bool,
    /// Camera Raw's favourites file ([`CameraRawPresetDirs::favorites`]) and the preset UUIDs it
    /// names, read with each new list.
    favorites_path: Option<String>,
    pub(crate) favorites: Arc<std::collections::HashSet<String>>,
    /// The presets starred in PhotoCraft ([`store::own_favorites`]): Camera Raw's favourites file
    /// names its presets by ids nothing else on disk carries, so it can't always be read.
    pub(crate) own_favorites: std::collections::HashSet<String>,
    own_dir: Option<String>,
    /// The preset applied last, while its Amount can still change.
    pub(crate) applied: Option<Applied>,
    create: Option<CreateForm>,
    renaming: Option<(String, String)>,
    /// The preset the pointer is on (its path), previewed.
    hovered: Option<String>,
}

/// A preset applied over `base`; `current` is what its Amount made of them last.
pub(crate) struct Applied {
    pub(crate) name: String,
    pub(crate) path: String,
    base: CameraRaw,
    target: CameraRaw,
    pub(crate) amount: f32,
    current: CameraRaw,
}

struct CreateForm {
    name: String,
    group: String,
    sections: Vec<Section>,
}

impl Default for CreateForm {
    fn default() -> Self {
        // Camera Raw's Create Preset leaves white balance and the lens settings out by default:
        // they belong to one photo more than to a look.
        let sections = Section::ALL.into_iter().filter(|s| !matches!(s, Section::WhiteBalance | Section::Optics)).collect();
        CreateForm { name: String::new(), group: String::new(), sections }
    }
}

/// The last list scanned and the folders it is from, kept for the whole session: a Camera Raw
/// window shows it at once instead of reading every preset again (user report: after a restart
/// the presets took a while to come). The app fills it at startup ([`prefetch`]).
static SCANNED: std::sync::Mutex<Option<Scanned>> = std::sync::Mutex::new(None);

/// A list of presets and the folders it is from.
type Scanned = (Vec<(String, Source)>, Arc<Vec<PresetFile>>);

fn cached(folders: &[(String, Source)]) -> Option<Arc<Vec<PresetFile>>> {
    let g = SCANNED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    g.as_ref().filter(|(f, _)| f.as_slice() == folders).map(|(_, l)| l.clone())
}

fn keep(folders: Vec<(String, Source)>, list: Arc<Vec<PresetFile>>) {
    *SCANNED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((folders, list));
}

/// Lists the presets in the background at startup, so the first Camera Raw window has them.
pub(crate) fn prefetch(dirs: &CameraRawPresetDirs) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let folders = folders(dirs, false);
        if folders.is_empty() || cached(&folders).is_some() {
            return;
        }
        let _ = std::thread::Builder::new().name("camera-raw-presets".into()).spawn(move || {
            let list = Arc::new(store::scan(&folders));
            keep(folders, list);
        });
    }
    #[cfg(target_arch = "wasm32")]
    let _ = dirs;
}

/// The key a preset is starred by in PhotoCraft: its UUID, else its path.
fn star_key(p: &PresetFile) -> &str {
    if p.uuid.is_empty() { &p.path } else { &p.uuid }
}

/// The folders to list for `dirs`: Adobe's when asked for, or when Camera Raw has favourites
/// (Adobe's presets among them show in Favorites with the Adobe section hidden).
pub(super) fn folders(dirs: &CameraRawPresetDirs, adobe: bool) -> Vec<(String, Source)> {
    let mut out: Vec<(String, Source)> = dirs.own.iter().map(|d| (d.clone(), Source::Own)).collect();
    out.extend(dirs.photoshop.iter().map(|d| (d.clone(), Source::Photoshop)));
    if adobe || dirs.favorites.is_some() {
        out.extend(dirs.adobe.iter().map(|d| (d.clone(), Source::Adobe)));
    }
    out
}

impl Presets {
    /// Starts listing `folders` on a worker unless the list is from them already (or a scan runs).
    pub(super) fn refresh(&mut self, folders: Vec<(String, Source)>, ctx: &egui::Context) {
        if self.pending.is_some() || self.scanned.as_ref() == Some(&folders) {
            return;
        }
        self.scanned = Some(folders.clone());
        // Listed already this session (or at startup): shown at once.
        if self.list.is_empty()
            && let Some(list) = cached(&folders)
        {
            self.set_list_arc(list);
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = ctx.clone();
            let spawned = std::thread::Builder::new().name("camera-raw-presets".into()).spawn(move || {
                let list = store::scan(&folders);
                keep(folders, Arc::new(list.clone()));
                if tx.send(list).is_ok() {
                    ctx.request_repaint();
                }
            });
            match spawned {
                Ok(_) => self.pending = Some(rx),
                Err(_) => self.scanned = None,
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = ctx;
            self.set_list(store::scan(&folders));
        }
    }

    /// A finished scan's list, with Camera Raw's favourites as they are now.
    fn set_list(&mut self, list: Vec<PresetFile>) {
        self.set_list_arc(Arc::new(list));
    }

    fn set_list_arc(&mut self, list: Arc<Vec<PresetFile>>) {
        self.list = list;
        self.favorites = Arc::new(self.favorites_path.as_deref().map(store::favorites).unwrap_or_default());
        self.own_favorites = self.own_dir.as_deref().map(store::own_favorites).unwrap_or_default();
    }

    /// Stars (or unstars) `p` in PhotoCraft's favourites, saved in PhotoCraft's presets folder.
    fn toggle_star(&mut self, p: &PresetFile) -> Result<(), String> {
        let dir = self.own_dir.clone().ok_or_else(|| tl!("Presets can't be saved here").to_string())?;
        let key = star_key(p).to_string();
        let mut stars = self.own_favorites.clone();
        if !stars.remove(&key) {
            stars.insert(key);
        }
        store::set_own_favorites(&dir, &stars)?;
        self.own_favorites = stars;
        Ok(())
    }

    /// Takes a finished scan's list.
    pub(super) fn poll(&mut self) {
        let Some(rx) = &self.pending else { return };
        match rx.try_recv() {
            Ok(list) => {
                self.set_list(list);
                self.pending = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
                self.scanned = None;
            }
        }
    }

    /// Lists `folders` on this thread (the control channel, whose reply describes the list).
    pub(super) fn scan_now(&mut self, folders: Vec<(String, Source)>) {
        if self.scanned.as_ref() == Some(&folders) && self.pending.is_none() {
            return;
        }
        self.pending = None;
        self.set_list(store::scan(&folders));
        self.scanned = Some(folders);
    }

    /// The folders changed (a preset saved, renamed or deleted): list them again.
    fn rescan(&mut self) {
        self.scanned = None;
    }

    pub(super) fn scanning(&self) -> bool {
        self.pending.is_some()
    }

    /// The preset named by `key`: its path, else its name.
    pub(super) fn find(&self, key: &str) -> Option<PresetFile> {
        self.list.iter().find(|p| p.path == key).or_else(|| self.list.iter().find(|p| p.name == key)).cloned()
    }

    pub(super) fn describe(&self, params: &CameraRaw) -> Value {
        let items: Vec<Value> = self.list.iter().take(2000).map(|p| json!({"name": p.name, "group": p.group, "source": p.source, "path": p.path})).collect();
        json!({
            "count": self.list.len(),
            "items": items,
            "scanning": self.scanning(),
            "showAdobe": self.show_adobe,
            "applied": self.applied.as_ref().map(|a| json!({"name": a.name, "path": a.path, "amount": a.amount, "active": a.current == *params})),
        })
    }
}

/// `next` as the dialog's settings, one undo step.
fn set_params(d: &mut CameraRawDialog, next: CameraRaw) {
    if d.params.point_curve != next.point_curve {
        d.curve_state = Default::default();
    }
    if d.params != next {
        remember_edit(d);
        d.params = next;
        d.dirty = true;
    }
}

/// The dialog's settings with `preset` applied over them (Auto white balance measured on the
/// image), and what applying it reported.
fn with_preset(d: &CameraRawDialog, preset: &PresetFile) -> Result<(CameraRaw, xmp::LoadReport), String> {
    let mut next = d.params.clone();
    let report = xmp::load(&mut next, &preset.bytes).map_err(|e| format!("{}: {e}", preset.name))?;
    if report.auto_white_balance
        && let Some((t, n)) = photocraft_algo::camera_raw::auto_white_balance(&d.proxy)
    {
        (next.temperature, next.tint) = (t, n);
    }
    next.validate_changes(&d.original).map_err(|e| format!("{}: {e}", preset.name))?;
    Ok((next, report))
}

/// Applies `preset` over the dialog's settings, one undo step.
pub(super) fn apply(d: &mut CameraRawDialog, preset: &PresetFile) -> Result<Value, String> {
    let base = d.params.clone();
    let (next, report) = with_preset(d, preset)?;
    d.hover_params = None;
    d.presets.hovered = None;
    set_params(d, next.clone());
    d.presets.applied = Some(Applied { name: preset.name.clone(), path: preset.path.clone(), base, target: next.clone(), amount: 100.0, current: next });
    Ok(json!({"name": preset.name, "applied": report.applied, "ignored": report.ignored}))
}

/// The applied preset's Amount (0 … 200 %): its settings mixed with the ones it was applied over.
/// `remember`: one undo step of its own (the dialog's drags are one step already).
pub(super) fn set_amount(d: &mut CameraRawDialog, amount: f32, remember: bool) -> Result<(), String> {
    if !amount.is_finite() || !(0.0..=200.0).contains(&amount) {
        return Err("a preset's Amount is 0–200".into());
    }
    let next = {
        let a = d.presets.applied.as_mut().filter(|a| a.current == d.params).ok_or("no preset is applied")?;
        a.amount = amount;
        a.current = xmp::blend(&a.base, &a.target, amount / 100.0);
        a.current.clone()
    };
    if remember {
        set_params(d, next);
    } else {
        d.params = next;
        d.dirty = true;
    }
    Ok(())
}

/// Saves the dialog's settings in `sections` as preset `name` in `group` (PhotoCraft's folder).
pub(super) fn save(d: &mut CameraRawDialog, own: Option<&str>, name: &str, group: &str, sections: &[Section]) -> Result<String, String> {
    let own = own.ok_or(tl!("Presets can't be saved here"))?;
    if name.trim().is_empty() {
        return Err(tl!("A preset needs a name").into());
    }
    if sections.is_empty() {
        return Err(tl!("Choose at least one group of settings").into());
    }
    let uuid = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (name, group, crate::gpu_canvas::now_ms().to_bits()).hash(&mut h);
        format!("{:016X}{:016X}", h.finish(), super::next_source())
    };
    let text = xmp::save_preset(&d.params, name.trim(), group.trim(), &uuid, sections);
    let path = store::write(own, group.trim(), name.trim(), &text)?;
    d.presets.rescan();
    Ok(path)
}

/// Deletes PhotoCraft preset `path`.
pub(super) fn delete(d: &mut CameraRawDialog, own: Option<&str>, path: &str) -> Result<(), String> {
    store::delete(own.ok_or(tl!("Presets can't be saved here"))?, path)?;
    d.presets.rescan();
    Ok(())
}

/// The status a panel action leaves: the text and whether it is an error.
pub(super) type Status = Option<(String, bool)>;

fn report_status(name: &str, r: &Value) -> (String, bool) {
    let ignored = r.get("ignored").and_then(Value::as_array).map_or(0, Vec::len);
    if ignored == 0 {
        (crate::i18n::fmt(tl!("Applied preset {name}"), &[("name", name)]), false)
    } else {
        (crate::i18n::fmt(tl!("Applied preset {name} ({n} settings not supported)"), &[("name", name), ("n", &ignored.to_string())]), false)
    }
}

/// The Presets panel. Returns a status to show.
pub(super) fn panel(ui: &mut egui::Ui, d: &mut CameraRawDialog, dirs: &CameraRawPresetDirs) -> Status {
    let t = Tokens::get(ui.ctx());
    let mut status: Status = None;
    d.presets.favorites_path.clone_from(&dirs.favorites);
    d.presets.own_dir.clone_from(&dirs.own);
    d.presets.poll();
    d.presets.refresh(folders(dirs, d.presets.show_adobe), ui.ctx());
    let width = ui.available_width();
    ui.horizontal(|ui| {
        ui.set_max_width(width);
        let search_id = ui.scope_id().with("cr-preset-search");
        widgets::text_field(ui, search_id, &mut d.presets.search, tl!("Search presets"), (width - 64.0).max(80.0), false);
        if crate::icons::button(ui, "plus", 26.0, d.presets.create.is_some(), tl!("Create Preset…")).clicked() {
            d.presets.create = if d.presets.create.is_some() { None } else { Some(CreateForm::default()) };
        }
        let more = crate::icons::button(ui, "ellipsis", 26.0, false, tl!("Presets menu"));
        egui::Popup::menu(&more).show(|ui| {
            if !dirs.adobe.is_empty() && widgets::checkbox(ui, &mut d.presets.show_adobe, tl!("Show Adobe's Presets")).changed() {
                ui.close();
            }
            if ui.button(tl!("Refresh")).clicked() {
                d.presets.rescan();
                ui.close();
            }
        });
    });
    if let Some(name) = d.presets.applied.as_ref().filter(|a| a.current == d.params).map(|a| a.name.clone()) {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(name).strong());
        let mut amount = d.presets.applied.as_ref().map_or(100.0, |a| a.amount);
        let r = widgets::slider_row_compact(ui, tl!("Amount"), &mut amount, 0.0..=200.0, None);
        if r.changed()
            && let Err(e) = set_amount(d, amount, false)
        {
            status = Some((e, true));
        }
        if r.double_clicked() {
            let _ = set_amount(d, 100.0, false);
        }
    }
    if d.presets.create.is_some() {
        status = create_form(ui, d, dirs).or(status);
    }
    ui.add_space(4.0);
    let query = d.presets.search.trim().to_lowercase();
    let list = d.presets.list.clone();
    let shown: Vec<&PresetFile> =
        list.iter().filter(|p| query.is_empty() || p.name.to_lowercase().contains(&query) || p.group.to_lowercase().contains(&query)).collect();
    if shown.is_empty() {
        let text = if d.presets.scanning() {
            tl!("Looking for presets…")
        } else if query.is_empty() {
            tl!("No presets yet. Create one with +, or put Camera Raw presets (.xmp) in PhotoCraft's presets folder.")
        } else {
            tl!("No presets match.")
        };
        ui.label(egui::RichText::new(text).color(t.text_faint));
    }
    let mut hovered_now: Option<String> = None;
    let mut clicked: Option<PresetFile> = None;
    let mut action: Option<(&'static str, PresetFile)> = None;
    let applied_path = d.presets.applied.as_ref().filter(|a| a.current == d.params).map(|a| a.path.clone());
    // The Favorites first, as Camera Raw's Presets panel lists them: the ones Camera Raw's
    // favourites file names, and the ones starred here (user report: Photoshop's favourites were
    // missing). Each preset once, by name.
    let favorites = d.presets.favorites.clone();
    let stars = d.presets.own_favorites.clone();
    let mut fav: Vec<&PresetFile> = Vec::new();
    for p in shown.iter().filter(|p| (!p.uuid.is_empty() && favorites.contains(&p.uuid)) || stars.contains(star_key(p))) {
        if !fav.iter().any(|f| f.path == p.path || (!f.uuid.is_empty() && f.uuid == p.uuid)) {
            fav.push(p);
        }
    }
    fav.sort_by_key(|p| p.name.to_lowercase());
    // (section title, groups: label, presets, open by default, id).
    type Group<'a> = (&'a str, Vec<&'a PresetFile>, bool, (Option<Source>, &'a str));
    let mut sections: Vec<(Option<&str>, Vec<Group<'_>>)> = Vec::new();
    if !fav.is_empty() {
        sections.push((None, vec![(tl!("Favorites"), fav, true, (None, "favorites"))]));
    }
    for (source, title) in [(Source::Own, tl!("My Presets")), (Source::Photoshop, tl!("Photoshop")), (Source::Adobe, tl!("Adobe"))] {
        if source == Source::Adobe && !d.presets.show_adobe {
            continue;
        }
        let of_source: Vec<&PresetFile> = shown.iter().filter(|p| p.source == source).copied().collect();
        if of_source.is_empty() {
            continue;
        }
        let mut groups: Vec<&str> = of_source.iter().map(|p| p.group.as_str()).collect();
        groups.dedup();
        let groups = groups
            .into_iter()
            .map(|group| {
                let label = if group.is_empty() { tl!("User Presets") } else { group };
                let presets = of_source.iter().filter(|p| p.group == group).copied().collect();
                (label, presets, !query.is_empty() || source == Source::Own, (Some(source), group))
            })
            .collect();
        sections.push((Some(title), groups));
    }
    for (title, groups) in sections {
        if let Some(title) = title {
            widgets::section_label(ui, title);
        }
        for (label, presets, open, salt) in groups {
            egui::CollapsingHeader::new(label).id_salt(("cr-preset-group", salt)).default_open(open).show(ui, |ui| {
                for p in presets {
                    let selected = applied_path.as_deref() == Some(p.path.as_str());
                    let response = ui.add_sized([ui.available_width(), 16.0], egui::Button::selectable(selected, p.name.as_str()));
                    if response.hovered() {
                        hovered_now = Some(p.path.clone());
                    }
                    if response.clicked() {
                        clicked = Some(p.clone());
                    }
                    response.context_menu(|ui| {
                        if ui.button(tl!("Apply")).clicked() {
                            clicked = Some(p.clone());
                            ui.close();
                        }
                        if dirs.own.is_some() {
                            let starred = stars.contains(star_key(p));
                            if ui.button(if starred { tl!("Remove from Favorites") } else { tl!("Add to Favorites") }).clicked() {
                                action = Some(("star", p.clone()));
                                ui.close();
                            }
                        }
                        if p.source == Source::Own {
                            if ui.button(tl!("Rename…")).clicked() {
                                action = Some(("rename", p.clone()));
                                ui.close();
                            }
                            if ui.button(tl!("Update with Current Settings")).clicked() {
                                action = Some(("update", p.clone()));
                                ui.close();
                            }
                            if ui.button(tl!("Delete")).clicked() {
                                action = Some(("delete", p.clone()));
                                ui.close();
                            }
                        } else if dirs.own.is_some() && ui.button(tl!("Copy to My Presets")).clicked() {
                            action = Some(("copy", p.clone()));
                            ui.close();
                        }
                    });
                }
            });
        }
    }
    if let Some((path, name)) = d.presets.renaming.clone() {
        let mut name = name;
        let mut done = None;
        egui::Window::new(tl!("Rename Preset")).id(egui::Id::unique("cr-preset-rename")).collapsible(false).resizable(false).show(ui.ctx(), |ui| {
            widgets::text_field(ui, ui.scope_id().with("cr-preset-rename-name"), &mut name, tl!("Name"), 220.0, false);
            ui.horizontal(|ui| {
                if widgets::secondary_button(ui, tl!("Rename"), 70.0).clicked() {
                    done = Some(true);
                }
                if widgets::secondary_button(ui, tl!("Cancel"), 70.0).clicked() {
                    done = Some(false);
                }
            });
        });
        match done {
            Some(true) => {
                d.presets.renaming = None;
                match dirs.own.as_deref().ok_or_else(|| tl!("Presets can't be saved here").to_string()).and_then(|own| store::rename(own, &path, name.trim())) {
                    Ok(_) => d.presets.rescan(),
                    Err(e) => status = Some((e, true)),
                }
            }
            Some(false) => d.presets.renaming = None,
            None => d.presets.renaming = Some((path, name)),
        }
    }
    if let Some((what, p)) = action {
        let r = match what {
            "rename" => {
                d.presets.renaming = Some((p.path.clone(), p.name.clone()));
                Ok(())
            }
            "delete" => delete(d, dirs.own.as_deref(), &p.path),
            "star" => d.presets.toggle_star(&p),
            "update" => {
                // The same groups of settings the preset held, now from the current settings.
                let held = held_sections(&p.bytes);
                save(d, dirs.own.as_deref(), &p.name, &p.group, &held).map(|_| ())
            }
            _ => dirs.own.as_deref().ok_or_else(|| tl!("Presets can't be saved here").to_string()).and_then(|own| {
                let text = String::from_utf8_lossy(&p.bytes).into_owned();
                store::write(own, &p.group, &p.name, &text).map(|_| d.presets.rescan())
            }),
        };
        if let Err(e) = r {
            status = Some((e, true));
        }
    }
    if let Some(p) = clicked {
        status = Some(match apply(d, &p) {
            Ok(r) => report_status(&p.name, &r),
            Err(e) => (e, true),
        });
    } else if hovered_now != d.presets.hovered {
        // Pointing at a preset previews it; leaving the list shows the settings again.
        d.hover_params = hovered_now
            .as_deref()
            .and_then(|path| d.presets.list.iter().find(|p| p.path == path).cloned())
            .and_then(|p| with_preset(d, &p).ok())
            .map(|(next, _)| next);
        d.presets.hovered = hovered_now;
        d.dirty = true;
    }
    status
}

/// The sections a preset file holds settings of.
fn held_sections(bytes: &[u8]) -> Vec<Section> {
    let text = String::from_utf8_lossy(bytes);
    Section::ALL
        .into_iter()
        .filter(|s| text.split("crs:").skip(1).filter_map(|t| t.split(['=', '>', ' ']).next()).any(|name| Section::of(name) == Some(*s)))
        .collect()
}

/// The Create Preset form, inline above the list.
fn create_form(ui: &mut egui::Ui, d: &mut CameraRawDialog, dirs: &CameraRawPresetDirs) -> Status {
    let t = Tokens::get(ui.ctx());
    let mut status = None;
    let groups: Vec<String> = {
        let mut g: Vec<String> = d.presets.list.iter().filter(|p| p.source == Source::Own && !p.group.is_empty()).map(|p| p.group.clone()).collect();
        g.sort();
        g.dedup();
        g
    };
    let mut close = false;
    let mut wanted: Option<(String, String, Vec<Section>)> = None;
    egui::Frame::new().fill(t.card).stroke(egui::Stroke::new(1.0, t.card_border)).corner_radius(t.radius_sm).inner_margin(8.0).show(ui, |ui| {
        let Some(form) = d.presets.create.as_mut() else { return };
        ui.label(egui::RichText::new(tl!("Create Preset")).strong());
        widgets::text_field(ui, ui.scope_id().with("cr-create-name"), &mut form.name, tl!("Name"), ui.available_width(), false);
        ui.horizontal(|ui| {
            widgets::text_field(ui, ui.scope_id().with("cr-create-group"), &mut form.group, tl!("Group"), (ui.available_width() - 30.0).max(80.0), false);
            if !groups.is_empty() {
                ui.menu_button("▾", |ui| {
                    for g in &groups {
                        if ui.button(g).clicked() {
                            form.group = g.clone();
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text(tl!("Existing groups"));
            }
        });
        ui.horizontal(|ui| {
            if ui.small_button(tl!("Check All")).clicked() {
                form.sections = Section::ALL.to_vec();
            }
            if ui.small_button(tl!("Check None")).clicked() {
                form.sections.clear();
            }
        });
        ui.columns(2, |cols| {
            for (i, s) in Section::ALL.into_iter().enumerate() {
                let mut on = form.sections.contains(&s);
                if widgets::checkbox(&mut cols[i % 2], &mut on, crate::i18n::tr_ctx(crate::i18n::current(), "cameraRaw", s.label())).changed() {
                    if on {
                        form.sections.push(s);
                    } else {
                        form.sections.retain(|x| *x != s);
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            if widgets::secondary_button(ui, tl!("Save"), 70.0).clicked() {
                wanted = Some((form.name.clone(), form.group.clone(), form.sections.clone()));
            }
            if widgets::secondary_button(ui, tl!("Cancel"), 70.0).clicked() {
                close = true;
            }
        });
    });
    if let Some((name, group, sections)) = wanted {
        status = Some(match save(d, dirs.own.as_deref(), &name, &group, &sections) {
            Ok(_) => {
                close = true;
                (crate::i18n::fmt(tl!("Saved preset {name}"), &[("name", name.trim())]), false)
            }
            Err(e) => (e, true),
        });
    }
    if close {
        d.presets.create = None;
    }
    status
}
