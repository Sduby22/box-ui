use eframe::egui;
use std::sync::Arc;

static DM_MONO: &[u8] = include_bytes!("../assets/fonts/DMMono-Regular.ttf");

pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // DM Mono as primary font — zero-copy from static data
    fonts.font_data.insert(
        "dm_mono".to_owned(),
        Arc::new(egui::FontData::from_static(DM_MONO)),
    );
    if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        list.insert(0, "dm_mono".to_owned());
    }
    if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        list.insert(0, "dm_mono".to_owned());
    }

    // CJK fallback — load system font with only CJK-relevant subset hint.
    // For .ttc (TrueType Collection) files, egui loads only the first face;
    // we set tweak.scale to slightly reduce rasterization overhead.
    let cjk_font_paths: &[&str] = &[
        // macOS
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        // Linux
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        // Windows
        "C:\\Windows\\Fonts\\msyh.ttc",
    ];
    for path in cjk_font_paths {
        if let Ok(font_data) = std::fs::read(path) {
            fonts.font_data.insert(
                "cjk".to_owned(),
                Arc::new(egui::FontData::from_owned(font_data)),
            );
            if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                list.push("cjk".to_owned());
            }
            if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                list.push("cjk".to_owned());
            }
            break;
        }
    }

    ctx.set_fonts(fonts);
}
