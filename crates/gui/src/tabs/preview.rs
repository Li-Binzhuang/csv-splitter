use crate::i18n::{t, Lang};
use crate::state::PreviewState;
use crate::widgets::*;
use csv_splitter_core::preview::load_preview;

pub fn ui_preview(
    ui: &mut egui::Ui,
    s: &mut PreviewState,
    lang: Lang,
    dialogs: &mut Vec<PendingDialog>,
) {
    ui.vertical_centered(|ui| {
        ui.heading(egui::RichText::new(t!(lang, PreviewTitle)).color(TEXT_BRIGHT));
        ui.label(
            egui::RichText::new(t!(lang, PreviewDesc))
                .size(12.0)
                .color(TEXT_DIM),
        );
    });
    ui.add_space(8.0);

    let total_w = ui.available_width();
    let gap = 8.0_f32;
    let left_w = ((total_w - gap) * 0.5).max(100.0);
    let right_w = (total_w - gap - left_w).max(100.0);
    let top_y = ui.cursor().min.y;
    let base_x = ui.cursor().min.x;
    let left_rect = egui::Rect::from_min_size(egui::pos2(base_x, top_y), egui::vec2(left_w, 10.0));
    let right_rect = egui::Rect::from_min_size(
        egui::pos2(base_x + left_w + gap, top_y),
        egui::vec2(right_w, 10.0),
    );

    let right_h = {
        let mut col = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("preview_right")
                .max_rect(right_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        col.set_max_width(right_w);
        col.set_width(right_w);
        draw_params_frame(&mut col, s, lang, right_w);
        col.min_rect().height()
    };

    let left_h = {
        let mut col = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("preview_left")
                .max_rect(left_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        col.set_max_width(left_w);
        col.set_width(left_w);
        draw_input_frame(&mut col, s, lang, dialogs, left_w, right_h);
        col.min_rect().height()
    };

    ui.allocate_space(egui::vec2(total_w, left_h.max(right_h)));
    draw_result(ui, s, lang);
}

fn draw_input_frame(
    ui: &mut egui::Ui,
    s: &mut PreviewState,
    lang: Lang,
    dialogs: &mut Vec<PendingDialog>,
    w: f32,
    match_h: f32,
) {
    let inner_w = w - 30.0;
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, CARD_BORDER))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(14.0)
        .outer_margin(egui::Margin::symmetric(0, 4))
        .show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.set_min_height((match_h - 40.0).max(60.0));
            ui.label(
                egui::RichText::new(t!(lang, SectionInput))
                    .size(13.0)
                    .strong()
                    .color(TEXT_BRIGHT),
            );
            ui.add_space(6.0);
            let card_w = (inner_w - 4.0).max(80.0);
            if let Some(d) = file_input_card(
                ui,
                &mut s.input_file,
                t!(lang, DropFileHere),
                ACCENT,
                card_w,
                70.0,
                lang,
                &[(t!(lang, FilterCsv), &["csv", "tsv", "txt"])],
                egui::Id::new("preview_input"),
            ) {
                dialogs.push(d);
            }
        });
}

