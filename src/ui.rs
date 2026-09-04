//! Baffle UI — a compact dark control panel for a live loudness instrument.
//!
//! Visual system:
//! • Surfaces: app background → card → inset/control well, separated by exact
//!   palette tones and 1 px borders rather than gradients or translucent chrome.
//! • Typography: a 20 px semibold nameplate, 11 px tracked eyebrows, and a
//!   36 px bold monospace loudness anchor. The numbers are intentionally the
//!   first thing the eye finds.
//! • Signature: the meter is treated like a small hardware instrument — a
//!   calibrated dB grid, dashed target reference, live signal profile, and
//!   restrained five-band energy indicators.
//! • Spacing: every layout margin and gap follows an 8 px base unit.

use eframe::egui;
use parking_lot::RwLock;
use std::sync::Arc;

use crate::config::Config;
use crate::dsp::BANDS;
use crate::SharedTelemetry;

// --------------------------------------------------------------- palette ----
// These are the only visible UI colors. Keep derived alpha colors out of the
// UI so the palette remains inspectable and every text pair stays predictable.

const BG: egui::Color32 = egui::Color32::from_rgb(0x0F, 0x11, 0x15);
const CARD: egui::Color32 = egui::Color32::from_rgb(0x1C, 0x1F, 0x26);
const INSET: egui::Color32 = egui::Color32::from_rgb(0x14, 0x16, 0x1C);
const BORDER: egui::Color32 = egui::Color32::from_rgb(0x2A, 0x2E, 0x38);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xF5, 0xF6, 0xF8);
const SECONDARY: egui::Color32 = egui::Color32::from_rgb(0x9C, 0xA3, 0xAF);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x2D, 0xD4, 0xBF);
const OFF: egui::Color32 = egui::Color32::from_rgb(0x6B, 0x72, 0x80);

const BAND_NAMES: [&str; BANDS] = ["SUB", "LOW", "MID", "HIGH", "AIR"];
const METER_FLOOR_DB: f32 = -80.0;
const PEAK_FALL_DB_S: f32 = 30.0;
const CARD_GAP: f32 = 16.0;
/// Compact widget scale: keeps every custom-painted control and text role in
/// the same proportion while making the native window small enough to park.
const UI_SCALE: f32 = 0.5;
/// Text remains intentionally larger than the compact control geometry. At a
/// 0.5 UI zoom this restores roughly 88% of the original physical type size,
/// keeping the widget small without reducing labels to unreadable microcopy.
const TEXT_SCALE: f32 = 1.75;

fn text_size(size: f32) -> f32 {
    size * TEXT_SCALE
}

fn font_proportional(size: f32) -> egui::FontId {
    egui::FontId::proportional(text_size(size))
}

fn font_monospace(size: f32) -> egui::FontId {
    egui::FontId::monospace(text_size(size))
}

fn font_semibold(size: f32) -> egui::FontId {
    egui::FontId::new(
        text_size(size),
        egui::FontFamily::Name("baffle-semibold".to_owned().into()),
    )
}

// ------------------------------------------------------------------ entry ---

pub fn run(cfg: Arc<RwLock<Config>>, tel: SharedTelemetry) {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 520.0])
            .with_min_inner_size([280.0, 400.0])
            .with_resizable(true)
            .with_transparent(false)
            .with_icon(egui::IconData {
                width: crate::icon::ICON_W,
                height: crate::icon::ICON_H,
                rgba: crate::icon::icon_rgba(),
            }),
        ..Default::default()
    };

    let _ = eframe::run_native(
        "Baffle",
        opts,
        Box::new(move |cc| {
            apply_style(cc.egui_ctx.clone());
            // Apply the scale before the first rendered pass so the viewport
            // and every custom-painted control share the same compact unit.
            cc.egui_ctx.set_zoom_factor(UI_SCALE);
            Ok(Box::new(BaffleApp {
                cfg: cfg.clone(),
                tel: tel.clone(),
                peaks: [METER_FLOOR_DB; BANDS],
                smoothed_levels: [METER_FLOOR_DB; BANDS],
            }))
        }),
    );
}

