//! egui panels: menu bar, scene tree, material inspector, view settings, status bar.

use std::path::Path;

use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText};

use crate::{
    renderer::{FrameStats, GpuInfo, ViewMode, ViewSettings},
    scene::{AlphaMode, OrbitCamera, Scene, UpAxis},
};

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    OpenDialog,
    CloseModel,
    SaveScreenshot,
    FitView,
    ResetView,
    /// Bounds changed (visibility, up axis): refit the camera.
    SceneChanged,
    Quit,
}

pub struct UiState {
    pub selected_node: Option<usize>,
    pub show_scene_panel: bool,
    pub show_inspector: bool,
    pub show_help: bool,
    pub error: Option<String>,
    pub vsync: bool,
    pub continuous: bool,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            selected_node: None,
            show_scene_panel: true,
            show_inspector: true,
            show_help: false,
            error: None,
            vsync: true,
            continuous: false,
        }
    }
}

/// Smoothed frame timings for display.
#[derive(Default)]
pub struct Timings {
    pub fps: f32,
    pub cpu_ms: f32,
    pub stats: FrameStats,
}

pub struct UiContext<'a> {
    pub state: &'a mut UiState,
    pub scene: Option<&'a mut Scene>,
    pub settings: &'a mut ViewSettings,
    pub camera: &'a mut OrbitCamera,
    pub timings: &'a Timings,
    pub gpu: &'a GpuInfo,
    pub loading: Option<&'a Path>,
}

pub struct UiOutput {
    pub actions: Vec<UiAction>,
    /// 3D viewport in points.
    pub viewport: egui::Rect,
}

pub fn draw(ui: &mut egui::Ui, cx: UiContext) -> UiOutput {
    let UiContext {
        state,
        mut scene,
        settings,
        camera,
        timings,
        gpu,
        loading,
    } = cx;
    let mut actions = Vec::new();
    shortcuts(ui.ctx(), settings, state, &mut actions);

    let has_model = scene.as_ref().is_some_and(|s| s.source.is_some());
    egui::Panel::top("menu").show(ui, |ui| {
        egui::MenuBar::new().ui(ui, |ui| menu_bar(ui, has_model, state, &mut actions));
    });

    egui::Panel::bottom("status").show(ui, |ui| {
        status_bar(ui, scene.as_deref(), timings, gpu, state)
    });

    if state.show_scene_panel {
        egui::Panel::left("scene")
            .resizable(true)
            .default_size(270.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    scene_panel(ui, scene.as_deref_mut(), gpu, state, &mut actions);
                });
            });
    }
    if state.show_inspector {
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(290.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    view_settings(ui, settings, gpu, &mut actions);
                    ui.separator();
                    inspector(ui, scene.as_deref_mut(), state);
                });
            });
    }

    let viewport = egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            let rect = ui.max_rect();
            viewport_input(ui, rect, camera);
            if let Some(path) = loading {
                overlay(ui, rect, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Loading {}…", file_name(path)));
                    });
                });
            } else if scene.as_ref().is_some_and(|s| s.source.is_none()) {
                hint(
                    ui,
                    rect,
                    "Open a model with Ctrl+O, or drop a .gltf / .glb / .obj / .stl file here",
                );
            }
            rect
        })
        .inner;

    if let Some(error) = &state.error {
        let mut open = true;
        let mut dismissed = false;
        egui::Window::new("Could not open model")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(RichText::new(error).color(Color32::from_rgb(255, 140, 120)));
                ui.add_space(6.0);
                dismissed = ui.button("OK").clicked();
            });
        if !open || dismissed {
            state.error = None;
        }
    }

    egui::Window::new("Controls")
        .open(&mut state.show_help)
        .resizable(false)
        .collapsible(false)
        .show(ui.ctx(), help);

    UiOutput { actions, viewport }
}

fn shortcuts(
    ctx: &egui::Context,
    settings: &mut ViewSettings,
    state: &mut UiState,
    actions: &mut Vec<UiAction>,
) {
    let open = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
    let quit = KeyboardShortcut::new(Modifiers::COMMAND, Key::Q);
    ctx.input_mut(|i| {
        if i.consume_shortcut(&open) {
            actions.push(UiAction::OpenDialog);
        }
        if i.consume_shortcut(&quit) {
            actions.push(UiAction::Quit);
        }
    });
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    ctx.input(|i| {
        let plain = i.modifiers.is_none();
        let pressed = |k| plain && i.key_pressed(k);
        if pressed(Key::F) {
            actions.push(UiAction::FitView);
        }
        if pressed(Key::R) {
            actions.push(UiAction::ResetView);
        }
        if pressed(Key::G) {
            settings.show_grid ^= true;
        }
        if pressed(Key::B) {
            settings.show_bounds ^= true;
        }
        for (key, mode) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4]
            .into_iter()
            .zip(ViewMode::ALL)
        {
            if pressed(key) {
                settings.mode = mode;
            }
        }
        if pressed(Key::Escape) {
            state.selected_node = None;
        }
        if i.key_pressed(Key::F1) {
            state.show_help ^= true;
        }
    });
}