fn draw_params_frame(ui: &mut egui::Ui, s: &mut PreviewState, lang: Lang, w: f32) {
    let inner_w = w - 30.0;
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, CARD_BORDER))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(14.0)
        .outer_margin(egui::Margin::symmetric(0, 4))
        .show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.label(
                egui::RichText::new(t!(lang, SectionParams))
                    .size(13.0)
                    .strong()
                    .color(TEXT_BRIGHT),
            );
            ui.add_space(6.0);
            delimiter_combo(ui, &mut s.delim_idx, &mut s.custom_delim, lang);
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(t!(lang, PreviewRows)).color(TEXT_BRIGHT));
                ui.add(egui::DragValue::new(&mut s.preview_rows).range(10..=10_000));
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(t!(lang, FromRow)).color(TEXT_BRIGHT));
                ui.add(egui::DragValue::new(&mut s.from_row).range(1..=s.preview_rows));
                ui.label(egui::RichText::new(t!(lang, ToRow)).color(TEXT_BRIGHT));
                ui.add(egui::DragValue::new(&mut s.to_row).range(s.from_row..=s.preview_rows));
            });
            ui.horizontal(|ui| {
                let max_col = s.preview.as_ref().map(|p| p.headers.len()).unwrap_or(9999);
                ui.label(egui::RichText::new(t!(lang, FromCol)).color(TEXT_BRIGHT));
                ui.add(egui::DragValue::new(&mut s.from_col).range(1..=max_col));
                ui.label(egui::RichText::new(t!(lang, ToCol)).color(TEXT_BRIGHT));
                ui.add(egui::DragValue::new(&mut s.to_col).range(s.from_col..=max_col));
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(t!(lang, FreezeRow)).color(TEXT_BRIGHT));
                ui.add(egui::DragValue::new(&mut s.freeze_row).range(0..=1));
                ui.label(egui::RichText::new(t!(lang, FreezeCol)).color(TEXT_BRIGHT));
                let max_col = s.preview.as_ref().map(|p| p.headers.len()).unwrap_or(9999);
                ui.add(egui::DragValue::new(&mut s.freeze_col).range(0..=max_col));
            });
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            let can_load = s.input_file.is_some();
            if ui
                .add_enabled(
                    can_load,
                    egui::Button::new(egui::RichText::new(t!(lang, LoadPreview)).size(14.0))
                        .min_size([120.0, 32.0].into()),
                )
                .clicked()
            {
                if let Some(ref info) = s.input_file {
                    let delim = get_delimiter(s.delim_idx, &s.custom_delim);
                    match load_preview(&info.path, s.preview_rows, Some(delim)) {
                        Ok(p) => {
                            s.from_row = 1;
                            s.to_row = p.rows.len();
                            s.from_col = 1;
                            s.to_col = p.headers.len();
                            s.preview = Some(p);
                        }
                        Err(_) => s.preview = None,
                    }
                }
            }
        });
}

fn draw_result(ui: &mut egui::Ui, s: &PreviewState, lang: Lang) {
    let Some(ref preview) = s.preview else {
        return;
    };
    let remaining_h = ui.available_height().max(300.0);
    let num_rows = preview.rows.len();
    let num_cols = preview.headers.len();
    let r0 = s.from_row.saturating_sub(1).min(num_rows);
    let r1 = s.to_row.min(num_rows);
    let c0 = s.from_col.saturating_sub(1).min(num_cols);
    let c1 = s.to_col.min(num_cols);
    let fr = s.freeze_row.min(1);
    let fc = s.freeze_col.min(c1.saturating_sub(c0));

    section_frame(ui, t!(lang, SectionResult), |ui| {
        ui.horizontal_wrapped(|ui| {
            stat_badge(
                ui,
                "\u{1F4C4}",
                t!(lang, FileSize),
                &format_bytes(preview.file_size),
            );
            stat_badge(
                ui,
                "\u{1F4CA}",
                t!(lang, ApproxRows),
                &format!("~{}", preview.total_rows_estimate),
            );
            stat_badge(
                ui,
                "\u{1F524}",
                t!(lang, DetectedDelim),
                preview.detected_delimiter.label(),
            );
            stat_badge(
                ui,
                "\u{1F4CB}",
                t!(lang, ColumnCount),
                &num_cols.to_string(),
            );
        });
        ui.add_space(6.0);

        let hdrs = &preview.headers[c0..c1];
        let rows = &preview.rows[r0..r1];
        let cw = 60.0_f32;
        let cs = 8.0_f32;
        let th = remaining_h - 80.0;

        if fc == 0 && fr == 0 {
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .min_scrolled_height(th)
                .show(ui, |ui| {
                    simple_grid("pv_grid", ui, hdrs, rows, c0, c1, cw, cs, TEXT_BRIGHT);
                });
        } else {
            frozen_grid(ui, hdrs, rows, c0, c1, fc, fr, cw, cs, th);
        }
    });
}

