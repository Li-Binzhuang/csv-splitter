use crate::state::{JobState, StatsState};
use crate::widgets::*;
use csv_splitter_core::preview::compute_column_stats;
use csv_splitter_core::ProgressEvent;
use std::path::PathBuf;
use std::sync::mpsc;

pub fn ui_stats(ui: &mut egui::Ui, s: &mut StatsState) {
    poll_stats(s);

    ui.heading("📊 列统计");
    ui.label(egui::RichText::new("通过采样分析各列的数据分布").size(12.0).weak());
    ui.add_space(6.0);

    file_picker(
        ui, "CSV 文件:", &mut s.path,
        &[("CSV", &["csv", "tsv", "txt"])],
    );
    delimiter_combo(ui, &mut s.delim_idx, &mut s.custom_delim);

    ui.horizontal(|ui| {
        ui.label("采样行数:");
        ui.add(egui::DragValue::new(&mut s.sample_size).range(1000..=10_000_000));
    });

    ui.add_space(4.0);
    let can_start = s.state != JobState::Running && !s.path.is_empty();
    ui.horizontal(|ui| {
        let btn = egui::Button::new("📊 开始分析").min_size([120.0, 28.0].into());
        if ui.add_enabled(can_start, btn).clicked() {
            start_stats(s);
        }
        job_status(ui, &s.state);
    });

    if let Some(ref stats) = s.stats {
        section(ui, "📈 分析结果");

        let col_names: Vec<&str> = stats.iter().map(|s| s.name.as_str()).collect();
        if !col_names.is_empty() {
            ui.horizontal(|ui| {
                ui.label("选择列:");
                egui::ComboBox::from_id_salt("stats_col")
                    .selected_text(*col_names.get(s.selected_col).unwrap_or(&""))
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for (i, name) in col_names.iter().enumerate() {
                            ui.selectable_value(&mut s.selected_col, i, *name);
                        }
                    });
            });

            if let Some(col) = stats.get(s.selected_col) {
                ui.add_space(4.0);

                // Stats summary bar
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(format!("🔢 唯一值: {}", col.unique_count));
                            ui.separator();
                            ui.label(format!("⬜ 空值: {}", col.empty_count));
                            ui.separator();
                            ui.label(format!("📊 采样: {}", col.total_sampled));
                        });
                    });

                ui.add_space(4.0);

                // Value frequency table
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Grid::new("stats_grid")
                            .striped(true)
                            .min_col_width(100.0)
                            .spacing([16.0, 4.0])
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new("值").strong());
                                ui.label(egui::RichText::new("计数").strong());
                                ui.label(egui::RichText::new("占比").strong());
                                ui.end_row();

                                for (val, count) in &col.top_values {
                                    let pct = if col.total_sampled > 0 {
                                        *count as f64 / col.total_sampled as f64 * 100.0
                                    } else {
                                        0.0
                                    };
                                    let display = if val.len() > 60 {
                                        format!("{}...", &val[..57])
                                    } else {
                                        val.clone()
                                    };
                                    ui.label(&display);
                                    ui.label(count.to_string());
                                    ui.label(format!("{:.2}%", pct));
                                    ui.end_row();
                                }
                            });
                    });
            }
        }
    }

    log_panel(ui, &s.logs);
}

fn start_stats(s: &mut StatsState) {
    s.logs.clear();
    s.state = JobState::Running;
    s.stats = None;
    let path = PathBuf::from(&s.path);
    let delim = get_delimiter(s.delim_idx, &s.custom_delim);
    let sample = s.sample_size;
    let (tx, rx) = mpsc::channel();
    s.rx = Some(rx);
    std::thread::spawn(move || {
        match compute_column_stats(&path, delim, sample, 10_000, &tx) {
            Ok(st) => { tx.send(ProgressEvent::StatsReady(st)).ok(); }
            Err(e) => { tx.send(ProgressEvent::Error(e.to_string())).ok(); }
        }
    });
}

fn poll_stats(s: &mut StatsState) {
    let Some(rx) = &s.rx else { return };
    while let Ok(ev) = rx.try_recv() {
        match ev {
            ProgressEvent::StatsReady(st) => {
                s.stats = Some(st);
                s.state = JobState::Done;
            }
            ProgressEvent::Error(e) => {
                s.logs.push(format!("❌ 错误: {}", e));
                s.state = JobState::Error;
            }
            ProgressEvent::Info(msg) => s.logs.push(msg),
            _ => {}
        }
    }
}
