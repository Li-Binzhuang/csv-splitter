//! Flow visualization cards and arrows for the split tab.
//! Extracted from split.rs to keep file sizes manageable.

use crate::i18n::{t, Lang};
use crate::state::{FileInfo, JobState, SplitState};
use crate::widgets::format_bytes;
use csv_splitter_core::CompressFormat;

/// Draw the full flow: Source → Split → Compress → Result + column filter below.
pub fn draw_flow(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang, avail_w: f32) {
    let accent = egui::Color32::from_rgb(88, 166, 255);
    let card_bg = egui::Color32::from_rgb(35, 38, 48);
    let card_border = egui::Color32::from_rgb(55, 60, 75);
    let success_color = egui::Color32::from_rgb(80, 200, 120);
    let compress_color = egui::Color32::from_rgb(255, 180, 60);

    let n_splits = if s.no_split {
        1
    } else {
        s.splits.max(1).min(6)
    };
    let has_compress = s.compress_idx < CompressFormat::all().len() - 1;

    let compact = avail_w < 700.0;
    let card_w = if compact {
        ((avail_w - 40.0) / 2.0).min(200.0).max(100.0)
    } else {
        ((avail_w - 80.0) / 4.0).min(220.0).max(120.0)
    };
    let card_h = if compact { 80.0 } else { 110.0 };
    let arrow_w = if compact { 14.0 } else { 20.0 };

    if compact {
        ui.horizontal(|ui| {
            draw_file_drop_card(
                ui,
                &mut s.input_file,
                t!(lang, DropFileHere),
                accent,
                card_bg,
                card_border,
                card_w,
                card_h,
                lang,
                &[(
                    "CSV",
                    &[
                        "csv", "tsv", "txt", "gz", "zst", "bz2", "xz", "zip", "7z", "tar",
                    ],
                )],
            );
            draw_arrow(ui, accent, arrow_w, card_h);
            draw_split_card(
                ui,
                s,
                lang,
                accent,
                card_bg,
                card_border,
                card_w,
                card_h,
                n_splits,
            );
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            draw_compress_card(
                ui,
                s,
                lang,
                compress_color,
                card_bg,
                card_border,
                card_w,
                card_h,
                has_compress,
            );
            draw_arrow(ui, success_color, arrow_w, card_h);
            draw_result_card(
                ui,
                s,
                lang,
                success_color,
                card_bg,
                card_border,
                card_w,
                card_h,
            );
        });
    } else {
        ui.horizontal(|ui| {
            draw_file_drop_card(
                ui,
                &mut s.input_file,
                t!(lang, DropFileHere),
                accent,
                card_bg,
                card_border,
                card_w,
                card_h,
                lang,
                &[(
                    "CSV",
                    &[
                        "csv", "tsv", "txt", "gz", "zst", "bz2", "xz", "zip", "7z", "tar",
                    ],
                )],
            );
            draw_arrow(ui, accent, arrow_w, card_h);
            draw_split_card(
                ui,
                s,
                lang,
                accent,
                card_bg,
                card_border,
                card_w,
                card_h,
                n_splits,
            );
            draw_arrow(
                ui,
                if has_compress { compress_color } else { accent },
                arrow_w,
                card_h,
            );
            draw_compress_card(
                ui,
                s,
                lang,
                compress_color,
                card_bg,
                card_border,
                card_w,
                card_h,
                has_compress,
            );
            draw_arrow(ui, success_color, arrow_w, card_h);
            draw_result_card(
                ui,
                s,
                lang,
                success_color,
                card_bg,
                card_border,
                card_w,
                card_h,
            );
        });
    }

    // Column filter card below
    ui.add_space(4.0);
    draw_file_drop_card(
        ui,
        &mut s.columns_file,
        t!(lang, DropColFileHere),
        egui::Color32::from_rgb(180, 140, 255),
        card_bg,
        card_border,
        if compact { avail_w - 16.0 } else { card_w },
        60.0,
        lang,
        &[("CSV/TXT", &["csv", "txt"])],
    );
}

// ===== Individual cards =====