/// Load a platform font when available, while retaining egui's built-in
/// fallback. This keeps the app native-feeling without bundling font files.
fn load_system_fonts(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    let mut regular_name = None;
    let mut bold_name = None;

    #[cfg(windows)]
    let candidates: &[(&str, &str)] = &[
        ("sys-regular", "C:\\Windows\\Fonts\\segoeui.ttf"),
        ("sys-bold", "C:\\Windows\\Fonts\\segoeuib.ttf"),
    ];
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &str)] = &[
        ("sys-regular", "/System/Library/Fonts/SFNS.ttf"),
        ("sys-bold", "/System/Library/Fonts/SFNS-Bold.ttf"),
    ];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: &[(&str, &str)] = &[
        (
            "sys-regular",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ),
        (
            "sys-bold",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        ),
    ];

    for (name, path) in candidates {
        if defs.font_data.contains_key(*name) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(path) {
            if ab_glyph::FontArc::try_from_vec(bytes.clone()).is_err() {
                continue;
            }
            defs.font_data
                .insert(name.to_string(), egui::FontData::from_owned(bytes).into());
            if *name == "sys-regular" {
                regular_name = Some(name.to_string());
            } else if *name == "sys-bold" {
                bold_name = Some(name.to_string());
            }
        }
    }

    if let Some(name) = regular_name.clone() {
        if let Some(fam) = defs.families.get_mut(&egui::FontFamily::Proportional) {
            fam.insert(0, name);
        }
    }

    // Painter-drawn value badges need an explicit semibold family because
    // FontId carries family + size, not a separate weight field.
    let mut strong_family = Vec::new();
    if let Some(name) = bold_name {
        strong_family.push(name);
    }
    if let Some(name) = regular_name {
        strong_family.push(name);
    }
    if strong_family.is_empty() {
        strong_family = defs
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
    }
    defs.families.insert(
        egui::FontFamily::Name("baffle-semibold".to_owned().into()),
        strong_family,
    );
    ctx.set_fonts(defs);
}

fn apply_style(ctx: egui::Context) {
    load_system_fonts(&ctx);

    let mut st = egui::Style::default();
    st.visuals = egui::Visuals::dark();
    st.visuals.override_text_color = Some(TEXT);
    st.visuals.panel_fill = BG;
    st.visuals.window_fill = BG;
    st.visuals.extreme_bg_color = INSET;
    st.visuals.faint_bg_color = INSET;
    st.visuals.selection.bg_fill = ACCENT;
    st.visuals.selection.stroke = egui::Stroke::new(1.0_f32, BG);
    st.visuals.hyperlink_color = ACCENT;
    st.visuals.code_bg_color = INSET;
    st.visuals.warn_fg_color = OFF;
    st.visuals.error_fg_color = OFF;
    st.visuals.window_fill = BG;
    st.visuals.window_stroke = egui::Stroke::new(1.0_f32, BORDER);
    st.visuals.window_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 16,
        spread: 0,
        color: BG,
    };
    st.visuals.panel_fill = BG;
    st.visuals.popup_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 16,
        spread: 0,
        color: BG,
    };
    st.visuals.text_cursor.stroke = egui::Stroke::new(2.0_f32, ACCENT);

    st.visuals.widgets.noninteractive.bg_fill = BG;
    st.visuals.widgets.noninteractive.weak_bg_fill = BG;
    st.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    st.visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    st.visuals.widgets.inactive.bg_fill = INSET;
    st.visuals.widgets.inactive.weak_bg_fill = INSET;
    st.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    st.visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    st.visuals.widgets.hovered.bg_fill = CARD;
    st.visuals.widgets.hovered.weak_bg_fill = CARD;
    st.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
    st.visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    st.visuals.widgets.active.bg_fill = ACCENT;
    st.visuals.widgets.active.weak_bg_fill = ACCENT;
    st.visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, BG);
    st.visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, BG);
    st.visuals.widgets.open.bg_fill = INSET;
    st.visuals.widgets.open.weak_bg_fill = INSET;
    st.visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    st.visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);

    let rounded = egui::CornerRadius::same(8);
    for widget in [
        &mut st.visuals.widgets.noninteractive,
        &mut st.visuals.widgets.inactive,
        &mut st.visuals.widgets.hovered,
        &mut st.visuals.widgets.active,
        &mut st.visuals.widgets.open,
    ] {
        widget.corner_radius = rounded;
    }

    st.spacing.item_spacing = egui::vec2(8.0, 8.0);
    st.spacing.button_padding = egui::vec2(8.0, 8.0);
    st.spacing.window_margin = egui::Margin::same(24);
    st.spacing.menu_margin = egui::Margin::same(8);
    st.spacing.indent = 16.0;
    st.spacing.icon_width = 16.0;
    st.spacing.icon_width_inner = 8.0;
    st.spacing.icon_spacing = 8.0;
    st.spacing.scroll = egui::style::ScrollStyle {
        floating: true,
        bar_width: 8.0,
        handle_min_length: 16.0,
        bar_inner_margin: 8.0,
        bar_outer_margin: 0.0,
        floating_width: 8.0,
        floating_allocated_width: 0.0,
        foreground_color: true,
        dormant_background_opacity: 0.0,
        active_background_opacity: 0.0,
        interact_background_opacity: 0.0,
        dormant_handle_opacity: 0.0,
        active_handle_opacity: 1.0,
        interact_handle_opacity: 1.0,
    };

    ctx.set_style(st);
}

