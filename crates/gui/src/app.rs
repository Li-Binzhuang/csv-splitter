use crate::i18n::Lang;
use crate::state::*;
use crate::tabs;
use crate::widgets::{self, PendingDialog};

pub struct App {
    pub lang: Lang,
    pub tab: Tab,
    pub split: SplitState,
    pub preview: PreviewState,
    pub stats: StatsState,
    pub decomp: DecompState,
    pub pending_dialogs: Vec<PendingDialog>,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            lang: Lang::Zh,
            tab: Tab::Split,
            split: SplitState::default(),
            preview: PreviewState::default(),
            stats: StatsState::default(),
            decomp: DecompState::default(),
            pending_dialogs: Vec::new(),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll async file dialogs
        for result in widgets::poll_dialogs(&mut self.pending_dialogs) {
            match result {
                widgets::DialogResult::File(id, info) => {
                    if id == egui::Id::new("split_input") {
                        self.split.input_file = info;
                    } else if id == egui::Id::new("split_columns") {
                        self.split.columns_file = info;
                    } else if id == egui::Id::new("preview_input") {
                        self.preview.input_file = info;
                    } else if id == egui::Id::new("stats_input") {
                        self.stats.input_file = info;
                    } else if id == egui::Id::new("decomp_input") {
                        self.decomp.input_file = info;
                    }
                }
                widgets::DialogResult::Dir(id, path) => {
                    if id == egui::Id::new("decomp_output_dir") {
                        self.decomp.output_dir = path.display().to_string();
                    }
                }
                widgets::DialogResult::SaveCopy { src, dest } => {
                    std::fs::copy(&src, &dest).ok();
                }
            }
        }

        if !self.pending_dialogs.is_empty() {
            ctx.request_repaint();
        }

        if self.split.state == JobState::Running
            || self.stats.state == JobState::Running
            || self.decomp.state == JobState::Running
            || self.tab == Tab::Split
        {
            ctx.request_repaint();
        }
        self.split.anim_time = ctx.input(|i| i.time);

        self.handle_drops(ctx);

        let lang = self.lang;

        // --- Tab bar ---
        egui::TopBottomPanel::top("tab_bar")
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(36, 40, 52))
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(58, 64, 80))),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for &t in Tab::all() {
                        let selected = self.tab == t;
                        let label = t.label(lang);
                        let text = egui::RichText::new(label).size(14.0);
                        let text = if selected {
                            text.strong().color(egui::Color32::from_rgb(88, 166, 255))
                        } else {
                            text.color(egui::Color32::from_rgb(190, 198, 215))
                        };
                        if ui.selectable_label(selected, text).clicked() {
                            self.tab = t;
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn_text = match self.lang {
                            Lang::Zh => "🌐 EN",
                            Lang::En => "🌐 中文",
                        };
                        if ui
                            .button(egui::RichText::new(btn_text).size(13.0))
                            .on_hover_text("Switch language / 切换语言")
                            .clicked()
                        {
                            self.lang = self.lang.toggle();
                        }
                    });
                });
            });

        // --- Content ---
        let mut new_dialogs: Vec<PendingDialog> = Vec::new();
        egui::CentralPanel::default().show(ctx, |ui| {
            let avail = ui.available_size();
            ui.set_min_size(avail);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(avail.x - 16.0);
                    ui.add_space(8.0);
                    match self.tab {
                        Tab::Split => tabs::ui_split(ui, &mut self.split, lang, &mut new_dialogs),
                        Tab::Preview => {
                            tabs::ui_preview(ui, &mut self.preview, lang, &mut new_dialogs)
                        }
                        Tab::Stats => tabs::ui_stats(ui, &mut self.stats, lang, &mut new_dialogs),
                        Tab::Decompress => {
                            tabs::ui_decompress(ui, &mut self.decomp, lang, &mut new_dialogs)
                        }
                    }
                });
        });
        self.pending_dialogs.extend(new_dialogs);
    }
}

impl App {
    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<_> = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            if let Some(path) = file.path {
                if let Some(info) = FileInfo::from_path(path) {
                    match self.tab {
                        Tab::Split => {
                            self.split.input_file = Some(info);
                        }
                        Tab::Preview => {
                            self.preview.input_file = Some(info);
                        }
                        Tab::Stats => {
                            self.stats.input_file = Some(info);
                        }
                        Tab::Decompress => {
                            self.decomp.input_file = Some(info);
                        }
                    }
                }
            }
        }
    }
}