fn simple_grid(
    id: &str,
    ui: &mut egui::Ui,
    hdrs: &[String],
    rows: &[Vec<String>],
    c0: usize,
    c1: usize,
    cw: f32,
    cs: f32,
    color: egui::Color32,
) {
    egui::Grid::new(id)
        .striped(true)
        .min_col_width(cw)
        .spacing([cs, 3.0])
        .show(ui, |ui| {
            for h in hdrs {
                ui.label(egui::RichText::new(h).strong().size(12.0).color(color));
            }
            ui.end_row();
            for row in rows {
                for ci in c0..c1 {
                    let v = row.get(ci).map(|s| s.as_str()).unwrap_or("");
                    ui.label(egui::RichText::new(trunc(v)).size(11.0).color(color));
                }
                ui.end_row();
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn frozen_grid(
    ui: &mut egui::Ui,
    hdrs: &[String],
    rows: &[Vec<String>],
    c0: usize,
    c1: usize,
    fc: usize,
    fr: usize,
    cw: f32,
    cs: f32,
    th: f32,
) {
    let frozen_w = (fc as f32) * (cw + cs);
    let hdr_h = if fr > 0 { 22.0 } else { 0.0 };
    let data_h = th - hdr_h;

    // Read the main scroll area offset from previous frame
    let sync_id = egui::Id::new("pv_sync_offset");
    let prev_offset: egui::Vec2 = ui
        .ctx()
        .data(|d| d.get_temp::<egui::Vec2>(sync_id))
        .unwrap_or(egui::Vec2::ZERO);

    // === Frozen header row ===
    if fr > 0 {
        ui.horizontal(|ui| {
            // Top-left corner: frozen col+row intersection (completely fixed)
            if fc > 0 {
                ui.horizontal(|ui| {
                    ui.set_width(frozen_w);
                    for h in &hdrs[..fc] {
                        ui.add_sized(
                            [cw, hdr_h],
                            egui::Label::new(
                                egui::RichText::new(h).strong().size(12.0).color(ACCENT),
                            ),
                        );
                    }
                });
            }
            // Top-right: header labels, clipped and offset horizontally to sync with main
            let avail_w = ui.available_width();
            let (clip_rect, _) =
                ui.allocate_exact_size(egui::vec2(avail_w, hdr_h), egui::Sense::hover());
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("pv_hdr_clip")
                    .max_rect(egui::Rect::from_min_size(
                        clip_rect.min - egui::vec2(prev_offset.x, 0.0),
                        egui::vec2(f32::INFINITY, hdr_h),
                    ))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.set_clip_rect(clip_rect);
            for h in &hdrs[fc..] {
                child.add_sized(
                    [cw, hdr_h],
                    egui::Label::new(
                        egui::RichText::new(h)
                            .strong()
                            .size(12.0)
                            .color(TEXT_BRIGHT),
                    ),
                );
            }
        });
    }

    // === Data rows ===
    ui.horizontal(|ui| {
        // Bottom-left: frozen columns, clipped and offset vertically to sync with main
        if fc > 0 {
            let (clip_rect, _) =
                ui.allocate_exact_size(egui::vec2(frozen_w, data_h), egui::Sense::hover());
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("pv_fc_clip")
                    .max_rect(egui::Rect::from_min_size(
                        clip_rect.min - egui::vec2(0.0, prev_offset.y),
                        egui::vec2(frozen_w, f32::INFINITY),
                    ))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            child.set_clip_rect(clip_rect);
            egui::Grid::new("pv_fc_grid")
                .striped(true)
                .min_col_width(cw)
                .spacing([cs, 3.0])
                .show(&mut child, |ui| {
                    for row in rows {
                        for ci in c0..(c0 + fc) {
                            let v = row.get(ci).map(|s| s.as_str()).unwrap_or("");
                            ui.label(egui::RichText::new(trunc(v)).size(11.0).color(ACCENT));
                        }
                        ui.end_row();
                    }
                });
        }

        // Bottom-right: main scrollable data area
        let main_out = egui::ScrollArea::both()
            .id_salt("pv_main_scroll")
            .auto_shrink([false, false])
            .max_height(data_h)
            .show(ui, |ui| {
                egui::Grid::new("pv_main_grid")
                    .striped(true)
                    .min_col_width(cw)
                    .spacing([cs, 3.0])
                    .show(ui, |ui| {
                        if fr == 0 {
                            for h in &hdrs[fc..] {
                                ui.label(
                                    egui::RichText::new(h)
                                        .strong()
                                        .size(12.0)
                                        .color(TEXT_BRIGHT),
                                );
                            }
                            ui.end_row();
                        }
                        for row in rows {
                            for ci in (c0 + fc)..c1 {
                                let v = row.get(ci).map(|s| s.as_str()).unwrap_or("");
                                ui.label(
                                    egui::RichText::new(trunc(v)).size(11.0).color(TEXT_BRIGHT),
                                );
                            }
                            ui.end_row();
                        }
                    });
            });

        // Store main scroll offset so frozen panes sync on next frame
        let offset = main_out.state.offset;
        ui.ctx().data_mut(|d| d.insert_temp(sync_id, offset));
    });
}

fn trunc(val: &str) -> String {
    if val.chars().count() > 40 {
        let t: String = val.chars().take(38).collect();
        format!("{}\u{2026}", t)
    } else {
        val.to_string()
    }
}