// ------------------------------------------------------------------- app ----

struct BaffleApp {
    cfg: Arc<RwLock<Config>>,
    tel: SharedTelemetry,
    peaks: [f32; BANDS],
    smoothed_levels: [f32; BANDS],
}

impl BaffleApp {
    fn save(&self, enabled: bool, target: f32, strength: f32) {
        let mut c = self.cfg.write();
        c.enabled = enabled;
        c.target_loudness = target;
        c.strength = strength;
        let _ = c.save();
    }
}

impl eframe::App for BaffleApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [
            0x0F as f32 / 255.0,
            0x11 as f32 / 255.0,
            0x15 as f32 / 255.0,
            1.0,
        ]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(target_os = "macos")]
        crate::volume::mac_tray_state::poll_menu_events();

        self.show(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}

impl BaffleApp {
    fn show(&mut self, ctx: &egui::Context) {
        let (enabled, target, strength) = {
            let c = self.cfg.read();
            (c.enabled, c.target_loudness, c.strength)
        };
        let tel = *self.tel.read();

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::same(24)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("baffle_main_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            self.header(ui, enabled);
                            ui.add_space(CARD_GAP);
                            self.meter_card(ui, &tel, enabled, target);
                            ui.add_space(CARD_GAP);
                            self.controls_card(ui, enabled, target, strength);
                        });
                    });
            });
    }

    fn card(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(CARD)
            .stroke(egui::Stroke::new(1.0_f32, BORDER))
            .corner_radius(egui::CornerRadius::same(16))
            .inner_margin(egui::Margin::same(24))
    }

    // --------------------------------------------------------------- header --

    fn header(&mut self, ui: &mut egui::Ui, enabled: bool) {
        ui.horizontal(|ui| {
            logo_mark(ui);
            ui.add_space(16.0);
            ui.vertical(|ui| {
                eyebrow(ui, "LOUDNESS GOVERNOR");
                ui.label(
                    egui::RichText::new("Baffle")
                        .size(text_size(20.0))
                        .strong()
                        .color(TEXT),
                );
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.state_toggle(ui, enabled);
            });
        });
    }

    /// One cohesive two-state control. Active uses the only accent fill;
    /// paused uses a neutral border surface and an OFF status dot so no text
    /// is placed on the lower-contrast gray warning color.
    fn state_toggle(&mut self, ui: &mut egui::Ui, enabled: bool) {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(192.0, 40.0), egui::Sense::click());
        let painter = ui.painter_at(rect);
        let radius = egui::CornerRadius::same(20);

        painter.rect_filled(rect, radius, INSET);
        painter.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0_f32, BORDER),
            egui::StrokeKind::Inside,
        );

        let position = ui.ctx().animate_value_with_time(
            egui::Id::new("baffle_state_indicator"),
            if enabled { 0.0 } else { 1.0 },
            0.16,
        );
        let segment_width = rect.width() * 0.5 - 8.0;
        let segment_x = rect.left() + 8.0 + position * (rect.width() * 0.5);
        let segment = egui::Rect::from_min_size(
            egui::pos2(segment_x, rect.top() + 8.0),
            egui::vec2(segment_width, rect.height() - 16.0),
        );
        let segment_fill = if enabled { ACCENT } else { BORDER };
        painter.rect_filled(segment, egui::CornerRadius::same(16), segment_fill);

        let center_y = rect.center().y;
        let active_center = egui::pos2(rect.left() + rect.width() * 0.25, center_y);
        let paused_center = egui::pos2(rect.left() + rect.width() * 0.75, center_y);
        let active_text = if enabled { BG } else { SECONDARY };
        let paused_text = if enabled { SECONDARY } else { TEXT };

        if enabled {
            painter.circle_filled(egui::pos2(active_center.x - 25.0, center_y), 3.0, BG);
        } else {
            painter.circle_filled(egui::pos2(paused_center.x - 25.0, center_y), 3.0, OFF);
        }

        painter.text(
            egui::pos2(active_center.x + 8.0, center_y),
            egui::Align2::CENTER_CENTER,
            "ACTIVE",
            font_proportional(11.0),
            active_text,
        );
        painter.text(
            egui::pos2(paused_center.x + 8.0, center_y),
            egui::Align2::CENTER_CENTER,
            "PAUSED",
            font_proportional(11.0),
            paused_text,
        );

        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            response.request_focus();
        }
        if response.clicked()
            || (response.has_focus()
                && ui.input(|input| {
                    input.key_pressed(egui::Key::Space) || input.key_pressed(egui::Key::Enter)
                }))
        {
            let want_active = response
                .interact_pointer_pos()
                .map(|pos| pos.x < rect.center().x)
                .unwrap_or(!enabled);
            if want_active != enabled {
                let (target, strength) = {
                    let c = self.cfg.read();
                    (c.target_loudness, c.strength)
                };
                self.save(want_active, target, strength);
            }
        }
    }

    // ---------------------------------------------------------- meter card ---

    fn meter_card(
        &mut self,
        ui: &mut egui::Ui,
        tel: &crate::Telemetry,
        enabled: bool,
        target_db: f32,
    ) {
        self.card().show(ui, |ui| {
            ui.horizontal(|ui| {
                let status = if !enabled {
                    "Bypassed"
                } else if tel.ducking {
                    "Clamping spike"
                } else if tel.lifting {
                    "Lifting dialogue"
                } else {
                    "Riding steady"
                };
                let status_color = if enabled { ACCENT } else { OFF };

                let (dot_rect, _) =
                    ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                ui.painter()
                    .circle_filled(dot_rect.center(), 4.0, status_color);
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(status)
                        .size(text_size(13.0))
                        .strong()
                        .color(TEXT),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if enabled && tel.action_db.abs() > 0.05 {
                        metric_badge(ui, &format_action(tel.action_db), ACCENT);
                        ui.add_space(8.0);
                    }
                    metric_badge(ui, &format!("TARGET {:.0} dB", target_db), SECONDARY);
                });
            });

            ui.add_space(16.0);
            self.meter_graph(ui, tel, enabled, target_db);
            ui.add_space(16.0);
            self.stats_row(ui, tel, enabled);
        });
    }

    fn meter_graph(
        &mut self,
        ui: &mut egui::Ui,
        tel: &crate::Telemetry,
        enabled: bool,
        target_db: f32,
    ) {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 248.0),
            egui::Sense::hover(),
        );
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, egui::CornerRadius::same(8), INSET);
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(8),
            egui::Stroke::new(1.0_f32, BORDER),
            egui::StrokeKind::Inside,
        );

        let plot = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 48.0, rect.top() + 16.0),
            egui::pos2(rect.right() - 16.0, rect.bottom() - 72.0),
        );
        let db_to_y = |db: f32| {
            let normalized = ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0);
            plot.bottom() - normalized * plot.height()
        };

        // Calibrated dB grid and labels.
        for db in [-72.0, -48.0, -24.0, 0.0] {
            let y = db_to_y(db);
            painter.line_segment(
                [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
                egui::Stroke::new(1.0_f32, BORDER),
            );
            painter.text(
                egui::pos2(rect.left() + 40.0, y),
                egui::Align2::RIGHT_CENTER,
                format!("{db:.0}"),
                font_monospace(10.0),
                SECONDARY,
            );
        }
        painter.text(
            egui::pos2(rect.left() + 16.0, rect.top() + 16.0),
            egui::Align2::LEFT_TOP,
            "dBFS",
            font_proportional(10.0),
            SECONDARY,
        );

        // Vertical guide lines keep the five-band instrument aligned to its
        // labels without making the grid visually busy.
        let slot = plot.width() / BANDS as f32;
        for i in 0..BANDS {
            let x = plot.left() + slot * (i as f32 + 0.5);
            painter.line_segment(
                [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
                egui::Stroke::new(1.0_f32, BORDER),
            );
        }

        // Target reference: dashed teal line plus a compact label badge.
        let target_y = db_to_y(target_db);
        let mut x = plot.left();
        while x < plot.right() {
            let end = (x + 10.0).min(plot.right());
            painter.line_segment(
                [egui::pos2(x, target_y), egui::pos2(end, target_y)],
                egui::Stroke::new(1.5_f32, ACCENT),
            );
            x += 18.0;
        }
        target_badge(
            &painter,
            plot.right() - 8.0,
            target_y,
            &format!("{target_db:.0} dB"),
        );

        // Smooth the live five-band profile so it reads as an instrument, not
        // a jittery diagnostic. Paused audio settles to the meter floor.
        let dt = ui.input(|input| input.stable_dt.max(0.001)) as f32;
        for i in 0..BANDS {
            let next = if enabled {
                tel.band_levels_db[i].max(METER_FLOOR_DB)
            } else {
                METER_FLOOR_DB
            };
            let response = 1.0 - (-dt / 0.08).exp();
            self.smoothed_levels[i] += (next - self.smoothed_levels[i]) * response;
            if self.smoothed_levels[i] > self.peaks[i] {
                self.peaks[i] = self.smoothed_levels[i];
            } else {
                self.peaks[i] = (self.peaks[i] - PEAK_FALL_DB_S * dt).max(self.smoothed_levels[i]);
            }
        }

        // Current live signal profile: primary-text solid line, intentionally
        // different from the dashed teal target reference.
        if enabled {
            for i in 0..BANDS.saturating_sub(1) {
                let x1 = plot.left() + slot * (i as f32 + 0.5);
                let x2 = plot.left() + slot * ((i + 1) as f32 + 0.5);
                let y1 = db_to_y(self.smoothed_levels[i]);
                let y2 = db_to_y(self.smoothed_levels[i + 1]);
                painter.line_segment(
                    [egui::pos2(x1, y1), egui::pos2(x2, y2)],
                    egui::Stroke::new(2.0_f32, TEXT),
                );
            }
        }

        // Small frequency energy indicators. Gray is the resting rail; teal
        // fills toward the current relative energy and never dominates the
        // calibrated plot above it.
        let indicator_top = plot.bottom() + 8.0;
        let indicator_bottom = rect.bottom() - 32.0;
        let indicator_height = indicator_bottom - indicator_top;
        for i in 0..BANDS {
            let cx = plot.left() + slot * (i as f32 + 0.5);
            let width = 6.0;
            let rail = egui::Rect::from_min_max(
                egui::pos2(cx - width * 0.5, indicator_top),
                egui::pos2(cx + width * 0.5, indicator_bottom),
            );
            painter.rect_filled(rail, egui::CornerRadius::same(3), SECONDARY);

            let level = if enabled {
                self.smoothed_levels[i]
            } else {
                METER_FLOOR_DB
            };
            let amount = ((level - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0);
            let fill_height = indicator_height * amount;
            if fill_height > 0.0 {
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(cx - width * 0.5, indicator_bottom - fill_height),
                        egui::pos2(cx + width * 0.5, indicator_bottom),
                    ),
                    egui::CornerRadius::same(3),
                    ACCENT,
                );
            }

            if enabled && self.peaks[i] > METER_FLOOR_DB + 1.0 {
                let peak_amount =
                    ((self.peaks[i] - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0);
                let peak_y = indicator_bottom - indicator_height * peak_amount;
                painter.line_segment(
                    [egui::pos2(cx - 6.0, peak_y), egui::pos2(cx + 6.0, peak_y)],
                    egui::Stroke::new(1.0_f32, TEXT),
                );
            }

            painter.text(
                egui::pos2(cx, rect.bottom() - 16.0),
                egui::Align2::CENTER_CENTER,
                BAND_NAMES[i],
                font_proportional(10.0),
                SECONDARY,
            );
        }

        if !enabled {
            painter.text(
                plot.center(),
                egui::Align2::CENTER_CENTER,
                "Paused",
                font_proportional(13.0),
                SECONDARY,
            );
        } else if tel.loudness < -70.0 {
            painter.text(
                plot.center(),
                egui::Align2::CENTER_CENTER,
                "Listening for audio...",
                font_proportional(12.0),
                SECONDARY,
            );
        }
    }

    fn stats_row(&self, ui: &mut egui::Ui, tel: &crate::Telemetry, enabled: bool) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                eyebrow(ui, "LOUDNESS");
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{:.1}", tel.loudness))
                            .size(text_size(36.0))
                            .strong()
                            .monospace()
                            .color(TEXT),
                    );
                    ui.label(
                        egui::RichText::new("dB")
                            .size(text_size(14.0))
                            .strong()
                            .color(SECONDARY),
                    );
                });
            });

            ui.add_space(24.0);
            vertical_rule(ui, 48.0);
            ui.add_space(24.0);
            stat_block(ui, "ANCHOR", &format!("{:.1} dB", tel.anchor), TEXT);
            ui.add_space(24.0);
            vertical_rule(ui, 48.0);
            ui.add_space(24.0);
            let gain_color = if enabled && tel.action_db.abs() > 0.05 {
                ACCENT
            } else {
                TEXT
            };
            stat_block(ui, "GAIN", &format!("×{:.2}", tel.applied), gain_color);
        });
    }

    // ------------------------------------------------------------ controls ---

    fn controls_card(&mut self, ui: &mut egui::Ui, enabled: bool, target: f32, strength: f32) {
        let mut target_value = target;
        let mut strength_value = strength;

        self.card().show(ui, |ui| {
            eyebrow(ui, "CONTROL SURFACE");
            ui.add_space(8.0);
            let target_response = custom_slider(
                ui,
                "TARGET LOUDNESS",
                &mut target_value,
                -80.0..=-12.0,
                |value| format!("{value:.0} dB"),
            );
            ui.add_space(16.0);
            let strength_response = custom_slider(
                ui,
                "CORRECTION STRENGTH",
                &mut strength_value,
                0.05..=1.5,
                |value| format!("{:.0}%", value * 100.0),
            );

            if target_response.changed() || strength_response.changed() {
                self.save(enabled, target_value, strength_value);
            }
        });
    }
}