fn draw_split_card(
    ui: &mut egui::Ui,
    s: &SplitState,
    lang: Lang,
    accent: egui::Color32,
    bg: egui::Color32,
    border: egui::Color32,
    w: f32,
    h: f32,
    n_splits: usize,
) {
    egui::Frame::new()
        .fill(bg)
        .stroke(egui::Stroke::new(
            1.0,
            if s.no_split { border } else { accent },
        ))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(w);
            ui.set_min_height(h);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(t!(lang, FlowSplit)).size(12.0).strong());
                ui.add_space(4.0);
                if s.no_split {
                    ui.label(egui::RichText::new("📄 1:1").size(20.0).weak());
                } else {
                    let anim = s.anim_time;
                    for i in 0..n_splits {
                        let phase = (anim * 2.0 + i as f64 * 0.3).sin() as f32 * 0.15 + 0.85;
                        let bar_w = (w - 20.0) * phase;
                        let (rect, _) = ui
                            .allocate_exact_size(egui::vec2(w - 20.0, 14.0), egui::Sense::hover());
                        let bar_rect = egui::Rect::from_min_size(rect.min, egui::vec2(bar_w, 12.0));
                        ui.painter().rect_filled(
                            bar_rect,
                            3.0,
                            accent.linear_multiply(0.3 + 0.1 * (i as f32)),
                        );
                        ui.painter().text(
                            bar_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            &format!("#{}", i + 1),
                            egui::FontId::proportional(10.0),
                            egui::Color32::from_rgb(200, 210, 225),
                        );
                    }
                }
            });
        });
}

fn draw_compress_card(
    ui: &mut egui::Ui,
    s: &SplitState,
    lang: Lang,
    compress_color: egui::Color32,
    bg: egui::Color32,
    border: egui::Color32,
    w: f32,
    h: f32,
    has_compress: bool,
) {
    egui::Frame::new()
        .fill(bg)
        .stroke(egui::Stroke::new(
            1.0,
            if has_compress { compress_color } else { border },
        ))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(w);
            ui.set_min_height(h);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(t!(lang, FlowCompress))
                        .size(12.0)
                        .strong(),
                );
                ui.add_space(4.0);
                if has_compress {
                    let fmt = CompressFormat::all()[s.compress_idx];
                    ui.label(egui::RichText::new(fmt.label()).size(11.0));
                    let scale = 0.9 + (s.anim_time * 3.0).sin() as f32 * 0.1;
                    ui.label(egui::RichText::new("🗜️").size(28.0 * scale));
                    ui.label(
                        egui::RichText::new(format!("Lv.{}", s.compress_level))
                            .size(10.0)
                            .weak(),
                    );
                } else {
                    ui.label(egui::RichText::new("—").size(20.0).weak());
                    ui.label(egui::RichText::new("No compress").size(10.0).weak());
                }
            });
        });
}

fn draw_result_card(
    ui: &mut egui::Ui,
    s: &SplitState,
    lang: Lang,
    success_color: egui::Color32,
    bg: egui::Color32,
    border: egui::Color32,
    w: f32,
    h: f32,
) {
    egui::Frame::new()
        .fill(bg)
        .stroke(egui::Stroke::new(
            1.0,
            if s.state == JobState::Done {
                success_color
            } else {
                border
            },
        ))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(w);
            ui.set_min_height(h);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(t!(lang, FlowResult))
                        .size(12.0)
                        .strong(),
                );
                ui.add_space(4.0);
                match s.state {
                    JobState::Done => {
                        ui.label(egui::RichText::new("✅").size(24.0));
                        if let Some(ref out) = s.output_file {
                            ui.label(egui::RichText::new(&out.name).size(10.0));
                            ui.label(
                                egui::RichText::new(format_bytes(out.size))
                                    .size(10.0)
                                    .weak(),
                            );
                        }
                    }
                    JobState::Running => {
                        ui.spinner();
                        ui.label(egui::RichText::new(t!(lang, Processing)).size(10.0).weak());
                    }
                    JobState::Error => {
                        ui.label(egui::RichText::new("❌").size(24.0));
                    }
                    JobState::Idle => {
                        ui.label(egui::RichText::new("📦").size(24.0).weak());
                        ui.label(egui::RichText::new("…").size(10.0).weak());
                    }
                }
            });
        });
}

