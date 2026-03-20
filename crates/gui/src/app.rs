use crate::i18n::Lang;
use crate::state::*;
use crate::tabs;

pub struct App {
    pub lang: Lang,
    tab: Tab,
    pub split: SplitState,
    preview: PreviewState,
    stats: StatsState,
    decomp: DecompState,
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
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Animate continuously when running or on split tab (for flow animation)
        if self.split.state == JobState::Running
            || self.stats.state == JobState::Running
            || self.decomp.state == JobState::Running
            || self.tab == Tab::Split
        {
            ctx.request_repaint();
        }

        // Track animation time
        self.split.anim_time = ctx.input(|i| i.time);

        // Handle file drops globally
        self.handle_drops(ctx);

        let lang = self.lang;

        // --- Top bar with tabs + language toggle ---
        egui::TopBottomPanel::top("tab_bar")
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(24, 26, 33))
                    .inner_margin(egui::Margin::symmetric(12, 6)),
            )
            .show(ctx, |ui| {
                let total_w = ui.available_width();
                ui.horizontal(|ui| {
                    // Tabs on the left
                    for &t in Tab::all() {
                        let selected = self.tab == t;
                        let text = egui::RichText::new(t.label(lang)).size(14.0);
                        let text = if selected {
                            text.strong().color(egui::Color32::from_rgb(88, 166, 255))
                        } else {
                            text
                        };
                        if ui.selectable_label(selected, text).clicked() {
                            self.tab = t;
                        }
                        ui.add_space(2.0);
                    }

                    // Push language toggle to the right using remaining space
                    let remaining = total_w - ui.min_rect().width() - 80.0;
                    if remaining > 0.0 {
                        ui.add_space(remaining);
                    }

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

        // --- Content ---
        egui::CentralPanel::default().show(ctx, |ui| {
            let avail = ui.available_size();
            // Make content fill available space
            ui.set_min_size(avail);

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(avail.x - 16.0);
                    ui.add_space(4.0);
                    match self.tab {
                        Tab::Split => tabs::ui_split(ui, &mut self.split, lang),
                        Tab::Preview => tabs::ui_preview(ui, &mut self.preview, lang),
                        Tab::Stats => tabs::ui_stats(ui, &mut self.stats, lang),
                        Tab::Decompress => tabs::ui_decompress(ui, &mut self.decomp, lang),
                    }
                });
        });
    }
}

impl App {
    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<_> = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            if let Some(path) = file.path {
                if self.tab == Tab::Split {
                    if let Some(info) = FileInfo::from_path(path) {
                        let ext = info.extension.to_lowercase();
                        // If it looks like a column filter file (small txt/csv), put it in columns slot
                        // Otherwise put it in input slot
                        if self.split.input_file.is_some()
                            && self.split.columns_file.is_none()
                            && (ext == "txt" || ext == "csv")
                            && info.size < 1_000_000
                        {
                            self.split.columns_file = Some(info);
                        } else {
                            self.split.input_file = Some(info);
                        }
                    }
                } else if self.tab == Tab::Preview {
                    self.preview.path = path.display().to_string();
                } else if self.tab == Tab::Stats {
                    self.stats.path = path.display().to_string();
                } else if self.tab == Tab::Decompress {
                    self.decomp.input = path.display().to_string();
                }
            }
        }
    }
}