// ------------------------------------------------------------- primitives ---

fn logo_mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::same(12), INSET);
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(12),
        egui::Stroke::new(1.0_f32, BORDER),
        egui::StrokeKind::Inside,
    );

    let bars = [10.0, 18.0, 28.0, 18.0, 10.0];
    let bar_width = 3.0;
    let gap = 3.0;
    let total_width = bars.len() as f32 * bar_width + (bars.len() as f32 - 1.0) * gap;
    let start_x = rect.center().x - total_width * 0.5;
    for (index, height) in bars.iter().enumerate() {
        let x = start_x + index as f32 * (bar_width + gap);
        painter.rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(x + bar_width * 0.5, rect.center().y),
                egui::vec2(bar_width, *height),
            ),
            egui::CornerRadius::same(2),
            ACCENT,
        );
    }
}

fn eyebrow(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(text_size(11.0))
            .strong()
            .extra_letter_spacing(text_size(0.55))
            .color(SECONDARY),
    );
}

fn metric_badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(INSET)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(8, 8))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .size(text_size(11.0))
                    .strong()
                    .color(color),
            );
        });
}

fn vertical_rule(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, height), egui::Sense::hover());
    ui.painter().line_segment(
        [rect.center_top(), rect.center_bottom()],
        egui::Stroke::new(1.0_f32, BORDER),
    );
}

