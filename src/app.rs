use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy},
    window::{Window, WindowId},
};

use crate::{
    assets,
    renderer::{EguiFrame, FrameInput, Renderer, ViewSettings},
    scene::{demo::demo_scene, OrbitCamera, Scene},
    ui::{self, Timings, UiAction, UiContext, UiState},
};

pub enum UserEvent {
    /// A background load finished.
    Loaded {
        path: PathBuf,
        result: Result<Scene, String>,
    },
}

#[derive(Debug, Default)]
pub struct Options {
    pub model: Option<PathBuf>,
    /// Save the first fully loaded frame to this PNG, then exit.
    pub screenshot: Option<PathBuf>,
}

pub struct App {
    proxy: EventLoopProxy<UserEvent>,
    options: Options,
    viewer: Option<Viewer>,
    pub fatal_error: Option<anyhow::Error>,
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>, options: Options) -> Self {
        App {
            proxy,
            options,
            viewer: None,
            fatal_error: None,
        }
    }
}

/// A screenshot waiting to be taken.
struct PendingCapture {
    path: PathBuf,
    /// Keep only the 3D viewport (menu action) or the whole window (CLI).
    crop: Option<[u32; 4]>,
    exit_after: bool,
    requested: bool,
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.viewer.is_some() {
            return;
        }
        match Viewer::new(event_loop, self.proxy.clone()) {
            Ok(mut viewer) => {
                if let Some(path) = self.options.model.take() {
                    viewer.open(path);
                }
                if let Some(path) = self.options.screenshot.take() {
                    viewer.capture = Some(PendingCapture {
                        path,
                        crop: None,
                        exit_after: true,
                        requested: false,
                    });
                }
                self.viewer = Some(viewer);
            }
            Err(e) => {
                self.fatal_error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let Some(viewer) = &mut self.viewer {
            if let Err(e) = viewer.window_event(event_loop, event) {
                self.fatal_error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        if let Some(viewer) = &mut self.viewer {
            viewer.user_event(event);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(viewer) = &mut self.viewer {
            viewer.schedule(event_loop);
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Tear down Vulkan before the window goes away.
        self.viewer.take();
    }
}

struct Viewer {
    // The renderer owns the window surface, so it must drop before the window.
    renderer: Renderer,
    window: Arc<Window>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    proxy: EventLoopProxy<UserEvent>,

    scene: Option<Scene>,
    camera: OrbitCamera,
    settings: ViewSettings,
    ui: UiState,
    loading: Option<PathBuf>,
    /// Width / height of the 3D viewport, kept for fit/reset outside a frame.
    viewport_aspect: f32,

    timings: Timings,
    last_frame: Instant,
    repaint_at: Option<Instant>,
    frames_rendered: u64,
    capture: Option<PendingCapture>,
}

impl Viewer {
    fn new(event_loop: &ActiveEventLoop, proxy: EventLoopProxy<UserEvent>) -> Result<Self> {
        let attributes = Window::default_attributes()
            .with_title("3D Viewer")
            .with_inner_size(winit::dpi::LogicalSize::new(1400.0, 860.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(480.0, 320.0));
        // Wayland app_id / X11 WM_CLASS, matched against viewer3d.desktop so the
        // desktop shows the packaged icon for the window.
        #[cfg(target_os = "linux")]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes.with_name("viewer3d", "viewer3d")
        };
        let window = Arc::new(event_loop.create_window(attributes)?);

        let ui = UiState::default();
        let renderer = Renderer::new(&window, ui.vsync)?;

        let egui_ctx = egui::Context::default();
        egui_ctx.set_visuals(egui::Visuals::dark());
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(renderer.max_texture_side()),
        );

        let mut viewer = Viewer {
            renderer,
            window,
            egui_ctx,
            egui_state,
            proxy,
            scene: None,
            camera: OrbitCamera::default(),
            settings: ViewSettings::default(),
            ui,
            loading: None,
            viewport_aspect: 1.6,
            timings: Timings::default(),
            last_frame: Instant::now(),
            repaint_at: None,
            frames_rendered: 0,
            capture: None,
        };
        viewer.set_scene(demo_scene())?;
        Ok(viewer)
    }

    /// Parse the file on a worker thread; the result arrives as a `UserEvent`.
    fn open(&mut self, path: PathBuf) {
        if self.loading.is_some() {
            return;
        }
        if !assets::is_supported(&path) {
            self.ui.error = Some(format!(
                "{}: unsupported file type.\nSupported formats: {}",
                path.display(),
                assets::SUPPORTED_EXTENSIONS.join(", ")
            ));
            self.window.request_redraw();
            return;
        }
        self.loading = Some(path.clone());
        let proxy = self.proxy.clone();
        std::thread::Builder::new()
            .name("model loader".into())
            .spawn(move || {
                let result = assets::load(&path).map_err(|e| format!("{e:#}"));
                proxy.send_event(UserEvent::Loaded { path, result }).ok();
            })
            .expect("failed to spawn loader thread");
        self.window.request_redraw();
    }

    fn open_dialog(&mut self) {
        if self.loading.is_some() {
            return;
        }
        let mut dialog = rfd::FileDialog::new()
            .set_title("Open 3D model")
            .add_filter("3D models", assets::SUPPORTED_EXTENSIONS)
            .add_filter("All files", &["*"]);
        if let Some(dir) = self
            .scene
            .as_ref()
            .and_then(|s| s.source.as_ref())
            .and_then(|p| p.parent())
        {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open(path);
        }
    }

    fn save_screenshot_dialog(&mut self, viewport_px: [f32; 4]) {
        let name = self
            .scene
            .as_ref()
            .and_then(|s| s.source.as_ref())
            .and_then(|p| p.file_stem())
            .map_or("screenshot".into(), |s| s.to_string_lossy().into_owned());
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Save screenshot")
            .add_filter("PNG image", &["png"])
            .set_file_name(format!("{name}.png"))
            .save_file()
        {
            self.capture = Some(PendingCapture {
                path: path.with_extension("png"),
                crop: Some(viewport_px.map(|v| v.max(0.0) as u32)),
                exit_after: false,
                requested: false,
            });
            self.window.request_redraw();
        }
    }

    fn save_capture(
        &mut self,
        image: Result<image::RgbaImage, String>,
        event_loop: &ActiveEventLoop,
    ) {
        let Some(capture) = self.capture.take() else {
            return;
        };
        let image = match image {
            Ok(image) => image,
            Err(e) => {
                tracing::error!("screenshot failed: {e}");
                self.ui.error = Some(format!("Screenshot failed: {e}"));
                if capture.exit_after {
                    event_loop.exit();
                }
                return;
            }
        };
        let image = match capture.crop {
            Some([x, y, w, h]) => {
                let w = w.min(image.width().saturating_sub(x)).max(1);
                let h = h.min(image.height().saturating_sub(y)).max(1);
                image::imageops::crop_imm(&image, x, y, w, h).to_image()
            }
            None => image,
        };
        match image.save(&capture.path) {
            Ok(()) => tracing::info!("saved screenshot to {}", capture.path.display()),
            Err(e) => {
                let msg = format!("could not save {}: {e}", capture.path.display());
                tracing::error!("{msg}");
                self.ui.error = Some(msg);
            }
        }
        if capture.exit_after {
            event_loop.exit();
        }
    }

    /// Upload a scene to the GPU and make it current. CPU geometry is released
    /// afterwards; only counts and bounds are kept.
    fn set_scene(&mut self, mut scene: Scene) -> Result<()> {
        self.renderer.upload_scene(&scene)?;
        for p in scene.meshes.iter_mut().flat_map(|m| &mut m.primitives) {
            p.release_geometry();
        }
        self.camera.reset(&scene.bounds(), self.viewport_aspect);
        self.window.set_title(&match &scene.source {
            Some(_) => format!("{} — 3D Viewer", scene.name),
            None => "3D Viewer".into(),
        });
        self.ui.selected_node = None;
        self.scene = Some(scene);
        self.frames_rendered = 0;
        Ok(())
    }

    fn user_event(&mut self, event: UserEvent) {
        match event {
            UserEvent::Loaded { path, result } => {
                self.loading = None;
                let result = result.and_then(|scene| {
                    self.set_scene(scene)
                        .map_err(|e| format!("{}: GPU upload failed: {e:#}", path.display()))
                });
                if let Err(e) = result {
                    tracing::error!("{e}");
                    self.ui.error = Some(e);
                }
                self.window.request_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) -> Result<()> {
        let response = self.egui_state.on_window_event(&self.window, &event);
        if response.repaint {
            self.window.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.renderer.request_resize();
                self.window.request_redraw();
            }
            WindowEvent::DroppedFile(path) => self.open(path),
            WindowEvent::RedrawRequested => self.frame(event_loop)?,
            _ => {}
        }
        Ok(())
    }

    fn schedule(&mut self, event_loop: &ActiveEventLoop) {
        match self.repaint_at {
            Some(t) if t <= Instant::now() => {
                self.repaint_at = None;
                self.window.request_redraw();
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(t) => event_loop.set_control_flow(ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn frame(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let start = Instant::now();
        let raw_input = self.egui_state.take_egui_input(&self.window);

        let gpu_info = self.renderer.info();
        let ctx = self.egui_ctx.clone();
        let mut output = None;
        let full_output = ctx.run_ui(raw_input, |ui| {
            output = Some(ui::draw(
                ui,
                UiContext {
                    state: &mut self.ui,
                    scene: self.scene.as_mut(),
                    settings: &mut self.settings,
                    camera: &mut self.camera,
                    timings: &self.timings,
                    gpu: &gpu_info,
                    loading: self.loading.as_deref(),
                },
            ));
        });
        self.egui_state
            .handle_platform_output(&self.window, full_output.platform_output);
        let output = output.expect("ui ran");

        let ppp = full_output.pixels_per_point;
        let viewport = output.viewport;
        self.viewport_aspect = (viewport.width() / viewport.height().max(1.0)).max(0.01);

        let mut actions = output.actions;
        if ui::take_fit_request(&ctx) {
            actions.push(UiAction::FitView);
        }
        for action in actions {
            match action {
                UiAction::OpenDialog => self.open_dialog(),
                UiAction::CloseModel => self.set_scene(demo_scene())?,
                UiAction::SaveScreenshot => self.save_screenshot_dialog([
                    viewport.min.x * ppp,
                    viewport.min.y * ppp,
                    viewport.width() * ppp,
                    viewport.height() * ppp,
                ]),
                UiAction::FitView => {
                    if let Some(s) = &self.scene {
                        self.camera.fit(&s.bounds(), self.viewport_aspect);
                    }
                }
                UiAction::ResetView | UiAction::SceneChanged => {
                    if let Some(s) = &self.scene {
                        self.camera.reset(&s.bounds(), self.viewport_aspect);
                    }
                }
                UiAction::Quit => event_loop.exit(),
            }
        }
        self.renderer.set_vsync(self.ui.vsync);

        // Capture once loading is done and the UI has had a few frames to settle.
        if let Some(capture) = &mut self.capture {
            if !capture.requested && self.loading.is_none() && self.frames_rendered >= 3 {
                capture.requested = true;
                self.renderer.request_capture();
            }
        }

        let primitives = ctx.tessellate(full_output.shapes, ppp);
        let size = self.window.inner_size();
        let stats = self.renderer.render(
            (size.width, size.height),
            FrameInput {
                scene: self.scene.as_ref(),
                camera: &self.camera,
                settings: &self.settings,
                selected_node: self.ui.selected_node,
                viewport: [
                    viewport.min.x * ppp,
                    viewport.min.y * ppp,
                    viewport.width() * ppp,
                    viewport.height() * ppp,
                ],
                egui: EguiFrame {
                    primitives,
                    textures_delta: full_output.textures_delta,
                    pixels_per_point: ppp,
                },
            },
        )?;

        // Exponential moving averages keep the readout steady.
        let now = Instant::now();
        let interval = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;
        let wait_ms = stats.map_or(0.0, |s| s.wait_ms);
        let cpu_ms = (now.duration_since(start).as_secs_f32() * 1e3 - wait_ms).max(0.0);
        let t = &mut self.timings;
        t.cpu_ms = if t.cpu_ms == 0.0 {
            cpu_ms
        } else {
            t.cpu_ms * 0.9 + cpu_ms * 0.1
        };
        if interval > 0.0 {
            t.fps = if t.fps == 0.0 {
                1.0 / interval
            } else {
                t.fps * 0.9 + 0.1 / interval
            };
        }
        match stats {
            Some(stats) => {
                t.stats = stats;
                self.frames_rendered += 1;
            }
            // Skipped frame (swapchain rebuilt): try again right away.
            None => self.window.request_redraw(),
        }
        if let Some(image) = self.renderer.take_capture() {
            self.save_capture(image, event_loop);
        } else if let Some(c) = &mut self.capture {
            // The capture didn't happen (skipped frame); request it again.
            if c.requested && stats.is_none() {
                c.requested = false;
            }
            self.window.request_redraw();
        }

        let repaint_delay = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |v| v.repaint_delay);
        if self.ui.continuous || self.loading.is_some() || repaint_delay.is_zero() {
            self.window.request_redraw();
        } else if repaint_delay < Duration::from_secs(3600) {
            self.repaint_at = Some(Instant::now() + repaint_delay);
        }
        Ok(())
    }
}
