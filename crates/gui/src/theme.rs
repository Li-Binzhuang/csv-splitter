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

    // ── Palette: warm slate-blue, NOT black ──
    let panel_bg = Color32::from_rgb(40, 44, 58); // main background
    let window_bg = Color32::from_rgb(44, 48, 62);
    let extreme_bg = Color32::from_rgb(36, 40, 52); // log area, text inputs
    let faint_bg = Color32::from_rgb(50, 55, 70);
    let widget_bg = Color32::from_rgb(58, 64, 82); // buttons, combos
    let border = Color32::from_rgb(72, 80, 100);
    let accent = Color32::from_rgb(88, 166, 255);
    let accent_hover = Color32::from_rgb(115, 185, 255);
    let text_primary = Color32::from_rgb(235, 238, 248);
    let text_secondary = Color32::from_rgb(190, 198, 215);

    let rounding = CornerRadius::same(10);

    visuals.panel_fill = panel_bg;
    visuals.window_fill = window_bg;
    visuals.extreme_bg_color = extreme_bg;
    visuals.faint_bg_color = faint_bg;

    visuals.widgets.noninteractive.bg_fill = widget_bg;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text_secondary);
    visuals.widgets.noninteractive.corner_radius = rounding;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);

    visuals.widgets.inactive.bg_fill = widget_bg;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, text_primary);
    visuals.widgets.inactive.corner_radius = rounding;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, border);

    visuals.widgets.hovered.bg_fill = Color32::from_rgb(65, 72, 92);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, accent_hover);
    visuals.widgets.hovered.corner_radius = rounding;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, accent);

    visuals.widgets.active.bg_fill = accent;
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.active.corner_radius = rounding;

    visuals.widgets.open.bg_fill = Color32::from_rgb(60, 66, 85);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, accent);
    visuals.widgets.open.corner_radius = rounding;

    visuals.selection.bg_fill = accent.linear_multiply(0.25);
    visuals.selection.stroke = Stroke::new(1.0, accent);

    visuals.window_shadow = Shadow::NONE;
    visuals.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 16,
        spread: 2,
        color: Color32::from_black_alpha(60),
    };

    visuals.striped = true;

    // ── Spacing: generous, bouncy feel ──
    style.spacing.item_spacing = [10.0, 8.0].into();
    style.spacing.button_padding = [14.0, 7.0].into();
    style.spacing.window_margin = 18.0.into();

    ctx.set_style(style);
}