fn stat_block(ui: &mut egui::Ui, label: &str, value: &str, color: egui::Color32) {
    ui.vertical(|ui| {
        eyebrow(ui, label);
        ui.label(
            egui::RichText::new(value)
                .size(text_size(18.0))
                .strong()
                .monospace()
                .color(color),
        );
    });
}

fn target_badge(painter: &egui::Painter, right: f32, y: f32, text: &str) {
    let font = font_monospace(11.0);
    let galley = painter.layout_no_wrap(text.to_owned(), font, ACCENT);
    let size = galley.size();
    let rect = egui::Rect::from_min_size(
        egui::pos2(right - size.x - 16.0, y - size.y - 8.0),
        egui::vec2(size.x + 16.0, size.y + 8.0),
    );
    painter.rect_filled(rect, egui::CornerRadius::same(8), INSET);
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(8),
        egui::Stroke::new(1.0_f32, BORDER),
        egui::StrokeKind::Inside,
    );
    painter.galley(
        egui::pos2(rect.left() + 8.0, rect.top() + 8.0),
        galley,
        ACCENT,
    );
}

fn format_action(action_db: f32) -> String {
    if action_db >= 0.0 {
        format!("+{action_db:.1} dB")
    } else {
        format!("{action_db:.1} dB")
    }
}