fn menu_bar(ui: &mut egui::Ui, has_model: bool, state: &mut UiState, actions: &mut Vec<UiAction>) {
    ui.menu_button("File", |ui| {
        if ui
            .add(
                egui::Button::new("Open…").shortcut_text(
                    ui.ctx()
                        .format_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::O)),
                ),
            )
            .clicked()
        {
            actions.push(UiAction::OpenDialog);
            ui.close();
        }
        if ui
            .add_enabled(has_model, egui::Button::new("Close model"))
            .clicked()
        {
            actions.push(UiAction::CloseModel);
            ui.close();
        }
        if ui.button("Save screenshot…").clicked() {
            actions.push(UiAction::SaveScreenshot);
            ui.close();
        }
        ui.separator();
        if ui.button("Quit").clicked() {
            actions.push(UiAction::Quit);
            ui.close();
        }
    });
    ui.menu_button("View", |ui| {
        if ui
            .add(egui::Button::new("Fit to model").shortcut_text("F"))
            .clicked()
        {
            actions.push(UiAction::FitView);
            ui.close();
        }
        if ui
            .add(egui::Button::new("Reset camera").shortcut_text("R"))
            .clicked()
        {
            actions.push(UiAction::ResetView);
            ui.close();
        }
    });
    ui.menu_button("Window", |ui| {
        ui.checkbox(&mut state.show_scene_panel, "Scene panel");
        ui.checkbox(&mut state.show_inspector, "Inspector");
    });
    ui.menu_button("Help", |ui| {
        if ui
            .add(egui::Button::new("Controls").shortcut_text("F1"))
            .clicked()
        {
            state.show_help = true;
            ui.close();
        }
    });
}

fn status_bar(
    ui: &mut egui::Ui,
    scene: Option<&Scene>,
    t: &Timings,
    gpu: &GpuInfo,
    state: &mut UiState,
) {
    ui.horizontal(|ui| {
        ui.label(format!("{:.0} fps", t.fps));
        ui.separator();
        ui.label(format!("CPU {:.2} ms", t.cpu_ms))
            .on_hover_text("CPU time per frame, excluding time spent waiting for the GPU or vsync");
        if let Some(gpu_ms) = t.stats.gpu_ms {
            ui.label(format!("GPU {gpu_ms:.2} ms"));
        }
        ui.separator();
        ui.label(format!("{} draws", t.stats.draw_calls));
        ui.label(format!("{} tris", format_count(t.stats.triangles as usize)));
        if t.stats.culled > 0 {
            ui.label(format!("{} culled", t.stats.culled));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.checkbox(&mut state.continuous, "Continuous")
                .on_hover_text("Redraw every frame (for profiling) instead of only on changes");
            ui.checkbox(&mut state.vsync, "VSync");
            ui.separator();
            ui.label(RichText::new(format!("{} · {}x MSAA", gpu.name, gpu.msaa_samples)).weak());
            if let Some(scene) = scene {
                ui.separator();
                ui.label(RichText::new(&scene.name).strong());
            }
        });
    });
}

