use super::{PhotocraftApp, gpu_canvas, gpu_status};

impl PhotocraftApp {
    /// Draw the document canvas on the GPU (custom WGSL shader) instead of via egui textures.
    /// Call from the app creator with `cc.wgpu_render_state`; without it the CPU path is used.
    pub fn set_wgpu(&mut self, rs: eframe::egui_wgpu::RenderState) {
        // Preferences › Performance › cache tile size (PHOTOCRAFT_GPU_TILE still overrides).
        let tile = self.session.prefs().performance.cache_tile_size;
        // Escaped driver/setup panics must leave the session and CPU canvas alive.
        self.perf.gpu_info.set_adapter(&rs.adapter.get_info());
        // The browser has no adapter list (egui-wgpu only has it natively).
        #[cfg(not(target_arch = "wasm32"))]
        self.perf.gpu_info.set_available(&rs.available_adapters);
        self.gpu_render_state = Some(rs.clone());
        let gpu = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| gpu_canvas::GpuCanvas::with_tile(&rs, Some(tile)))) {
            Ok(gpu) => gpu,
            Err(payload) => {
                // The window still draws on this device: watch it for a loss (see `gpu_status`).
                self.watch_window_render_device(&rs.device);
                let detail = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_else(|| "GPU canvas initialization failed".into());
                self.perf.gpu_info.canvas = "cpu".into();
                self.perf.gpu_info.fallback = Some(detail.clone());
                gpu_status::queue_fallback_notice(self, detail);
                return;
            }
        };
        self.perf.gpu_info.canvas = "gpu".into();
        // The canvas shares eframe's device, so its health flag is the window's too.
        self.window_gpu.device = Some(rs.device.clone());
        self.watch_window_device(gpu.health().clone());
        self.gpu = Some(gpu);
        self.prefs_rt.gpu_style = None;
    }

    /// Start building the GPU canvas's layer compositor on a thread ([`gpu_canvas::GpuCanvas::warm_up`]),
    /// so the first document opened doesn't wait seconds for its shaders: the desktop app calls it
    /// after [`Self::set_wgpu`].
    pub fn warm_up_gpu(&self) {
        if let Some(g) = &self.gpu {
            g.warm_up();
        }
    }

    /// Whether the document canvas currently draws on the GPU (false on the CPU path, and after
    /// the device was lost).
    pub fn gpu_active(&self) -> bool {
        self.gpu.is_some()
    }

    /// The GPU canvas's device health flag (tests inject faults through it).
    pub fn gpu_health(&self) -> Option<photocraft_gpu::DeviceHealth> {
        self.gpu.as_ref().map(|g| g.health().clone())
    }

    /// Watch the device eframe draws the window with (`cc.wgpu_render_state`): once it is lost,
    /// the window stops drawing and PhotoCraft restarts (see `gpu_status`). [`Self::set_wgpu`]
    /// does this itself; a CPU canvas passes `DeviceHealth::watch(&render_state.device)`.
    pub fn watch_window_device(&mut self, health: photocraft_gpu::DeviceHealth) {
        self.window_gpu.watch(health);
    }

    /// The window device's health flag, once watched (tests inject a loss through it; the shell
    /// reads it after a panic).
    pub fn window_device_health(&self) -> Option<photocraft_gpu::DeviceHealth> {
        self.window_gpu.health()
    }

    /// Watch the device the window draws with when the GPU canvas doesn't (the CPU canvas, or a
    /// GPU canvas that failed to start): its loss is handled like the GPU canvas's.
    pub fn watch_window_render_device(&mut self, device: &eframe::wgpu::Device) {
        self.window_gpu.device = Some(device.clone());
        self.watch_window_device(gpu_canvas::DeviceHealth::watch(device));
    }

    /// Whether the window's graphics device was lost: the app draws nothing more and restarts
    /// once unsaved work is in crash recovery (see `gpu_status`).
    pub fn window_lost(&self) -> bool {
        gpu_status::window_lost(self)
    }

    /// Destroy the window's graphics device, as a driver reset does (`ui.gpu.simulateLoss`
    /// with `destroy`, tests). Returns whether there was one.
    pub fn destroy_window_device(&mut self) -> bool {
        self.window_gpu.device.as_ref().map(eframe::wgpu::Device::destroy).is_some()
    }

    /// Run `hook` once the app has rendered its first frames (and a document opened at launch
    /// has drawn): the desktop app clears its crash-safe GPU startup marker there.
    pub fn on_started(&mut self, hook: impl FnOnce(&mut PhotocraftApp) + 'static) {
        self.started = Some(Box::new(hook));
    }

    /// Per-frame GPU health check (called from `logic`; tests call it directly): falls back to
    /// the CPU canvas after a device loss and runs the started hook.
    pub fn check_gpu(&mut self, ctx: &egui::Context) {
        gpu_status::check(self, ctx);
    }
}