fn draw_arrow(ui: &mut egui::Ui, color: egui::Color32, w: f32, h: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let mid_y = rect.center().y;
    let painter = ui.painter();
    painter.line_segment(
        [
            egui::pos2(rect.left() + 2.0, mid_y),
            egui::pos2(rect.right() - 6.0, mid_y),
        ],
        egui::Stroke::new(2.0, color),
    );
    let tip = egui::pos2(rect.right() - 2.0, mid_y);
    painter.line_segment(
        [egui::pos2(tip.x - 6.0, mid_y - 4.0), tip],
        egui::Stroke::new(2.0, color),
    );
    painter.line_segment(
        [egui::pos2(tip.x - 6.0, mid_y + 4.0), tip],
        egui::Stroke::new(2.0, color),
    );
}

/// File drop card with click-to-browse. Used in split flow and can be reused elsewhere.
pub fn draw_file_drop_card(
    ui: &mut egui::Ui,
    file: &mut Option<FileInfo>,
    placeholder: &str,
    accent: egui::Color32,
    bg: egui::Color32,
    border: egui::Color32,
    w: f32,
    h: f32,
    lang: Lang,
    filters: &[(&str, &[&str])],
) {
    let has_file = file.is_some();
    let stroke_color = if has_file { accent } else { border };

    // Use an invisible button area to capture clicks reliably
    let id = ui.make_persistent_id(format!("drop_card_{}", placeholder));
    let (outer_rect, response) = ui.allocate_exact_size(
        egui::vec2(w + 18.0, h + 18.0), // account for inner_margin + stroke
        egui::Sense::click(),
    );

    // Draw the card frame manually
    let card_rect = outer_rect.shrink(1.0);
    let painter = ui.painter();
    painter.rect_filled(card_rect, egui::CornerRadius::same(8), bg);
    painter.rect_stroke(
        card_rect,
        egui::CornerRadius::same(8),
        egui::Stroke::new(if has_file { 1.5 } else { 1.0 }, stroke_color),
        egui::StrokeKind::Outside,
    );

    // Hover highlight
    if response.hovered() {
        painter.rect_filled(
            card_rect,
            egui::CornerRadius::same(8),
            egui::Color32::from_white_alpha(6),
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // Draw content inside the card
    let content_rect = card_rect.shrink(8.0);
    let mut child_ui = ui.new_child(egui::UiBuilder::new().id_salt(id).max_rect(content_rect));

    if let Some(ref info) = file {
        let name = info.name.clone();
        let size = info.size;
        let ext = info.extension.clone();
        let modified = info.modified;
        child_ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("📄").size(16.0));
                ui.label(egui::RichText::new(&name).size(12.0).strong());
            });
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(format!(
                    "{}: {}",
                    t!(lang, FileSizeLabel),
                    format_bytes(size)
                ))
                .size(10.0)
                .weak(),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{}: {}",
                    t!(lang, FileType),
                    if ext.is_empty() {
                        "—".to_string()
                    } else {
                        ext
                    }
                ))
                .size(10.0)
                .weak(),
            );
            if let Some(modified) = modified {
                if let Ok(dur) = modified.elapsed() {
                    let secs = dur.as_secs();
                    let time_str = if secs < 60 {
                        format!("{}s ago", secs)
                    } else if secs < 3600 {
                        format!("{}m ago", secs / 60)
                    } else if secs < 86400 {
                        format!("{}h ago", secs / 3600)
                    } else {
                        format!("{}d ago", secs / 86400)
                    };
                    ui.label(
                        egui::RichText::new(format!("{}: {}", t!(lang, FileModified), time_str))
                            .size(10.0)
                            .weak(),
                    );
                }
            }
            if ui.small_button(t!(lang, RemoveFile)).clicked() {
                *file = None;
            }
        });
    } else {
        child_ui.vertical_centered(|ui| {
            ui.add_space((h - 30.0).max(0.0) / 2.0);
            ui.label(
                egui::RichText::new(placeholder)
                    .size(11.0)
                    .weak()
                    .color(egui::Color32::from_rgb(120, 128, 145)),
            );
        });
    }

    // Click to browse — only if not clicking the remove button
    if response.clicked() && file.is_some() {
        // If file exists, click does nothing (use remove button)
    } else if response.clicked() && file.is_none() {
        let mut dialog = rfd::FileDialog::new();
        for (name, exts) in filters {
            dialog = dialog.add_filter(*name, exts);
        }
        if let Some(path) = dialog.pick_file() {
            *file = FileInfo::from_path(path);
        }
    }
}
