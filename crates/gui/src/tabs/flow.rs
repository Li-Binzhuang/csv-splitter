//! Flow visualization cards and arrows for the split tab.

use crate::i18n::{t, Lang};
use crate::state::{JobState, SplitState};
use crate::widgets::*;
use csv_splitter_core::CompressFormat;

/// Draw the full flow: Source → Split → Compress → Result + column filter below.
pub fn draw_flow(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang, avail_w: f32) {
    let n_splits = if s.no_split {
        1
    } else {
        s.splits.max(1).min(6)
    };
    let has_compress = s.compress_idx < CompressFormat::all().len() - 1;

    let compact = avail_w < 700.0;
    let card_w = if compact {
        ((avail_w - 40.0) / 2.0).min(240.0).max(100.0)
    } else {
        ((avail_w - 80.0) / 4.0).min(240.0).max(130.0)
    };
    let card_h = if compact { 70.0 } else { 90.0 };
    let arrow_w = if compact { 14.0 } else { 20.0 };
    let csv_filters: &[(&str, &[&str])] = &[(
        "CSV",
        &[
            "csv", "tsv", "txt", "gz", "zst", "bz2", "xz", "zip", "7z", "tar",
        ],
    )];

    if compact {
        ui.horizontal(|ui| {
            file_input_card(
                ui,
                &mut s.input_file,
                t!(lang, DropFileHere),
                ACCENT,
                card_w,
                card_h,
                lang,
                csv_filters,
            );
            draw_arrow(ui, ACCENT, arrow_w, card_h + 22.0);
            draw_split_card(ui, s, lang, card_w, card_h, n_splits);
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            draw_compress_card(ui, s, lang, card_w, card_h, has_compress);
            draw_arrow(ui, SUCCESS, arrow_w, card_h + 22.0);
            draw_result_card(ui, s, lang, card_w, card_h);
        });
    } else {
        ui.horizontal(|ui| {
            file_input_card(
                ui,
                &mut s.input_file,
                t!(lang, DropFileHere),
                ACCENT,
                card_w,
                card_h,
                lang,
                csv_filters,
            );
            draw_arrow(ui, ACCENT, arrow_w, card_h + 22.0);
            draw_split_card(ui, s, lang, card_w, card_h, n_splits);
            draw_arrow(
                ui,
                if has_compress { WARN } else { ACCENT },
                arrow_w,
                card_h + 22.0,
            );
            draw_compress_card(ui, s, lang, card_w, card_h, has_compress);
            draw_arrow(ui, SUCCESS, arrow_w, card_h + 22.0);
            draw_result_card(ui, s, lang, card_w, card_h);
        });
    }

    // Column filter card below
    ui.add_space(6.0);
    let col_w = if compact {
        avail_w - 16.0
    } else {
        card_w * 2.0 + arrow_w
    };
    file_input_card(
        ui,
        &mut s.columns_file,
        t!(lang, DropColFileHere),
        PURPLE,
        col_w,
        50.0,
        lang,
        &[("CSV/TXT", &["csv", "txt"])],
    );
}

fn draw_split_card(ui: &mut egui::Ui, s: &SplitState, lang: Lang, w: f32, h: f32, n_splits: usize) {
    let border = if s.no_split { CARD_BORDER } else { ACCENT };
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(w);
            ui.set_min_height(h);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(t!(lang, FlowSplit)).size(12.0).strong());
                ui.add_space(4.0);
                if s.no_split {
                    ui.label(egui::RichText::new("📄 1:1").size(18.0).color(TEXT_DIM));
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
                            ACCENT.linear_multiply(0.3 + 0.1 * (i as f32)),
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
    w: f32,
    h: f32,
    has_compress: bool,
) {
    let border = if has_compress { WARN } else { CARD_BORDER };
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, border))
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
                            .color(TEXT_DIM),
                    );
                } else {
                    ui.label(egui::RichText::new("—").size(18.0).color(TEXT_DIM));
                    ui.label(
                        egui::RichText::new("No compress")
                            .size(10.0)
                            .color(TEXT_DIM),
                    );
                }
            });
        });
}

fn draw_result_card(ui: &mut egui::Ui, s: &SplitState, lang: Lang, w: f32, h: f32) {
    let border = if s.state == JobState::Done {
        SUCCESS
    } else {
        CARD_BORDER
    };
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, border))
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
                        ui.label(egui::RichText::new("✅").size(22.0));
                        if let Some(ref out) = s.output_file {
                            ui.label(egui::RichText::new(&out.name).size(10.0));
                            ui.label(
                                egui::RichText::new(format_bytes(out.size))
                                    .size(10.0)
                                    .color(TEXT_DIM),
                            );
                        }
                    }
                    JobState::Running => {
                        ui.spinner();
                        ui.label(
                            egui::RichText::new(t!(lang, Processing))
                                .size(10.0)
                                .color(TEXT_DIM),
                        );
                    }
                    JobState::Error => {
                        ui.label(egui::RichText::new("❌").size(22.0));
                    }
                    JobState::Idle => {
                        ui.label(egui::RichText::new("📦").size(22.0).color(TEXT_DIM));
                        ui.label(egui::RichText::new("…").size(10.0).color(TEXT_DIM));
                    }
                }
            });
        });
}

fn draw_arrow(ui: &mut egui::Ui, color: egui::Color32, w: f32, h: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let mid_y = rect.center().y;
    let p = ui.painter();
    p.line_segment(
        [
            egui::pos2(rect.left() + 2.0, mid_y),
            egui::pos2(rect.right() - 6.0, mid_y),
        ],
        egui::Stroke::new(2.0, color),
    );
    let tip = egui::pos2(rect.right() - 2.0, mid_y);
    p.line_segment(
        [egui::pos2(tip.x - 6.0, mid_y - 4.0), tip],
        egui::Stroke::new(2.0, color),
    );
    p.line_segment(
        [egui::pos2(tip.x - 6.0, mid_y + 4.0), tip],
        egui::Stroke::new(2.0, color),
    );
}
