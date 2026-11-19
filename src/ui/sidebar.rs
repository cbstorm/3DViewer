//! Blender-style viewport sidebar (the "N panel"): a translucent region docked
//! inside the right edge of the 3D viewport, with vertical tabs along its outer
//! edge and collapsible sub-panels.
//!
//! - `N` toggles it, the small arrow at the viewport edge re-opens it.
//! - Drag its inner edge to resize; drag it far enough to collapse it.
//! - Ctrl + wheel over the sidebar cycles tabs.
//! - Ctrl + click a panel header to open it and collapse the others in the tab.

use std::f32::consts::FRAC_PI_2;
use std::hash::Hash;

use egui::{
    collapsing_header::{paint_default_icon, CollapsingState},
    pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Event, FontId, Id, Margin, Rect, Sense,
    Shape, Stroke, Ui, UiBuilder,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Item,
    View,
    Light,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Item, Tab::View, Tab::Light];

    fn label(self) -> &'static str {
        match self {
            Tab::Item => "Item",
            Tab::View => "View",
            Tab::Light => "Light",
        }
    }

    fn cycle(self, step: isize) -> Tab {
        let n = Self::ALL.len() as isize;
        let i = Self::ALL.iter().position(|&t| t == self).unwrap() as isize;
        Self::ALL[(i + step).rem_euclid(n) as usize]
    }
}

pub struct Sidebar {
    pub open: bool,
    pub tab: Tab,
    /// Content width in points, excluding the tab strip.
    pub width: f32,
    /// Panel Ctrl+clicked last frame: it stays open, the rest of its tab collapses.
    solo: Option<Id>,
}

impl Default for Sidebar {
    fn default() -> Self {
        Sidebar {
            open: true,
            tab: Tab::View,
            width: 260.0,
            solo: None,
        }
    }
}

const TAB_STRIP: f32 = 22.0;
const MIN_WIDTH: f32 = 180.0;
const MAX_WIDTH: f32 = 640.0;
/// Dragging the edge this far below `MIN_WIDTH` collapses the sidebar.
const COLLAPSE_SLACK: f32 = 90.0;

