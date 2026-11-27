use super::thumbs::{channel_tint, same_but_selection};
use super::{
    DocId, PhotocraftApp, Tool, Value, actions, canvas, delete_layer_prompt, dock, gpu_canvas, jobs_ui, json, mask_trash_prompt, notices, panel_docking,
    smart_svg_ui, state, transform_tool, type_transform, vector_ui,
};
use photocraft_doc::Document;

/// A command that took this long (ms) the last time shows the busy cursor while it runs.
const WAIT_CURSOR_MS: f64 = 100.0;

impl PhotocraftApp {
    /// Run an engine command, reporting errors in the status bar.
    pub fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        let params = if id == "select.findLayers" { crate::layer_filter_ui::find::params_for_run(self, params) } else { params };
        if let Some(result) = delete_layer_prompt::intercept(self, id, &params) {
            return result;
        }
        if let Some(result) = mask_trash_prompt::intercept(self, id, &params) {
            return result;
        }
        // A preset saved with its tool's settings (Photoshop's Eraser or Smudge presets) belongs
        // to that tool: picking it switches there first, so it becomes that tool's brush.
        if id == "tools.setBrush" {
            crate::paint_mouse::switch_to_preset_tool(self, &params);
        }
        // Automation input also gates every step a command runs on its behalf (`actions.play`).
        let gate = if self.automation_input && self.session.authorize.is_none() { self.services.automation_authorize } else { None };
        if gate.is_some() {
            self.session.authorize = gate;
        }
        let undone =
            matches!(id, "edit.undo" | "edit.stepBackward").then(|| self.session.active().and_then(|st| st.history.undo_label().map(str::to_string))).flatten();
        let generative = (id == "edit.generativeFill").then(|| params.clone());
        let prompt = matches!(id, "edit.generativeFill" | "paint.remove").then(|| params.get("prompt").and_then(Value::as_str).map(str::to_string)).flatten();
        let result = self.run_command(id, params);
        if gate.is_some() {
            self.session.authorize = None;
        }
        if let (Ok(_), Some(p)) = (&result, &generative) {
            crate::dialog_memory::used_generative_fill(self, p);
        }
        // Generative Fill's and the Remove tool's prompt go to the recent list (user request).
        if let (Ok(_), Some(prompt)) = (&result, &prompt) {
            crate::dialog_memory::remember_prompt(self, prompt);
        }
        if result.is_ok() && undone.is_some() {
            crate::fill_tool_ui::after_undo(self, undone.as_deref());
        }
        // An explicit channel target (⌘2, ⌘\, ⌘3…) decides what the Layers panel targets too:
        // ⌘2 after the mask was targeted goes back to the layer's pixels.
        if result.is_ok() && id.starts_with("channel.target") {
            if let Some(t) = self.session.active().map(|st| st.channel_view.target) {
                use photocraft_engine::channel_cmds::ChannelTarget;
                self.ui.mask_target = t == ChannelTarget::LayerMask;
                self.ui.vector_mask_target = t == ChannelTarget::VectorMask;
            }
            self.sync_mask_targets();
        }
        // Find Layers sets the Layers panel's Name filter however it runs: the menu, the shortcut,
        // `engine.execute` or an action step (#10884).
        if id == "select.findLayers"
            && let Ok(reply) = &result
        {
            crate::layer_filter_ui::find::apply(self, reply);
        }
        result
    }

    pub(crate) fn run_command(&mut self, id: &str, params: Value) -> Result<Value, String> {
        self.run_depth += 1;
        let r = self.run_command_at_depth(id, params);
        self.run_depth -= 1;
        r
    }

    // One dispatcher: both branches' pre- and post-command hooks meet here (0.21 merge).
    #[allow(clippy::cognitive_complexity)]
    fn run_command_at_depth(&mut self, id: &str, params: Value) -> Result<Value, String> {
        if !vector_ui::cancel_stale(self) {
            return Err(self.ui.status.clone());
        }
        if matches!(id, "paint.setSymmetry" | "paint.symmetryFromPath" | "paint.symmetryDisable") {
            self.ui.symmetry_transform = None;
        }
        let clip_read = std::mem::take(&mut self.clip_read_for_paste);
        if self.automation_input
            && let Some(authorize) = self.services.automation_command.as_ref()
        {
            authorize(id, &params)?;
        }
        // A new layer while Free Transform is open applies the transform first, as Photoshop does
        // (#5667): History reads the transform (or Place Embedded), then New Layer. Only for a
        // command that can run; a transform refused or still being applied keeps the box open and
        // the command doesn't run.
        if photocraft_engine::PRECHECKED.contains(&id) && self.ui.transform.is_some() {
            // Bad params or a command that can't run leave the box as it is.
            self.session.precheck(id, &params).map_err(|e| e.to_string())?;
            transform_tool::commit(self);
            if transform_tool::committing(self).is_some() {
                // A click stays a click: it runs when the commit lands (#9974). Only a click in the
                // committing document, one at a time: a script, a control request or an action step
                // needs its answer now, and a click in another tab wasn't meant for this one.
                let committing_here = self.transform_preview.as_ref().is_some_and(|pv| self.session.active().is_some_and(|st| st.doc.id == pv.doc.id));
                let click = !self.automation_input && self.session.authorize.is_none() && self.run_depth == 1;
                if !click || !committing_here || self.after_transform_commit.is_some() {
                    return Err(tl!("Free Transform is still being applied; try again when it has finished.").into());
                }
                self.after_transform_commit = Some((id.to_string(), params));
                return Ok(Value::Null);
            }
            if self.ui.transform.is_some() {
                return Err(if self.ui.status.is_empty() { tl!("The transform couldn't be applied.").into() } else { self.ui.status.clone() });
            }
        }
        actions::suspended_guard(self, id)?;
        if let Some(result) = panel_docking::command(self, id, &params) {
            return result;
        }
        if let Some(result) = smart_svg_ui::intercept(self, id, &params) {
            return result;
        }
        // Document exports read committed pixels, not the pending Free Transform preview.
        // Check on execution (also dialog OK and automation), leaving Folder processing usable.
        if id == "file.scripts.imageProcessor"
            && self.ui.transform.is_some()
            && match params.get("source").and_then(Value::as_str) {
                Some("open") => true,
                Some("current") => self.transform_preview.as_ref().is_none_or(|preview| self.session.active().is_some_and(|st| st.doc.id == preview.doc.id)),
                _ => false,
            }
        {
            return Err("Commit or cancel the transform first".into());
        }
        if let Some(r) = transform_tool::intercept(self, id) {
            return r;
        }
        if let Some(r) = crate::crop_ui::intercept(self, id) {
            return r;
        }
        let t0 = gpu_canvas::now_ms();
        // The OS clipboard is read only on an explicit paste, never in the background (privacy, CPU).
        if matches!(id, "edit.paste" | "edit.pasteSpecial.pasteInPlace" | "file.newFromClipboard") {
            let paste_as = if id == "edit.paste" {
                match params.get("as") {
                    None => None,
                    Some(value) => match value.as_str() {
                        Some(mode @ ("pixels" | "path" | "shape")) => Some(mode),
                        _ => return Err("`as` must be pixels, path or shape".into()),
                    },
                }
            } else {
                None
            };
            if let Some(mode) = paste_as {
                // Pixels can come from the bitmap when the SVG can't be read; vectors can't.
                let (imported, svg_error) = match self.import_os_svg_clipboard() {
                    Ok(imported) => (imported, None),
                    Err(error) if mode == "pixels" => (false, Some(error)),
                    Err(error) => return Err(error),
                };
                if mode == "pixels"
                    && !imported
                    && !clip_read
                    && let Err(error) = self.import_os_clipboard()
                {
                    return Err(svg_error.unwrap_or(error));
                }
                if let Some(error) = svg_error {
                    if self.session.clipboard.is_none() {
                        return Err(error);
                    }
                    self.ui.status.clear();
                    self.ui.status_error = false;
                }
            } else if !clip_read {
                self.import_os_clipboard()?;
            }
            if paste_as.is_none() && self.session.clipboard.is_none() && self.services.clipboard_get_image.is_some() {
                if self.services.clipboard_unreadable.as_ref().is_some_and(|unreadable| unreadable()) {
                    // The clipboard may well hold an image that can't be reached: say why.
                    notices::clipboard_unreadable(self);
                    return Ok(serde_json::json!({"pasted": false}));
                }
                // Enabled on the strength of the OS clipboard, which held no image: a quiet no-op.
                let message = "Nothing to paste: the clipboard holds no image";
                self.ui.status = tl!(message).into();
                self.ui.status_error = false;
                return Ok(serde_json::json!({"pasted": false}));
            }
        }
        let suppress_events = self.automation_input && self.session.prefs().script_events.enabled;
        if suppress_events {
            self.session.edit_prefs(|prefs| prefs.script_events.enabled = false);
        }
        // Long commands become background jobs when enabled (`jobs_ui`); the rest run inline.
        let params = self.with_mask_target(id, params);
        let params = vector_ui::with_active_path(self, id, params);
        let path_mask = vector_ui::takes_path_mask(id) && params.get("path").is_some_and(|v| !v.is_null());
        let on_active_layer = self
            .session
            .active()
            .is_some_and(|st| st.active_layer.is_some_and(|layer| params.get("layer").and_then(Value::as_u64).is_none_or(|target| target == layer.0)));
        let creates_active_mask =
            matches!(id, "layer.layerMask.revealAll" | "layer.layerMask.hideAll" | "layer.layerMask.revealSelection" | "layer.layerMask.hideSelection")
                && on_active_layer;
        let creates_active_vector_mask = matches!(
            id,
            "layer.vectorMask.add" | "layer.vectorMask.fromPath" | "layer.vectorMask.revealAll" | "layer.vectorMask.hideAll" | "layer.vectorMask.currentPath"
        ) && on_active_layer;
        // The busy cursor, for a command that held the window the last time it ran: set from a
        // frame it would only show once the command is done.
        let wait = !self.perf.waiting && self.perf.command_cost.get(id).is_some_and(|ms| *ms >= WAIT_CURSOR_MS);
        if let Some(show) = self.services.wait_cursor.as_ref().filter(|_| wait) {
            show(true);
            self.perf.waiting = true;
        }
        // An action plays its steps in the engine, so the targeting a Layers panel click adds
        // after Add Mask doesn't run: compare the active layer's masks after it.
        let doc_before = (id == "actions.play").then(|| self.session.active().map(|st| std::sync::Arc::clone(&st.doc))).flatten();
        let r = if id == "actions.play" {
            actions::play(self, &params)
        } else if photocraft_engine::actions_cmds::shell_view_command(id) {
            actions::run_view(self, id, params)
        } else {
            jobs_ui::run(self, id, params)
        };
        if let Some(show) = self.services.wait_cursor.as_ref().filter(|_| wait) {
            show(false);
            self.perf.waiting = false;
        }
        if let Ok(v) = &r
            && id == photocraft_engine::gigapixel_cmds::ID
        {
            // The image went to Gigapixel's app: watch for its export.
            crate::gigapixel_ui::started(self, v);
        }
        let creates_adjustment_or_fill = id.starts_with("layer.newAdjustmentLayer.") || id.starts_with("layer.newFillLayer.");
        // Add Mask says which mask it made: a new layer or vector mask is targeted as above.
        let added = r
            .as_ref()
            .ok()
            .filter(|_| id == photocraft_engine::second_mask_cmds::ADD_MASK && on_active_layer)
            .and_then(|v| v.get("added"))
            .and_then(Value::as_str);
        let gained = doc_before.filter(|_| r.is_ok()).map(|before| self.masks_gained(&before)).unwrap_or_default();
        let creates_active_mask = creates_active_mask || added == Some("mask") || gained.mask;
        let creates_active_vector_mask = creates_active_vector_mask || added == Some("vectorMask") || (gained.vector && !gained.mask && !gained.second);
        self.sync_command_side_effects(id, &r, creates_active_mask, creates_active_vector_mask, creates_adjustment_or_fill, path_mask);
        if gained.second && !gained.mask {
            // A second mask the action added is targeted, as the button targets it.
            (self.ui.mask_target, self.ui.second_mask_target, self.ui.vector_mask_target) = (true, true, false);
            self.target_mask_over_colour_rows();
            self.sync_mask_targets();
        } else if let Some(index) = gained.effect.filter(|_| !gained.mask && !gained.second && !gained.vector) {
            let layer = self.session.active().and_then(|st| st.active_layer).map(|l| l.0);
            if let Some(layer) = layer {
                self.select_fx_mask_target(Some((layer, index)));
            }
        }
        self.perf.last_command = id.to_string();
        self.perf.command_ms = gpu_canvas::now_ms() - t0;
        self.perf.command_cost.insert(id.to_string(), self.perf.command_ms);
        if self.perf.command_ms >= 1.0 {
            self.perf.frame_commands.push((id.to_string(), self.perf.command_ms));
        }
        if self.run_depth == 1 {
            self.perf.stage.command(id, self.perf.command_ms);
        }
        match &r {
            Ok(_) => {
                if self.ui.status_error {
                    self.ui.status.clear();
                    self.ui.status_error = false;
                }
                self.sync_views();
                let removed = self.session.take_samplers_removed();
                if removed > 0 && !self.ui.status_error {
                    // A canvas-size change (crop, Canvas Size, Image Size, rotation) dropped Color
                    // Sampler points that fell outside the new canvas (#1046).
                    self.ui.status = crate::i18n::trn(crate::i18n::current(), removed as u64, "{n} sample point removed", "{n} sample points removed");
                    self.ui.status_error = false;
                }
            }
            Err(e) => {
                self.ui.status = e.clone();
                self.ui.status_error = true;
            }
        }
        if suppress_events {
            self.session.edit_prefs(|prefs| prefs.script_events.enabled = true);
        }
        r
    }

    /// Keep one view per document, in tab order: a view and its windows stay with their document
    /// when tabs move (`document.move`) or close.
    /// The tool pointer events go to: a held temporary tool when there is one (⌘ is the Move
    /// tool, `hold_keys::cmd_moves`), else the selected tool.
    pub fn active_tool(&self) -> state::Tool {
        self.tool_override.unwrap_or(self.ui.tool)
    }

    /// Which masks the active layer has now that it didn't have in `before` (a layer `before`
    /// lacks had none): what a played action added, to target as the Layers panel's Add Mask does
    /// (user report: an action's Add Mask left the layer's pixels targeted).
    fn masks_gained(&self, before: &Document) -> MasksGained {
        let Some(st) = self.session.active() else { return MasksGained::default() };
        let Some(l) = st.active_layer.and_then(|id| st.doc.layer(id)) else { return MasksGained::default() };
        let old = before.layer(l.id);
        let had = |f: fn(&photocraft_doc::Layer) -> bool| old.is_some_and(f);
        MasksGained {
            mask: l.mask.is_some() && !had(|o| o.mask.is_some()),
            second: l.second_mask.is_some() && !had(|o| o.second_mask.is_some()),
            vector: l.vector_mask.is_some() && !had(|o| o.vector_mask.is_some()),
            effect: l
                .effects
                .items
                .iter()
                .enumerate()
                .position(|(i, e)| e.mask().is_some() && !old.and_then(|o| o.effects.items.get(i)).is_some_and(|o| o.mask().is_some())),
        }
    }

    /// Selects the newly attached type path on composite channel target so the path becomes immediately editable.
    fn select_attached_type_path(&mut self, command: &str, result: &Result<Value, String>) {
        if command != "type.attachToPath" {
            return;
        }
        let attached = result.as_ref().ok().and_then(|value| value.get("layer")).and_then(Value::as_u64);
        let active = self.session.active().and_then(|state| state.active_layer).map(|layer| layer.0);
        if attached.is_some() && attached == active {
            // New path text becomes the editable target; its source work path stays intact.
            let _ = self.session.execute("channel.target.composite", json!({}));
            self.ui.selected_path = Some("layer".into());
            self.ui.vector_mask_target = false;
            self.ui.mask_target = false;
        }
    }

    /// Updates secondary UI state following command execution.
    fn sync_command_side_effects(
        &mut self,
        id: &str,
        result: &Result<Value, String>,
        creates_active_mask: bool,
        creates_active_vector_mask: bool,
        creates_adjustment_or_fill: bool,
        path_mask: bool,
    ) {
        if result.is_ok() && ADDS_LAYER_MASK.contains(&id) {
            // Adding an explicit mask targets it, as in Photoshop (#2166).
            self.ui.mask_target = true;
            self.ui.vector_mask_target = false;
            self.ui.properties_layer = None;
        } else if result.is_ok() && creates_adjustment_or_fill {
            // Panel Options may suppress a fill layer's default mask. Never target a
            // nonexistent mask; an active path creates a vector mask instead.
            let masks = self
                .session
                .active()
                .and_then(|st| st.active_layer.and_then(|id| st.doc.layer(id)))
                .map(|layer| (layer.mask.is_some(), layer.vector_mask.is_some()))
                .unwrap_or((false, false));
            self.ui.mask_target = masks.0;
            self.ui.vector_mask_target = !masks.0 && masks.1;
            // Keep Properties on the new layer contents, as on main (#3369).
            self.ui.properties_layer = self.session.active().and_then(|st| st.active_layer).map(|l| l.0);
        }
        if result.is_ok() && creates_adjustment_or_fill {
            // Properties comes to the front, shown and expanded, so the new layer's settings can
            // be edited at once, as in Photoshop (#4028). The layer is made whatever happens here.
            if self.ui.docking.is_some() {
                let _ = panel_docking::command(self, "window.panel.activate", &json!({"panel": "properties"}));
            } else {
                self.ui.dock_tabs.properties = 0;
                dock::reveal(self, dock::Group::Properties);
            }
        }
        // Making a new selection reveals its outline after View > Extras was hidden (#5597).
        // These interactive selection commands complete inline; a failed command, deselection,
        // or painting inside an existing selection must leave the user's view choice alone.
        if result.as_ref().is_ok_and(|v| v.get("changed") != Some(&Value::Bool(false)) && v.get("pending") != Some(&Value::Bool(true)))
            && matches!(
                id,
                "select.rect"
                    | "select.lasso"
                    | "select.magneticLasso"
                    | "select.magicWand"
                    | "select.quick"
                    | "select.object"
                    | "select.loadSelection"
                    | "select.loadLuminosity"
                    | "path.toSelection"
                    | "path.addToSelection"
                    | "path.subtractFromSelection"
                    | "path.intersectWithSelection"
            )
            && self.session.active().is_some_and(|st| st.doc.selection.is_some())
        {
            self.ui.view.extras = true;
        }
        self.select_attached_type_path(id, result);
        if result.is_ok() && id == "select.toWorkPath" {
            // Make Work Path selects the new work path in the Paths panel, as in Photoshop.
            self.ui.selected_path = Some("work".into());
        }
        if result.is_ok() && path_mask {
            // The new layer's vector mask becomes the active path, as in Photoshop: the path it
            // was made from is no longer selected, so the next fill layer isn't masked by it too.
            self.ui.selected_path = Some("layer".into());
        }
        if result.is_ok() && creates_active_mask {
            // Adding a mask selects its thumbnail: the next brush or footer Delete targets it.
            self.ui.mask_target = true;
            self.ui.vector_mask_target = false;
            self.target_mask_over_colour_rows();
        }
        if result.is_ok() && creates_active_vector_mask {
            // Likewise a new vector mask (#4728): Properties shows its Density and Feather, and a
            // mask made from a path becomes the active path.
            self.ui.vector_mask_target = true;
            self.ui.mask_target = false;
            if matches!(id, "layer.vectorMask.currentPath" | "layer.vectorMask.fromPath") {
                self.ui.selected_path = Some("layer".into());
            }
        }
        if result.is_ok() && matches!(id, "edit.copy" | "edit.cut" | "edit.copyMerged") {
            self.clip_external = false;
            self.export_os_clipboard();
        }
    }

    /// Reconciles canvas view state with open documents in the session.
    pub fn sync_views(&mut self) {
        crate::type_path_placement::cancel_stale(self);
        // The Paths panel selection belongs to the active document. Ordinary edits replace the
        // snapshot and advance its revision; a replaced snapshot without a newer revision is a
        // reopened document, even with the same ID.
        let document = self.session.active().map(|st| (st.doc.id, st.revision, std::sync::Arc::downgrade(&st.doc)));
        // Crop's Fill goes back to its default entry when another document that was already open
        // becomes active (a tab switch, or closing the active one), as in the reference
        // application. A new or opened document isn't in `view_docs` yet, so it keeps the setting,
        // as do tool switches, commits and cancels, which leave the active document alone (#10894).
        if let (Some((id, ..)), Some((old_id, ..))) = (&document, &self.path_document)
            && id != old_id
            && self.view_docs.contains(id)
        {
            self.ui.tool_options.crop_content_aware = false;
        }
        let same_document = match (&self.path_document, &document) {
            (Some((old_id, old_revision, old_source)), Some((id, revision, source))) => old_id == id && (revision > old_revision || old_source.ptr_eq(source)),
            (None, None) => true,
            _ => false,
        };
        if !same_document {
            self.ui.selected_path = None;
        }
        self.path_document = document;
        // A shape box handle drag belongs to its document, revision and tool.
        crate::shape_bounds_box::cancel_stale(self);
        vector_ui::cancel_stale_pen(self);
        crate::direct_select::cancel_stale(self);
        canvas::cancel_stale_vector_drag(self);
        type_transform::cancel_stale(self);
        crate::cam_transform::cancel_stale(self);
        crate::lasso_ui::cancel_stale(self);
        crate::retouch_ui::cancel_stale(self);
        // A polygon under construction belongs to the Polygonal Lasso tool, not the next tool.
        if self.ui.tool != Tool::PolygonLasso {
            self.ui.polygon.clear();
            self.ui.polygon_mode.clear();
        }
        // ... and to the document it was started on.
        crate::canvas::polygon_cancel_stale(self);
        crate::crop_ui::cancel_stale(self);
        crate::distort_ui::cancel_stale(self);
        crate::perspective_crop_ui::cancel_stale(self);
        // A pen path under construction belongs to the document it was started on too.
        vector_ui::cancel_stale(self);
        let ids: Vec<DocId> = self.session.documents().iter().map(|d| d.doc.id).collect();
        // Where the document of view `i` is now. Views not tracked yet keep their index.
        let now = |i: usize| match self.view_docs.get(i) {
            Some(id) => ids.iter().position(|d| d == id),
            None => (i < ids.len()).then_some(i),
        };
        let mut views: Vec<Option<state::View>> = ids.iter().map(|_| None).collect();
        for (i, v) in std::mem::take(&mut self.ui.views).into_iter().enumerate() {
            if let Some(slot) = now(i).and_then(|to| views.get_mut(to)) {
                *slot = Some(v);
            }
        }
        self.ui.views = views.into_iter().map(Option::unwrap_or_default).collect();
        self.ui.windows.retain_mut(|w| now(w.document).map(|d| w.document = d).is_some());
        if !self.session.prefs().workspace.open_documents_as_tabs {
            let new_docs: Vec<usize> = ids.iter().enumerate().filter(|(_, id)| !self.view_docs.contains(id)).map(|(i, _)| i).collect();
            for &idx in &new_docs {
                if !self.ui.windows.iter().any(|w| w.document == idx) {
                    crate::view_cmds::float_window(self, idx, idx, false);
                }
            }
        }
        self.canvases.retain(|(doc, _), _| ids.contains(doc));
        self.navigator_textures.retain(|doc, _| ids.contains(doc));
        self.snapshot_thumbs.retain(|(doc, _), _| ids.contains(doc));
        // Closing a document cancels its page work; its GPU pages go with `retain_gpu_documents`.
        self.streams.retain(|doc, _| ids.contains(doc));
        self.view_docs = ids;
        self.prune_thumbs();
        self.sync_mask_targets();
        // Channel-view textures outlive a hidden view (cheap re-show), not their document.
        if !self.channel_views.is_empty() {
            let docs = self.session.documents();
            self.channel_views.retain(|id, _| docs.iter().any(|d| d.doc.id.0 == *id));
        }
        // With no document left, no canvas paints the Levels clipping display to drop its cache.
        if self.session.documents().is_empty() {
            self.levels_clipping = None;
        }
        // Commands may close and reopen a preserved-ID document before the next repaint.
        canvas::retain_gpu_documents(self);
    }
}

