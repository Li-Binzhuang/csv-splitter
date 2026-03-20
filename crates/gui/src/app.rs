use crate::i18n::Lang;
use crate::state::*;
use crate::tabs;

pub struct App {
    pub lang: Lang,
    tab: Tab,
    split: SplitState,
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
        if self.split.state == JobState::Running
            || self.stats.state == JobState::Running
            || self.decomp.state == JobState::Running
        {
            ctx.request_repaint();
        }

        let lang = self.lang;

        // --- Tab bar ---
        egui::TopBottomPanel::top("tab_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                for &t in Tab::all() {
                    let selected = self.tab == t;
                    let text = egui::RichText::new(t.label(lang)).size(14.0);
                    let text = if selected { text.strong() } else { text };
                    if ui.selectable_label(selected, text).clicked() {
                        self.tab = t;
                    }
                    ui.add_space(4.0);
                }

                // Push language toggle to the right
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let toggle_text = match self.lang {
                        Lang::Zh => "🌐 EN",
                        Lang::En => "🌐 中文",
                    };
                    if ui.button(toggle_text).on_hover_text("Switch language / 切换语言").clicked() {
                        self.lang = self.lang.toggle();
                    }
                    ui.add_space(4.0);
                });
            });
            ui.add_space(2.0);
        });

        // --- Content ---
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
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
