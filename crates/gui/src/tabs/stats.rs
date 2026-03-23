use crate::i18n::{t, Lang};
use crate::state::{JobState, StatsState};
use crate::widgets::*;
use csv_splitter_core::preview::compute_column_stats;
use csv_splitter_core::ProgressEvent;
use std::sync::mpsc;

pub fn ui_stats(
    ui: &mut egui::Ui,
    s: &mut StatsState,
    lang: Lang,
    dialogs: &mut Vec<PendingDialog>,
) {
    poll_stats(s, lang);

    ui.vertical_centered(|ui| {
        ui.heading(egui::RichText::new(t!(lang, StatsTitle)).color(TEXT_BRIGHT));
        ui.label(
            egui::RichText::new(t!(lang, StatsDesc))
                .size(12.0)
                .color(TEXT_DIM),
        );
    });
    ui.add_space(8.0);

    // ── Two-column: Input (left) | Params + Action (right) ──
    let (col_w, left_rect, right_rect) = two_col_rects(ui, 8.0);

    // Draw right (params) first to measure its height
    let right_h = {
        let mut col = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("stats_right")
                .max_rect(right_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        col.set_max_width(col_w);
        col.set_width(col_w);
        egui::Frame::new()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut col, |ui| {
                ui.label(
                    egui::RichText::new(t!(lang, SectionParams))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(6.0);
                delimiter_combo(ui, &mut s.delim_idx, &mut s.custom_delim, lang);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(t!(lang, SampleSize)).color(TEXT_BRIGHT));
                    ui.add(egui::DragValue::new(&mut s.sample_size).range(1000..=10_000_000));
                });
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(t!(lang, SectionAction))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(4.0);
                let can_start = s.state != JobState::Running && s.input_file.is_some();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            can_start,
                            egui::Button::new(
                                egui::RichText::new(t!(lang, StartAnalysis)).size(14.0),
                            )
                            .min_size([120.0, 32.0].into()),
                        )
                        .clicked()
                    {
                        start_stats(s);
                    }
                    job_status(ui, &s.state, lang);
                });
            });
        col.min_rect().height()
    };

    // Draw left (input) second, matching right height
    let left_h = {
        let mut col = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("stats_left")
                .max_rect(left_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        col.set_max_width(col_w);
        col.set_width(col_w);
        let inner_w = col_w - 30.0;
        egui::Frame::new()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut col, |ui| {
                ui.set_min_width(inner_w);
                ui.set_min_height((right_h - 40.0).max(60.0));
                ui.label(
                    egui::RichText::new(t!(lang, SectionInput))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(6.0);
                let cw = ui.available_width() - 4.0;
                if let Some(d) = file_input_card(
                    ui,
                    &mut s.input_file,
                    t!(lang, DropFileHere),
                    ACCENT,
                    cw,
                    70.0,
                    lang,
                    &[(t!(lang, FilterCsv), &["csv", "tsv", "txt"])],
                    egui::Id::new("stats_input"),
                ) {
                    dialogs.push(d);
                }
            });
        col.min_rect().height()
    };

    ui.allocate_space(egui::vec2(ui.available_width(), left_h.max(right_h)));

    show_results(ui, s, lang);
    log_panel(ui, &s.logs, lang);
    let _ = dialogs;
}
fn show_results(ui: &mut egui::Ui, s: &mut StatsState, lang: Lang) {
    let Some(ref stats) = s.stats else { return };

    let remaining_h = ui.available_height().max(250.0);
    section_frame(ui, t!(lang, SectionResult), |ui| {
        let col_names: Vec<&str> = stats.iter().map(|c| c.name.as_str()).collect();
        if col_names.is_empty() {
            return;
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(t!(lang, SelectColumn)).color(TEXT_BRIGHT));
            let combo_w = (ui.available_width() - 20.0).clamp(120.0, 400.0);
            ui.add(
                egui::TextEdit::singleline(&mut s.col_search)
                    .desired_width(combo_w)
                    .hint_text(t!(lang, SearchColumn)),
            );
        });
        let search_lower = s.col_search.to_lowercase();
        let filtered: Vec<(usize, &str)> = col_names
            .iter()
            .enumerate()
            .filter(|(_, n)| search_lower.is_empty() || n.to_lowercase().contains(&search_lower))
            .map(|(i, n)| (i, *n))
            .collect();
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .id_salt("stats_col_list")
            .show(ui, |ui| {
                for (i, name) in &filtered {
                    let selected = s.selected_col == *i;
                    if ui
                        .selectable_label(
                            selected,
                            egui::RichText::new(*name).size(11.0).color(if selected {
                                ACCENT
                            } else {
                                TEXT_BRIGHT
                            }),
                        )
                        .clicked()
                    {
                        s.selected_col = *i;
                        s.col_search.clear();
                    }
                }
            });
        let Some(col) = stats.get(s.selected_col) else {
            return;
        };
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            stat_badge(
                ui,
                "🔢",
                t!(lang, UniqueValues),
                &col.unique_count.to_string(),
            );
            stat_badge(
                ui,
                "⬜",
                t!(lang, EmptyValues),
                &col.empty_count.to_string(),
            );
            stat_badge(ui, "📊", t!(lang, Sampled), &col.total_sampled.to_string());
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .min_scrolled_height(remaining_h - 100.0)
            .show(ui, |ui| {
                egui::Grid::new("stats_grid")
                    .striped(true)
                    .min_col_width(60.0)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(t!(lang, ColValue))
                                .strong()
                                .color(TEXT_BRIGHT),
                        );
                        ui.label(
                            egui::RichText::new(t!(lang, ColCount))
                                .strong()
                                .color(TEXT_BRIGHT),
                        );
                        ui.label(
                            egui::RichText::new(t!(lang, ColPercent))
                                .strong()
                                .color(TEXT_BRIGHT),
                        );
                        ui.end_row();
                        for (val, count) in &col.top_values {
                            let pct = if col.total_sampled > 0 {
                                *count as f64 / col.total_sampled as f64 * 100.0
                            } else {
                                0.0
                            };
                            let display = if val.chars().count() > 30 {
                                format!("{}...", val.chars().take(27).collect::<String>())
                            } else {
                                val.clone()
                            };
                            ui.label(egui::RichText::new(&display).color(TEXT_BRIGHT));
                            ui.label(egui::RichText::new(count.to_string()).color(TEXT_BRIGHT));
                            ui.label(
                                egui::RichText::new(format!("{:.2}%", pct)).color(TEXT_BRIGHT),
                            );
                            ui.end_row();
                        }
                    });
            });
    });
}

