use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Shadow, Stroke, Style, Visuals,
};

/// Apply a polished modern theme with Chinese font support.
pub fn apply_theme(ctx: &egui::Context) {
    setup_fonts(ctx);
    setup_visuals(ctx);
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    // Load system CJK font for Chinese/Japanese/Korean support.
    // Multiple candidates per platform — first match wins.
    let cjk_font_paths: Vec<&str> = if cfg!(target_os = "macos") {
        vec![
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/STHeiti Medium.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/System/Library/Fonts/Supplemental/Songti.ttc",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            "/Library/Fonts/Arial Unicode.ttf",
        ]
    } else if cfg!(target_os = "windows") {
        vec![
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\msyhbd.ttc",
            "C:\\Windows\\Fonts\\simsun.ttc",
            "C:\\Windows\\Fonts\\simhei.ttf",
        ]
    } else {
        vec![
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
            "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
        ]
    };

    for path in cjk_font_paths {
        if let Ok(data) = std::fs::read(path) {
            fonts.font_data.insert(
                "cjk".to_owned(),
                std::sync::Arc::new(FontData::from_owned(data)),
            );
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("cjk".to_owned());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push("cjk".to_owned());
            break;
        }
    }

    ctx.set_fonts(fonts);
}

fn setup_visuals(ctx: &egui::Context) {
    let mut style = Style::default();
    let visuals = &mut style.visuals;

    *visuals = Visuals::dark();

    let panel_bg = Color32::from_rgb(32, 36, 46);
    let bg = Color32::from_rgb(26, 29, 38);
    let widget_bg = Color32::from_rgb(50, 55, 68);
    let accent = Color32::from_rgb(88, 166, 255);
    let accent_hover = Color32::from_rgb(110, 180, 255);
    let text_primary = Color32::from_rgb(230, 235, 245);
    let text_secondary = Color32::from_rgb(180, 188, 205);
    let border = Color32::from_rgb(62, 68, 85);
    let success = Color32::from_rgb(80, 200, 120);
    let error = Color32::from_rgb(240, 80, 80);

    let rounding = CornerRadius::same(6);

    visuals.panel_fill = panel_bg;
    visuals.window_fill = panel_bg;
    visuals.extreme_bg_color = bg;
    visuals.faint_bg_color = Color32::from_rgb(44, 48, 60);

    visuals.widgets.noninteractive.bg_fill = widget_bg;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text_secondary);
    visuals.widgets.noninteractive.corner_radius = rounding;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);

    visuals.widgets.inactive.bg_fill = widget_bg;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, text_primary);
    visuals.widgets.inactive.corner_radius = rounding;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, border);

    visuals.widgets.hovered.bg_fill = Color32::from_rgb(50, 55, 70);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, accent_hover);
    visuals.widgets.hovered.corner_radius = rounding;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, accent);

    visuals.widgets.active.bg_fill = accent;
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.active.corner_radius = rounding;

    visuals.widgets.open.bg_fill = Color32::from_rgb(50, 55, 70);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, accent);
    visuals.widgets.open.corner_radius = rounding;

    visuals.selection.bg_fill = accent.linear_multiply(0.3);
    visuals.selection.stroke = Stroke::new(1.0, accent);

    visuals.window_shadow = Shadow::NONE;
    visuals.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(80),
    };

    visuals.striped = true;

    style.spacing.item_spacing = [8.0, 6.0].into();
    style.spacing.button_padding = [12.0, 6.0].into();
    style.spacing.window_margin = 16.0.into();

    ctx.set_style(style);

    // Store custom colors for use in tabs
    ctx.data_mut(|d| {
        d.insert_temp(egui::Id::new("color_accent"), accent);
        d.insert_temp(egui::Id::new("color_success"), success);
        d.insert_temp(egui::Id::new("color_error"), error);
        d.insert_temp(egui::Id::new("color_text_secondary"), text_secondary);
    });
}