/// Hand-built slider: exact 4 px rail, exact 16 px light thumb, and a value
/// badge positioned above the thumb so the value belongs to the control.
fn custom_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    format_value: impl Fn(f32) -> String,
) -> egui::Response {
    eyebrow(ui, label);
    ui.add_space(8.0);

    let (rect, mut response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 56.0),
        egui::Sense::click_and_drag(),
    );

    let min = *range.start();
    let max = *range.end();
    let track_left = rect.left() + 8.0;
    let track_right = rect.right() - 8.0;
    let track_width = (track_right - track_left).max(1.0);

    let set_from_pointer = |value: &mut f32, response: &egui::Response| {
        if let Some(pointer) = response.interact_pointer_pos() {
            let t = ((pointer.x - track_left) / track_width).clamp(0.0, 1.0);
            let next = min + t * (max - min);
            if (next - *value).abs() > 0.0001 {
                *value = next;
            }
        }
    };

    if response.clicked() || response.dragged() {
        response.request_focus();
        let before = *value;
        set_from_pointer(value, &response);
        if (*value - before).abs() > 0.0001 {
            response.mark_changed();
        }
    }

    if response.has_focus() {
        let keyboard_step = (max - min) / 100.0;
        let (left, right) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowLeft),
                input.key_pressed(egui::Key::ArrowRight),
            )
        });
        let before = *value;
        if left {
            *value = (*value - keyboard_step).clamp(min, max);
        }
        if right {
            *value = (*value + keyboard_step).clamp(min, max);
        }
        if (*value - before).abs() > 0.0001 {
            response.mark_changed();
        }
    }

    let t = ((*value - min) / (max - min)).clamp(0.0, 1.0);
    let thumb_x = track_left + track_width * t;
    let track_y = rect.top() + 40.0;
    let painter = ui.painter_at(rect);

    painter.rect_filled(
        egui::Rect::from_center_size(
            egui::pos2((track_left + track_right) * 0.5, track_y),
            egui::vec2(track_width, 4.0),
        ),
        egui::CornerRadius::same(2),
        BORDER,
    );
    let filled_width = (thumb_x - track_left).max(0.0);
    if filled_width > 0.0 {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(track_left, track_y - 2.0),
                egui::pos2(thumb_x, track_y + 2.0),
            ),
            egui::CornerRadius::same(2),
            ACCENT,
        );
    }

    // The dark outer disc acts as a quiet shadow/ring without introducing a
    // color outside the prescribed palette.
    painter.circle_filled(egui::pos2(thumb_x, track_y + 2.0), 10.0, BG);
    painter.circle_filled(egui::pos2(thumb_x, track_y), 8.0, TEXT);
    painter.circle_stroke(
        egui::pos2(thumb_x, track_y),
        8.0,
        egui::Stroke::new(1.0_f32, BORDER),
    );

    let value_text = format_value(*value);
    let value_font = font_semibold(13.0);
    let galley = painter.layout_no_wrap(value_text, value_font, TEXT);
    let size = galley.size();
    let badge_width = size.x + 16.0;
    let badge_left = (thumb_x - badge_width * 0.5)
        .max(rect.left())
        .min(rect.right() - badge_width);
    let badge_rect = egui::Rect::from_min_size(
        egui::pos2(badge_left, rect.top()),
        egui::vec2(badge_width, size.y + 8.0),
    );
    painter.rect_filled(badge_rect, egui::CornerRadius::same(8), INSET);
    painter.rect_stroke(
        badge_rect,
        egui::CornerRadius::same(8),
        egui::Stroke::new(1.0_f32, BORDER),
        egui::StrokeKind::Inside,
    );
    painter.galley(
        egui::pos2(badge_rect.left() + 8.0, badge_rect.top() + 4.0),
        galley,
        TEXT,
    );

    if response.has_focus() || response.hovered() {
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(8),
            egui::Stroke::new(1.0_f32, ACCENT),
            egui::StrokeKind::Inside,
        );
    }

    response.on_hover_cursor(egui::CursorIcon::Grab)
}