impl Sidebar {
    /// Draws the sidebar over the right edge of `viewport`. Call after the
    /// viewport's own input handling so the sidebar sits on top of it.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        viewport: Rect,
        add_tab: impl FnOnce(&mut Ui, Tab, &mut Panels),
    ) {
        let id = Id::new("viewport_sidebar");
        let colors = Palette::new(ui.visuals());

        if !self.open {
            self.reopen_handle(ui, id, viewport, &colors);
            return;
        }

        let max_width = (viewport.width() - TAB_STRIP - 80.0).clamp(MIN_WIDTH, MAX_WIDTH);
        self.width = self.width.clamp(MIN_WIDTH, max_width);
        let region = Rect::from_min_max(
            pos2(viewport.right() - TAB_STRIP - self.width, viewport.top()),
            viewport.right_bottom(),
        );
        let (content, strip) = region.split_left_right_at_x(region.right() - TAB_STRIP);

        // Swallow clicks, drags and scrolling so they don't reach the 3D view.
        ui.interact(region, id.with("bg"), Sense::click_and_drag());
        let painter = ui.painter_at(region);
        painter.rect_filled(content, 0.0, colors.region);
        painter.rect_filled(strip, 0.0, colors.strip);

        self.resize_handle(ui, id, region);
        self.tab_strip(ui, id, strip, &colors);
        if ui.rect_contains_pointer(region) {
            let step = ui.input(|i| {
                i.events.iter().fold(0.0, |acc, e| match e {
                    Event::MouseWheel {
                        delta, modifiers, ..
                    } if modifiers.ctrl => acc + delta.y,
                    _ => acc,
                })
            });
            if step != 0.0 {
                self.tab = self.tab.cycle(if step > 0.0 { -1 } else { 1 });
            }
        }

        let mut panels = Panels {
            solo: self.solo.take(),
            clicked: None,
            colors,
        };
        let tab = self.tab;
        ui.scope_builder(
            UiBuilder::new()
                .id_salt(id.with("content"))
                .max_rect(content.shrink2(vec2(6.0, 6.0))),
            |ui| {
                ui.set_clip_rect(content.intersect(ui.clip_rect()));
                egui::ScrollArea::vertical()
                    .id_salt(("sidebar_scroll", tab.label()))
                    .auto_shrink([false, false])
                    .scroll_source(
                        egui::scroll_area::ScrollSource::SCROLL_BAR
                            | egui::scroll_area::ScrollSource::MOUSE_WHEEL,
                    )
                    .show(ui, |ui| add_tab(ui, tab, &mut panels));
            },
        );
        if let Some(clicked) = panels.clicked {
            self.solo = Some(clicked);
            ui.ctx().request_repaint();
        }
    }

    /// The inner (left) edge: drag to resize, or past the minimum to collapse.
    fn resize_handle(&mut self, ui: &mut Ui, id: Id, region: Rect) {
        let rect =
            Rect::from_x_y_ranges(region.left() - 3.0..=region.left() + 3.0, region.y_range());
        let response = ui
            .interact(rect, id.with("resize"), Sense::drag())
            .on_hover_cursor(CursorIcon::ResizeHorizontal);
        let raw_id = id.with("raw_width");
        if response.drag_started() {
            ui.data_mut(|d| d.insert_temp(raw_id, self.width));
        }
        if response.dragged() {
            let raw = ui.data(|d| d.get_temp::<f32>(raw_id)).unwrap_or(self.width)
                - response.drag_delta().x;
            ui.data_mut(|d| d.insert_temp(raw_id, raw));
            if raw < MIN_WIDTH - COLLAPSE_SLACK {
                self.open = false;
            } else {
                self.width = raw;
            }
        }
        if response.hovered() || response.dragged() {
            let stroke = ui.visuals().selection.stroke;
            ui.painter().vline(
                region.left(),
                region.y_range(),
                Stroke::new(2.0, stroke.color),
            );
        }
    }

    fn tab_strip(&mut self, ui: &mut Ui, id: Id, strip: Rect, colors: &Palette) {
        let font = FontId::proportional(13.0);
        let mut y = strip.top() + 6.0;
        for tab in Tab::ALL {
            let text_color = if tab == self.tab {
                ui.visuals().strong_text_color()
            } else {
                ui.visuals().weak_text_color()
            };
            let galley =
                ui.painter()
                    .layout_no_wrap(tab.label().to_owned(), font.clone(), text_color);
            let rect = Rect::from_min_size(
                pos2(strip.left(), y),
                vec2(strip.width() - 3.0, galley.size().x + 16.0),
            );
            y = rect.bottom() + 2.0;

            let response = ui.interact(rect, id.with(("tab", tab.label())), Sense::click());
            if response.clicked() {
                self.tab = tab;
            }
            let fill = if tab == self.tab {
                colors.region
            } else if response.hovered() {
                colors.tab_hover
            } else {
                colors.tab
            };
            let radius = CornerRadius {
                ne: 4,
                se: 4,
                ..Default::default()
            };
            ui.painter().rect_filled(rect, radius, fill);
            // Rotated 90° clockwise so it reads top to bottom, like Blender's tabs.
            let text_pos = pos2(rect.center().x + galley.size().y / 2.0, rect.top() + 8.0);
            ui.painter().add(
                egui::epaint::TextShape::new(text_pos, galley, text_color).with_angle(FRAC_PI_2),
            );
        }
    }

    /// The little arrow at the viewport edge shown while the sidebar is hidden.
    fn reopen_handle(&mut self, ui: &mut Ui, id: Id, viewport: Rect, colors: &Palette) {
        let rect = Rect::from_min_size(
            pos2(viewport.right() - 14.0, viewport.top() + 10.0),
            vec2(14.0, 28.0),
        );
        let response = ui
            .interact(rect, id.with("reopen"), Sense::click())
            .on_hover_text("Sidebar (N)");
        if response.clicked() {
            self.open = true;
        }
        let fill = if response.hovered() {
            colors.tab_hover
        } else {
            colors.strip
        };
        let radius = CornerRadius {
            nw: 4,
            sw: 4,
            ..Default::default()
        };
        ui.painter().rect_filled(rect, radius, fill);
        let c = rect.center();
        ui.painter().add(Shape::convex_polygon(
            vec![c + vec2(2.5, -4.0), c + vec2(2.5, 4.0), c + vec2(-2.5, 0.0)],
            ui.visuals().text_color(),
            Stroke::NONE,
        ));
    }
}