/// Documents larger than this (pixels) build their Channels thumbnails on a background thread.
pub(super) const ASYNC_CHANNEL_THUMB_PIXELS: u64 = 1 << 20;

/// Channels thumbnail images with their texture names.
type ChannelImages = Vec<(String, egui::ColorImage)>;

/// The Channels panel's thumbnail images of `doc`, named for their textures: the composite, each
/// colour channel (when there is more than one), each alpha channel, then the Quick Mask.
pub(super) fn channel_images(doc: &Document, show_color: bool) -> ChannelImages {
    // `thumbnail(doc, 56)` with its proxy kept, so the ink thumbnails below sample the same proxy
    // instead of building another one.
    let proxy = photocraft_compose::thumbnail_proxy(doc, 56);
    let src = proxy.as_ref().unwrap_or(doc);
    let (tw, th) = photocraft_compose::thumbnail_size(doc, 56);
    let comp = photocraft_compose::render_reduced(src, tw, th).to_rgba8();
    let (w, h) = (comp.width as usize, comp.height as usize);
    let side = w.max(h);
    let (ox, oy) = ((side - w) / 2, (side - h) / 2);
    let make = |f: &dyn Fn(&[u8]) -> egui::Color32, name: &str| {
        let mut px = vec![egui::Color32::TRANSPARENT; side * side];
        for y in 0..h {
            for x in 0..w {
                let o = (y * w + x) * 4;
                if let (Some(p), Some(d)) = (comp.pixels.get(o..o + 4), px.get_mut((y + oy) * side + x + ox)) {
                    *d = f(p);
                }
            }
        }
        (format!("chan-{name}"), egui::ColorImage::new([side, side], px))
    };
    let fmt = doc.pixel_format();
    let colors = fmt.mode.color_channels();
    let mut texs = vec![make(&|p| egui::Color32::from_rgb(p[0], p[1], p[2]), "composite")];
    // CMYK channels show the document's own inks (#8359), not the composite converted back.
    let inks = (fmt.mode == photocraft_doc::ColorMode::Cmyk).then(|| match &proxy {
        Some(proxy) => super::thumbs::ink_thumbnail(proxy, w, h),
        None => match super::thumbs::ink_proxy(doc, 56) {
            Some(proxy) => super::thumbs::ink_thumbnail(&proxy, w, h),
            None => super::thumbs::ink_thumbnail(doc, w, h),
        },
    });
    if colors > 1 {
        for k in 0..colors {
            let mut px = vec![egui::Color32::TRANSPARENT; side * side];
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let g = match &inks {
                        // Ink channels read dark where there is ink.
                        Some(inks) => 1.0 - inks.get(i).map_or(0.0, |v| v[k]),
                        None => {
                            let Some(p) = comp.pixels.get(i * 4..i * 4 + 4) else { continue };
                            let rgba = [p[0], p[1], p[2], 255].map(|v| f32::from(v) / 255.0);
                            let mut native = [0.0f32; 8];
                            photocraft_raster::from_rgba_into(&fmt, rgba, &mut native);
                            native.get(k).copied().unwrap_or(0.0)
                        }
                    };
                    let byte = (g.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    if let Some(d) = px.get_mut((y + oy) * side + x + ox) {
                        *d = channel_tint(fmt.mode, k, byte, show_color);
                    }
                }
            }
            texs.push((format!("chan-c{k}"), egui::ColorImage::new([side, side], px)));
        }
    }
    // Alpha channels and the Quick Mask, sampled at thumbnail resolution.
    let (dw, dh) = (doc.size.width.max(1) as f32, doc.size.height.max(1) as f32);
    for (i, ch) in doc.channels.iter().chain(doc.quick_mask.as_ref()).enumerate() {
        let mut px = vec![egui::Color32::TRANSPARENT; side * side];
        for y in 0..h {
            for x in 0..w {
                let sx = ((x as f32 + 0.5) * dw / w as f32) as i32;
                let sy = ((y as f32 + 0.5) * dh / h as f32) as i32;
                let v = ch.surface.sample_channel(sx, sy, 0);
                if let Some(d) = px.get_mut((y + oy) * side + x + ox) {
                    *d = egui::Color32::from_gray((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
                }
            }
        }
        texs.push((format!("chan-a{i}"), egui::ColorImage::new([side, side], px)));
    }
    texs
}

/// Channels thumbnails being built on a background thread, for document `doc` as it was in
/// `snapshot`.
#[derive(Clone)]
struct ChannelThumbJob {
    doc: DocId,
    snapshot: std::sync::Weak<Document>,
    show_color: bool,
    /// Filled by the thread when it is done (empty if building them panicked).
    result: std::sync::Arc<std::sync::Mutex<Option<ChannelImages>>>,
}

fn channel_thumb_job_id() -> egui::Id {
    egui::Id::unique("pc-channel-thumbs-job")
}

/// The thumbnails of `doc` from the background thread once they are ready (`None` while it
/// runs). One thread at a time: a document changed under a running build waits for it, then
/// gets a build of its own.
pub(super) fn channel_thumb_job(ctx: &egui::Context, id: DocId, doc: &std::sync::Arc<Document>, show_color: bool) -> Option<ChannelImages> {
    use std::sync::{Arc, Mutex, PoisonError};
    let key = channel_thumb_job_id();
    if let Some(job) = ctx.data(|d| d.get_temp::<ChannelThumbJob>(key)) {
        let same_doc = job.doc == id && job.show_color == show_color;
        let matches = same_doc && (job.snapshot.ptr_eq(&Arc::downgrade(doc)) || job.snapshot.upgrade().is_some_and(|prev| same_but_selection(&prev, doc)));
        let done = job.result.lock().unwrap_or_else(PoisonError::into_inner).take();
        match (matches, done) {
            (true, Some(images)) => {
                ctx.data_mut(|d| d.remove::<ChannelThumbJob>(key));
                return Some(images);
            }
            // Still running (for this document as it is now, or as it was): wait for it.
            (_, None) if same_doc => return None,
            // Done for an older state, or another document: build again below.
            _ => {}
        }
    }
    let result = Arc::new(Mutex::new(None));
    let (out, snap) = (result.clone(), doc.clone());
    let spawned = std::thread::Builder::new().name("channel-thumbs".into()).spawn(move || {
        let images = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| channel_images(&snap, show_color))).unwrap_or_default();
        *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(images);
    });
    if spawned.is_err() {
        return Some(channel_images(doc, show_color));
    }
    ctx.data_mut(|d| d.insert_temp(key, ChannelThumbJob { doc: id, snapshot: Arc::downgrade(doc), show_color, result }));
    None
}

/// Layer › Layer Mask commands that add a layer mask: Photoshop targets the new mask (#2166).
const ADDS_LAYER_MASK: [&str; 5] = [
    "layer.layerMask.revealAll",
    "layer.layerMask.hideAll",
    "layer.layerMask.revealSelection",
    "layer.layerMask.hideSelection",
    "layer.layerMask.fromTransparency",
];

/// The masks an action gave the active layer ([`PhotocraftApp::masks_gained`]).
#[derive(Default)]
struct MasksGained {
    mask: bool,
    second: bool,
    vector: bool,
    /// The first effect that gained its own mask.
    effect: Option<usize>,
}
