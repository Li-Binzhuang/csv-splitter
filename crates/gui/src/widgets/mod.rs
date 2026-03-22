mod common;
mod dialog;
mod file_card;

pub use common::*;
pub use dialog::*;
pub use file_card::*;

// ===== Theme colors (centralized) =====
pub const CARD_BG: egui::Color32 = egui::Color32::from_rgb(52, 58, 76);
pub const CARD_BORDER: egui::Color32 = egui::Color32::from_rgb(78, 86, 108);
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(88, 166, 255);
pub const SUCCESS: egui::Color32 = egui::Color32::from_rgb(80, 200, 120);
pub const WARN: egui::Color32 = egui::Color32::from_rgb(255, 180, 60);
pub const ERROR_C: egui::Color32 = egui::Color32::from_rgb(240, 80, 80);
pub const PURPLE: egui::Color32 = egui::Color32::from_rgb(180, 140, 255);
pub const TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(175, 185, 205);
pub const TEXT_BRIGHT: egui::Color32 = egui::Color32::from_rgb(232, 236, 248);

/// Format bytes into human-readable string.
pub fn format_bytes(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.2} GB", b as f64 / (1u64 << 30) as f64)
    } else if b >= 1 << 20 {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    } else if b >= 1 << 10 {
        format!("{:.1} KB", b as f64 / (1u64 << 10) as f64)
    } else {
        format!("{} B", b)
    }
}

/// Get delimiter from index + custom string.
pub fn get_delimiter(idx: usize, custom: &str) -> csv_splitter_core::Delimiter {
    let common = csv_splitter_core::Delimiter::common();
    if idx < common.len() {
        common[idx]
    } else {
        let b = custom.as_bytes().first().copied().unwrap_or(b',');
        csv_splitter_core::Delimiter::Custom(b)
    }
}