fn scene_panel(
    ui: &mut egui::Ui,
    scene: Option<&mut Scene>,
    gpu: &GpuInfo,
    state: &mut UiState,
    actions: &mut Vec<UiAction>,
) {
    ui.heading("Scene");
    let Some(scene) = scene else {
        ui.label("No model loaded.");
        return;
    };
    let stats = scene.stats();
    egui::Grid::new("scene_stats")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            let mut row = |k: &str, v: String| {
                ui.label(k);
                ui.label(v);
                ui.end_row();
            };
            if let Some(path) = &scene.source {
                row("File", file_name(path));
                row("Load time", format!("{:.0?}", scene.load_time));
            }
            row("Nodes", stats.nodes.to_string());
            row("Meshes", stats.meshes.to_string());
            row("Primitives", stats.primitives.to_string());
            row("Materials", scene.materials.len().to_string());
            row("Textures", scene.textures.len().to_string());
            row("Vertices", format_count(stats.vertices));
            row("Triangles", format_count(stats.triangles));
            row(
                "GPU memory",
                format!(
                    "{:.1} MiB",
                    gpu.scene_memory_bytes as f64 / (1024.0 * 1024.0)
                ),
            );
            let size = scene.bounds().size();
            row(
                "Size",
                format!("{:.3} × {:.3} × {:.3}", size.x, size.y, size.z),
            );
        });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Up axis");
        let before = scene.up_axis;
        ui.selectable_value(&mut scene.up_axis, UpAxis::Y, "Y");
        ui.selectable_value(&mut scene.up_axis, UpAxis::Z, "Z");
        if scene.up_axis != before {
            actions.push(UiAction::SceneChanged);
        }
    });

    ui.separator();
    ui.horizontal(|ui| {
        ui.strong("Nodes");
        if ui.small_button("Show all").clicked() {
            scene.nodes.iter_mut().for_each(|n| n.visible = true);
            actions.push(UiAction::SceneChanged);
        }
    });
    let row_height = ui.spacing().interact_size.y;
    let total = scene.nodes.len();
    egui::ScrollArea::vertical()
        .id_salt("nodes")
        .max_height(ui.available_height().max(200.0))
        .show_rows(ui, row_height, total, |ui, range| {
            for i in range {
                let node = &mut scene.nodes[i];
                ui.horizontal(|ui| {
                    ui.add_space(node.depth as f32 * 10.0);
                    if ui.checkbox(&mut node.visible, "").changed() {
                        actions.push(UiAction::SceneChanged);
                    }
                    let selected = state.selected_node == Some(i);
                    if ui.selectable_label(selected, &node.name).clicked() {
                        state.selected_node = if selected { None } else { Some(i) };
                    }
                });
            }
        });
}

fn view_settings(
    ui: &mut egui::Ui,
    s: &mut ViewSettings,
    gpu: &GpuInfo,
    actions: &mut Vec<UiAction>,
) {
    ui.heading("View");
    for (i, mode) in ViewMode::ALL.into_iter().enumerate() {
        let enabled = gpu.wireframe || !mode.needs_wireframe();
        ui.add_enabled_ui(enabled, |ui| {
            ui.radio_value(&mut s.mode, mode, format!("{}  ({})", mode.label(), i + 1))
                .on_disabled_hover_text("This GPU does not support wireframe rendering");
        });
    }
    ui.horizontal(|ui| {
        ui.checkbox(&mut s.show_grid, "Grid");
        ui.checkbox(&mut s.show_bounds, "Bounds");
        ui.checkbox(&mut s.frustum_culling, "Culling");
    });
    ui.horizontal(|ui| {
        if ui.button("Fit (F)").clicked() {
            actions.push(UiAction::FitView);
        }
        if ui.button("Reset (R)").clicked() {
            actions.push(UiAction::ResetView);
        }
    });

    egui::CollapsingHeader::new("Lighting")
        .default_open(false)
        .show(ui, |ui| {
            ui.add(egui::Slider::new(&mut s.light_intensity, 0.0..=10.0).text("Light"));
            ui.add(egui::Slider::new(&mut s.ambient, 0.0..=2.0).text("Ambient"));
            ui.add(egui::Slider::new(&mut s.light_azimuth, -180.0..=180.0).text("Azimuth"));
            ui.add(egui::Slider::new(&mut s.light_elevation, -90.0..=90.0).text("Elevation"));
            ui.horizontal(|ui| {
                ui.label("Background");
                ui.color_edit_button_srgb(&mut s.background);
            });
        });
}

