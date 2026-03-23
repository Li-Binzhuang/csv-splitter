//! Flow visualization card widgets for the split tab.

use crate::i18n::{t, Lang};
use crate::state::{JobState, SplitState};
use crate::widgets::*;
use csv_splitter_core::CompressFormat;

pub fn draw_split_card(
    ui: &mut egui::Ui,
    s: &SplitState,
    lang: Lang,
    w: f32,
    h: f32,
    n_splits: usize,
) {
    let border = if s.no_split { CARD_BORDER } else { ACCENT };
    let margin = 8.0_f32;
    let content_w = (w - 2.0 * (margin + 1.0)).max(40.0);
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(content_w);
            ui.set_max_width(content_w);
            ui.set_min_height(h);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(t!(lang, FlowSplit))
                        .size(12.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(4.0);
                if s.no_split {
                    ui.label(egui::RichText::new("📄 1:1").size(18.0).color(TEXT_DIM));
                } else {
                    let anim = s.anim_time;
                    for i in 0..n_splits {
                        let phase = (anim * 2.0 + i as f64 * 0.3).sin() as f32 * 0.15 + 0.85;
                        let bar_w = (content_w - 20.0) * phase;
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(content_w - 20.0, 14.0),
                            egui::Sense::hover(),
                        );
                        let bar_rect = egui::Rect::from_min_size(rect.min, egui::vec2(bar_w, 12.0));
                        ui.painter().rect_filled(
                            bar_rect,
                            3.0,
                            ACCENT.linear_multiply(0.3 + 0.1 * (i as f32)),
                        );
                        ui.painter().text(
                            bar_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            format!("#{}", i + 1),
                            egui::FontId::proportional(10.0),
                            egui::Color32::from_rgb(200, 210, 225),
                        );
                    }
                }
            });
        });
}

pub fn draw_compress_card(
    ui: &mut egui::Ui,
    s: &SplitState,
    lang: Lang,
    w: f32,
    h: f32,
    has_compress: bool,
) {
    let border = if has_compress { WARN } else { CARD_BORDER };
    let margin = 8.0_f32;
    let content_w = (w - 2.0 * (margin + 1.0)).max(40.0);
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(content_w);
            ui.set_max_width(content_w);
            ui.set_min_height(h);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(t!(lang, FlowCompress))
                        .size(12.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(4.0);
                if has_compress {
                    let fmt = CompressFormat::all()[s.compress_idx];
                    let label = fmt.label();
                    let max_chars = ((content_w - 20.0) / 7.0).max(6.0) as usize;
                    let display = if label.chars().count() > max_chars {
                        let t: String = label.chars().take(max_chars - 1).collect();
                        format!("{}…", t)
                    } else {
                        label.to_string()
                    };
                    ui.label(egui::RichText::new(&display).size(11.0).color(TEXT_BRIGHT));
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

pub fn draw_result_card(
    ui: &mut egui::Ui,
    s: &mut SplitState,
    lang: Lang,
    w: f32,
    h: f32,
    dialogs: &mut Vec<PendingDialog>,
) {
    let border = match s.state {
        JobState::Done => SUCCESS,
        JobState::Running => WARN,
        JobState::Error => ERROR_C,
        _ => CARD_BORDER,
    };
    let margin = 8.0_f32;
    let content_w = (w - 2.0 * (margin + 1.0)).max(40.0);
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(content_w);
            ui.set_max_width(content_w);
            ui.set_min_height(h);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(t!(lang, SectionAction))
                        .size(12.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(4.0);
                let can_start = s.state != JobState::Running && s.input_file.is_some();
                let btn = egui::Button::new(egui::RichText::new(t!(lang, StartSplit)).size(13.0))
                    .min_size([content_w - 20.0, 28.0].into());
                if ui.add_enabled(can_start, btn).clicked() {
                    crate::tabs::split_job::start_split(s);
                }
                match s.state {
                    JobState::Done => {
                        if let Some(ref out) = s.output_file {
                            let max_c = ((content_w - 20.0) / 7.0).max(6.0) as usize;
                            let name = if out.name.chars().count() > max_c {
                                format!("{}…", out.name.chars().take(max_c - 1).collect::<String>())
                            } else {
                                out.name.clone()
                            };
                            ui.label(egui::RichText::new(&name).size(10.0).color(TEXT_BRIGHT));
                            if ui
                                .button(egui::RichText::new(t!(lang, SaveResult)).size(11.0))
                                .clicked()
                            {
                                dialogs.push(spawn_save_file_dialog(
                                    out.name.clone(),
                                    out.path.clone(),
                                ));
                            }
                        }
                    }
                    JobState::Running => {
                        if s.progress_total > 0 {
                            let frac = s.progress_bytes as f32 / s.progress_total as f32;
                            ui.add(egui::ProgressBar::new(frac).animate(true));
                        } else {
                            ui.spinner();
                        }
                    }
                    JobState::Error => {
                        ui.label(egui::RichText::new("❌").size(18.0));
                    }
                    JobState::Idle => {}
                }
            });
        });
}