/// Lays out Blender-style collapsible panels inside a sidebar tab.
pub struct Panels {
    solo: Option<Id>,
    clicked: Option<Id>,
    colors: Palette,
}

impl Panels {
    /// A collapsible panel with a header bar. `key` identifies it so its open
    /// state persists; it only needs to be unique within the sidebar.
    pub fn panel(
        &mut self,
        ui: &mut Ui,
        key: impl Hash + std::fmt::Debug,
        title: &str,
        default_open: bool,
        add_body: impl FnOnce(&mut Ui),
    ) {
        let id = Id::new(("sidebar_panel", key));
        let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
        if let Some(solo) = self.solo {
            state.set_open(id == solo);
        }
        let colors = &self.colors;

        egui::Frame::new()
            .fill(colors.panel)
            .corner_radius(4)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
                if response.clicked() {
                    if ui.input(|i| i.modifiers.command) {
                        state.set_open(true);
                        self.clicked = Some(id);
                    } else {
                        state.toggle(ui);
                    }
                }
                let openness = state.openness(ui.ctx());
                let radius = if openness > 0.0 {
                    CornerRadius {
                        nw: 4,
                        ne: 4,
                        ..Default::default()
                    }
                } else {
                    CornerRadius::same(4)
                };
                let fill = if response.hovered() {
                    colors.header_hover
                } else {
                    colors.header
                };
                ui.painter().rect_filled(rect, radius, fill);
                let icon = Rect::from_center_size(
                    pos2(rect.left() + 12.0, rect.center().y),
                    vec2(8.0, 8.0),
                );
                paint_default_icon(ui, openness, &response.clone().with_new_rect(icon));
                ui.painter_at(rect.with_max_x(rect.right() - 6.0)).text(
                    pos2(rect.left() + 24.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    title,
                    FontId::proportional(13.5),
                    ui.visuals().strong_text_color(),
                );

                state.show_body_unindented(ui, |ui| {
                    egui::Frame::new()
                        .inner_margin(Margin {
                            left: 10,
                            right: 8,
                            top: 2,
                            bottom: 8,
                        })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            add_body(ui);
                        });
                });
            });
        ui.add_space(4.0);
    }
}

#[derive(Clone, Copy)]
struct Palette {
    /// Sidebar background, translucent so the model shows through.
    region: Color32,
    strip: Color32,
    tab: Color32,
    tab_hover: Color32,
    panel: Color32,
    header: Color32,
    header_hover: Color32,
}

impl Palette {
    fn new(v: &egui::Visuals) -> Self {
        let base = v.panel_fill;
        let (light, dark) = if v.dark_mode {
            (Color32::WHITE, Color32::BLACK)
        } else {
            (Color32::BLACK, Color32::WHITE)
        };
        Palette {
            region: base.gamma_multiply(0.88),
            strip: base.lerp_to_gamma(dark, 0.35).gamma_multiply(0.88),
            tab: base.lerp_to_gamma(dark, 0.15),
            tab_hover: base.lerp_to_gamma(light, 0.08),
            panel: base.lerp_to_gamma(light, 0.04),
            header: base.lerp_to_gamma(light, 0.08),
            header_hover: base.lerp_to_gamma(light, 0.13),
        }
    }
}