fn inspector(ui: &mut egui::Ui, scene: Option<&mut Scene>, state: &UiState) {
    ui.heading("Inspector");
    let (Some(scene), Some(index)) = (scene, state.selected_node) else {
        ui.label(RichText::new("Select a node in the scene panel.").weak());
        return;
    };
    let Some(node) = scene.nodes.get(index) else {
        return;
    };
    let (scale, rotation, translation) = node.transform.to_scale_rotation_translation();
    let (ax, ay, az) = rotation.to_euler(glam::EulerRot::XYZ);
    let mesh_index = node.mesh;
    ui.strong(&node.name);
    egui::Grid::new("node_transform")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Translation");
            ui.label(format!(
                "{:.3}, {:.3}, {:.3}",
                translation.x, translation.y, translation.z
            ));
            ui.end_row();
            ui.label("Rotation");
            ui.label(format!(
                "{:.1}°, {:.1}°, {:.1}°",
                ax.to_degrees(),
                ay.to_degrees(),
                az.to_degrees()
            ));
            ui.end_row();
            ui.label("Scale");
            ui.label(format!("{:.3}, {:.3}, {:.3}", scale.x, scale.y, scale.z));
            ui.end_row();
        });

    let mesh = &scene.meshes[mesh_index];
    ui.label(format!(
        "Mesh: {} ({} primitives)",
        mesh.name,
        mesh.primitives.len()
    ));
    let materials: Vec<(usize, usize, usize)> = mesh
        .primitives
        .iter()
        .map(|p| (p.material, p.vertex_count, p.triangle_count()))
        .collect();

    for (pi, (material_index, vertices, triangles)) in materials.into_iter().enumerate() {
        let textures = &scene.textures;
        let m = &mut scene.materials[material_index];
        egui::CollapsingHeader::new(format!("Primitive {pi}: {}", m.name))
            .id_salt(("prim", index, pi))
            .default_open(pi == 0)
            .show(ui, |ui| {
                ui.label(format!(
                    "{} vertices, {} triangles",
                    format_count(vertices),
                    format_count(triangles)
                ));
                ui.horizontal(|ui| {
                    ui.label("Base color");
                    ui.color_edit_button_rgba_unmultiplied(&mut m.base_color);
                });
                ui.add(egui::Slider::new(&mut m.metallic, 0.0..=1.0).text("Metallic"));
                ui.add(egui::Slider::new(&mut m.roughness, 0.0..=1.0).text("Roughness"));
                ui.label(format!(
                    "Alpha: {}",
                    match m.alpha_mode {
                        AlphaMode::Opaque => "opaque".to_string(),
                        AlphaMode::Mask(c) => format!("mask ({c:.2})"),
                        AlphaMode::Blend => "blend".to_string(),
                    }
                ));
                for (label, tex) in [
                    ("Base color map", m.base_color_texture),
                    ("Metal/rough map", m.metallic_roughness_texture),
                    ("Emissive map", m.emissive_texture),
                ] {
                    if let Some(t) = tex.and_then(|i| textures.get(i)) {
                        ui.label(format!("{label}: {} ({}×{})", t.name, t.width, t.height));
                    }
                }
            });
    }
}

/// Camera control over the 3D viewport: drag to orbit, right/middle-drag or
/// shift-drag to pan, scroll to zoom, double-click to fit.
fn viewport_input(ui: &mut egui::Ui, rect: egui::Rect, camera: &mut OrbitCamera) {
    let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
    let delta = response.drag_delta();
    let shift = ui.input(|i| i.modifiers.shift);
    if response.dragged_by(egui::PointerButton::Primary) && !shift {
        camera.orbit(delta.x, delta.y);
    } else if response.dragged_by(egui::PointerButton::Secondary)
        || response.dragged_by(egui::PointerButton::Middle)
        || (response.dragged_by(egui::PointerButton::Primary) && shift)
    {
        camera.pan(delta.x, delta.y, rect.height());
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            camera.zoom(scroll / 60.0);
        }
    }
    if response.double_clicked() {
        ui.ctx()
            .data_mut(|d| d.insert_temp(egui::Id::new("fit_request"), true));
    }
}

/// Whether the viewport was double-clicked this frame.
pub fn take_fit_request(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::new("fit_request")))
        .is_some()
}

fn overlay(ui: &mut egui::Ui, rect: egui::Rect, add: impl FnOnce(&mut egui::Ui)) {
    egui::Area::new(egui::Id::new("viewport_overlay"))
        .fixed_pos(rect.center_top() + egui::vec2(0.0, 16.0))
        .pivot(egui::Align2::CENTER_TOP)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, add);
        });
}

fn hint(ui: &mut egui::Ui, rect: egui::Rect, text: &str) {
    ui.painter().text(
        rect.center_bottom() - egui::vec2(0.0, 24.0),
        egui::Align2::CENTER_BOTTOM,
        text,
        egui::FontId::proportional(15.0),
        Color32::from_white_alpha(140),
    );
}

fn help(ui: &mut egui::Ui) {
    egui::Grid::new("help")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for (k, v) in [
                ("Left drag", "Orbit"),
                ("Right / middle drag, Shift + left drag", "Pan"),
                ("Scroll", "Zoom"),
                ("Double-click, F", "Fit to model"),
                ("R", "Reset camera"),
                ("1 – 4", "Shaded / shaded + wire / wireframe / normals"),
                ("G / B", "Toggle grid / bounds"),
                ("Ctrl+O", "Open model"),
                ("Esc", "Clear selection"),
                ("Drag & drop", "Open a .gltf, .glb, .obj or .stl file"),
            ] {
                ui.strong(k);
                ui.label(v);
                ui.end_row();
            }
        });
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn format_count(n: usize) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}K", n as f32 / 1e3),
        _ => format!("{:.2}M", n as f32 / 1e6),
    }
}
