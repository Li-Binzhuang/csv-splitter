mod app;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([820.0, 680.0])
            .with_min_inner_size([700.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "CSV Splitter Pro",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