fn start_stats(s: &mut StatsState) {
    let Some(ref info) = s.input_file else { return };
    s.logs.clear();
    s.state = JobState::Running;
    s.stats = None;
    let path = info.path.clone();
    let delim = get_delimiter(s.delim_idx, &s.custom_delim);
    let sample = s.sample_size;
    let (tx, rx) = mpsc::channel();
    s.rx = Some(rx);
    std::thread::spawn(
        move || match compute_column_stats(&path, delim, sample, 10_000, &tx) {
            Ok(st) => {
                tx.send(ProgressEvent::StatsReady(st)).ok();
            }
            Err(e) => {
                tx.send(ProgressEvent::Error(e.to_string())).ok();
            }
        },
    );
}

fn poll_stats(s: &mut StatsState, lang: Lang) {
    let Some(rx) = &s.rx else { return };
    while let Ok(ev) = rx.try_recv() {
        match ev {
            ProgressEvent::StatsReady(st) => {
                s.stats = Some(st);
                s.state = JobState::Done;
            }
            ProgressEvent::Error(e) => {
                s.logs.push(format!("❌ {}: {}", t!(lang, Error), e));
                s.state = JobState::Error;
            }
            ProgressEvent::Info(msg) => s.logs.push(msg),
            _ => {}
        }
    }
}
