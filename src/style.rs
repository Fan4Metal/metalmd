//! Colors, fonts and egui visuals.

use crate::fonts::FontSpec;
use crate::markdown::document::{AlertKind, Scripts};
use crate::settings::DarkStyle;
use egui::{Color32, FontData, FontDefinitions, FontFamily, Theme, Visuals};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Body text size in points unless the reader chose another.
pub const DEFAULT_BODY_SIZE: f32 = 16.0;
/// The sizes the Text panel offers.
pub const BODY_SIZES: std::ops::RangeInclusive<f32> = 12.0..=24.0;

/// The body text size, as `f32` bits.
static BODY_SIZE: AtomicU32 = AtomicU32::new(DEFAULT_BODY_SIZE.to_bits());

/// Body text size in points. Everything in a document is relative to it;
/// the interface keeps its own sizes.
pub fn body_size() -> f32 {
    f32::from_bits(BODY_SIZE.load(Ordering::Relaxed))
}

/// Sets the body text size (see [`BODY_SIZES`]). Blocks measured at the old
/// size must be measured again: the caller bumps the fonts generation.
pub fn set_body_size(size: f32) {
    let size = if size.is_finite() {
        size.clamp(*BODY_SIZES.start(), *BODY_SIZES.end())
    } else {
        DEFAULT_BODY_SIZE
    };
    BODY_SIZE.store(size.to_bits(), Ordering::Relaxed);
}
/// Highlight colours the Text panel offers, for the light and the dark
/// theme, the theme's own first, with their names.
pub fn mark_presets(dark: bool) -> [(Color32, &'static str); 6] {
    let rgb = Color32::from_rgb;
    if dark {
        [
            (rgb(0x5c, 0x47, 0x0f), tr!("Yellow", "Жёлтый")),
            (rgb(0x1f, 0x4a, 0x2a), tr!("Green", "Зелёный")),
            (rgb(0x1b, 0x3a, 0x5e), tr!("Blue", "Синий")),
            (rgb(0x5a, 0x24, 0x40), tr!("Pink", "Розовый")),
            (rgb(0x5e, 0x3a, 0x12), tr!("Orange", "Оранжевый")),
            (rgb(0x3d, 0x2d, 0x66), tr!("Violet", "Фиолетовый")),
        ]
    } else {
        [
            (rgb(0xff, 0xf3, 0x9c), tr!("Yellow", "Жёлтый")),
            (rgb(0xd3, 0xf5, 0xc9), tr!("Green", "Зелёный")),
            (rgb(0xd4, 0xe9, 0xff), tr!("Blue", "Синий")),
            (rgb(0xff, 0xd9, 0xe8), tr!("Pink", "Розовый")),
            (rgb(0xff, 0xe1, 0xbf), tr!("Orange", "Оранжевый")),
            (rgb(0xe8, 0xdc, 0xff), tr!("Violet", "Фиолетовый")),
        ]
    }
}

/// `#rrggbb` (the `#` may be left out) as a colour.
pub fn parse_hex(text: &str) -> Option<Color32> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(Color32::from_rgb(channel(0)?, channel(2)?, channel(4)?))
}

/// A colour as `#rrggbb`.
pub fn to_hex(color: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

/// Widest the text column gets, in points, before it is centered, unless
/// the reader chose another width.
pub const DEFAULT_CONTENT_WIDTH: f32 = 880.0;
/// The widths the Text panel offers; the last one means no limit.
pub const CONTENT_WIDTHS: std::ops::RangeInclusive<f32> = 560.0..=1600.0;

/// The widest text column, as `f32` bits.
static CONTENT_WIDTH: AtomicU32 = AtomicU32::new(DEFAULT_CONTENT_WIDTH.to_bits());

/// Widest the text column gets, in points; infinite for the whole window.
pub fn content_width() -> f32 {
    f32::from_bits(CONTENT_WIDTH.load(Ordering::Relaxed))
}

/// Sets the widest text column (see [`CONTENT_WIDTHS`]; its end means the
/// whole window). The viewer measures the blocks again on its own.
pub fn set_content_width(width: f32) {
    let width = if width >= *CONTENT_WIDTHS.end() {
        f32::INFINITY
    } else if width.is_finite() {
        width.max(*CONTENT_WIDTHS.start())
    } else {
        DEFAULT_CONTENT_WIDTH
    };
    CONTENT_WIDTH.store(width.to_bits(), Ordering::Relaxed);
}

/// Document colors for one theme: GitHub's palette in the light one; in
/// the dark one the neutral greys of metalmpv or GitHub's, nearly black
/// ([`DarkStyle`]).
#[derive(Clone, Debug)]
pub struct Palette {
    pub dark: bool,
    /// The page, and so panels and windows.
    pub page: Color32,
    /// The tab and find bars.
    pub chrome: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub link: Color32,
    pub code_bg: Color32,
    pub code_block_bg: Color32,
    pub mark_bg: Color32,
    pub border: Color32,
    pub quote_bar: Color32,
    pub search_bg: Color32,
    pub search_current_bg: Color32,
    pub accent: Color32,
}

impl Palette {
    pub fn new(dark: bool, dark_style: DarkStyle) -> Palette {
        let rgb = Color32::from_rgb;
        if !dark {
            return Palette {
                dark,
                page: Color32::WHITE,
                chrome: rgb(0xf6, 0xf8, 0xfa),
                text: rgb(0x1f, 0x23, 0x28),
                muted: rgb(0x59, 0x63, 0x6e),
                link: rgb(0x09, 0x69, 0xda),
                code_bg: rgb(0xec, 0xee, 0xf1),
                code_block_bg: rgb(0xf6, 0xf8, 0xfa),
                mark_bg: mark_presets(false)[0].0,
                border: rgb(0xd1, 0xd9, 0xe0),
                quote_bar: rgb(0xd1, 0xd9, 0xe0),
                search_bg: rgb(0xff, 0xe0, 0x82),
                search_current_bg: rgb(0xff, 0x9f, 0x43),
                accent: rgb(0x09, 0x69, 0xda),
            };
        }
        let blue = rgb(0x44, 0x93, 0xf8);
        let palette = Palette {
            dark,
            page: rgb(0x0d, 0x11, 0x17),
            chrome: rgb(0x01, 0x04, 0x09),
            text: rgb(0xe6, 0xed, 0xf3),
            muted: rgb(0x91, 0x98, 0xa1),
            link: blue,
            code_bg: rgb(0x2a, 0x30, 0x38),
            code_block_bg: rgb(0x15, 0x1b, 0x23),
            mark_bg: mark_presets(true)[0].0,
            border: rgb(0x3d, 0x44, 0x4d),
            quote_bar: rgb(0x3d, 0x44, 0x4d),
            search_bg: rgb(0x6b, 0x4e, 0x0c),
            search_current_bg: rgb(0xb0, 0x60, 0x10),
            accent: blue,
        };
        match dark_style {
            DarkStyle::Black => palette,
            // The bars a shade darker than the page, as the title row of
            // metalmpv.
            DarkStyle::Grey => Palette {
                page: rgb(0x20, 0x22, 0x27),
                chrome: rgb(0x1c, 0x1e, 0x22),
                text: rgb(0xd4, 0xd7, 0xdc),
                muted: rgb(0x8a, 0x8f, 0x98),
                code_bg: rgb(0x33, 0x36, 0x3d),
                code_block_bg: rgb(0x29, 0x2c, 0x32),
                border: rgb(0x3a, 0x3e, 0x46),
                quote_bar: rgb(0x3a, 0x3e, 0x46),
                ..palette
            },
        }
    }

    pub fn alert(&self, kind: AlertKind) -> Color32 {
        match (kind, self.dark) {
            (AlertKind::Note, false) => Color32::from_rgb(0x09, 0x69, 0xda),
            (AlertKind::Note, true) => Color32::from_rgb(0x44, 0x93, 0xf8),
            (AlertKind::Tip, false) => Color32::from_rgb(0x1a, 0x7f, 0x37),
            (AlertKind::Tip, true) => Color32::from_rgb(0x3f, 0xb9, 0x50),
            (AlertKind::Important, false) => Color32::from_rgb(0x82, 0x50, 0xdf),
            (AlertKind::Important, true) => Color32::from_rgb(0xab, 0x7d, 0xf8),
            (AlertKind::Warning, false) => Color32::from_rgb(0x9a, 0x67, 0x00),
            (AlertKind::Warning, true) => Color32::from_rgb(0xd2, 0x99, 0x22),
            (AlertKind::Caution, false) => Color32::from_rgb(0xd1, 0x24, 0x2f),
            (AlertKind::Caution, true) => Color32::from_rgb(0xf8, 0x51, 0x49),
        }
    }
}

/// Applies the app's look to both egui themes, the dark one in
/// `dark_style`.
pub fn apply_visuals(ctx: &egui::Context, dark_style: DarkStyle) {
    for theme in [Theme::Light, Theme::Dark] {
        ctx.style_mut_of(theme, |style| {
            let palette = Palette::new(theme == Theme::Dark, dark_style);
            let mut visuals = if palette.dark {
                Visuals::dark()
            } else {
                Visuals::light()
            };
            visuals.panel_fill = palette.page;
            visuals.window_fill = palette.page;
            visuals.extreme_bg_color = palette.chrome;
            visuals.faint_bg_color = palette.code_block_bg;
            visuals.widgets.noninteractive.fg_stroke.color = palette.text;
            visuals.widgets.noninteractive.bg_stroke.color = palette.border;
            visuals.hyperlink_color = palette.link;
            visuals.selection.bg_fill = if palette.dark {
                Color32::from_rgba_unmultiplied(0x38, 0x8b, 0xfd, 0x66)
            } else {
                Color32::from_rgba_unmultiplied(0x54, 0xae, 0xff, 0x66)
            };
            style.visuals = visuals;
            style.spacing.scroll.floating = true;
            style.interaction.selectable_labels = true;
            style.url_in_tooltip = true;
            use egui::{FontId, TextStyle};
            style
                .text_styles
                .insert(TextStyle::Body, FontId::proportional(14.0));
            style
                .text_styles
                .insert(TextStyle::Button, FontId::proportional(14.0));
            style
                .text_styles
                .insert(TextStyle::Small, FontId::proportional(11.0));
            style
                .text_styles
                .insert(TextStyle::Monospace, FontId::monospace(13.5));
            style
                .text_styles
                .insert(TextStyle::Heading, FontId::proportional(22.0));
        });
    }
}

/// Which font families exist for bold and italic text, and how high their
/// baselines sit.
#[derive(Clone, Debug)]
pub struct FontChoice {
    pub bold: FontFamily,
    pub italic: FontFamily,
    pub bold_italic: FontFamily,
    /// No italic face was found: skew the regular one.
    pub fake_italic: bool,
    /// Ascent (top of row to baseline) per point of font size.
    pub body_ascent: f32,
    pub mono_ascent: f32,
}

impl Default for FontChoice {
    fn default() -> Self {
        FontChoice {
            bold: FontFamily::Proportional,
            italic: FontFamily::Proportional,
            bold_italic: FontFamily::Proportional,
            fake_italic: true,
            body_ascent: 0.9,
            mono_ascent: 0.9,
        }
    }
}

impl FontChoice {
    /// Reads the baseline positions of the loaded fonts. Must run after the
    /// font definitions took effect (they apply at the start of a frame).
    pub fn measure_ascents(&mut self, ctx: &egui::Context) {
        let pixels_per_point = ctx.pixels_per_point();
        let ratio = |family: FontFamily| {
            ctx.fonts_mut(|fonts| {
                fonts
                    .fonts
                    .font(&family)
                    .styled_metrics(pixels_per_point, 100.0, &Default::default())
                    .ascent
                    / 100.0
            })
        };
        self.body_ascent = ratio(FontFamily::Proportional);
        self.mono_ascent = ratio(FontFamily::Monospace);
    }
}

struct FaceFiles {
    regular: &'static [&'static str],
    bold: &'static [&'static str],
    italic: &'static [&'static str],
    bold_italic: &'static [&'static str],
    mono: &'static [&'static str],
    symbols: &'static [&'static str],
}

/// The families used when the reader chose none, as the Text panel names
/// them.
#[cfg(windows)]
pub const DEFAULT_FAMILIES: (&str, &str) = ("Segoe UI", "Consolas");
#[cfg(target_os = "macos")]
pub const DEFAULT_FAMILIES: (&str, &str) = ("Arial", "Courier New");
#[cfg(all(unix, not(target_os = "macos")))]
pub const DEFAULT_FAMILIES: (&str, &str) = ("DejaVu Sans", "DejaVu Sans Mono");

#[cfg(windows)]
const FACES: FaceFiles = FaceFiles {
    regular: &["segoeui.ttf"],
    bold: &["segoeuib.ttf"],
    italic: &["segoeuii.ttf"],
    bold_italic: &["segoeuiz.ttf"],
    mono: &["consola.ttf", "cour.ttf"],
    symbols: &["seguisym.ttf"],
};

#[cfg(target_os = "macos")]
const FACES: FaceFiles = FaceFiles {
    regular: &[
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/Library/Fonts/Arial.ttf",
    ],
    bold: &[
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        "/Library/Fonts/Arial Bold.ttf",
    ],
    italic: &[
        "/System/Library/Fonts/Supplemental/Arial Italic.ttf",
        "/Library/Fonts/Arial Italic.ttf",
    ],
    bold_italic: &[
        "/System/Library/Fonts/Supplemental/Arial Bold Italic.ttf",
        "/Library/Fonts/Arial Bold Italic.ttf",
    ],
    mono: &["/System/Library/Fonts/Supplemental/Courier New.ttf"],
    symbols: &["/System/Library/Fonts/Apple Symbols.ttf"],
};

#[cfg(all(unix, not(target_os = "macos")))]
const FACES: FaceFiles = FaceFiles {
    regular: &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    ],
    bold: &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
    ],
    italic: &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Oblique.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-Oblique.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans-Oblique.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Italic.ttf",
    ],
    bold_italic: &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-BoldOblique.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-BoldOblique.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans-BoldOblique.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-BoldItalic.ttf",
    ],
    mono: &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
    ],
    symbols: &[],
};

pub fn font_path(name: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(windir).join("Fonts").join(name)
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(name)
    }
}

fn read_first(candidates: &[&str]) -> Option<Vec<u8>> {
    candidates
        .iter()
        .find_map(|name| std::fs::read(font_path(name)).ok())
}

fn read_data(candidates: &[&str]) -> Option<FontData> {
    read_first(candidates).map(FontData::from_owned)
}

/// Builds the font set: the reader's fonts or the platform's UI font, with
/// real bold and italic faces, the reader's or the platform's monospace
/// font, then egui's bundled fonts as fallbacks for symbols and emoji. A
/// chosen font whose files are gone gives way to the platform's, missing
/// platform files to egui's defaults.
pub fn load_fonts(
    body: Option<&FontSpec>,
    mono: Option<&FontSpec>,
) -> (FontDefinitions, FontChoice) {
    let mut defs = FontDefinitions::default();
    let mut choice = FontChoice::default();
    let bundled: Vec<String> = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let mut insert = |key: &str, data: Option<FontData>| -> bool {
        match data {
            Some(data) => {
                defs.font_data.insert(key.to_owned(), Arc::new(data));
                true
            }
            None => false,
        }
    };
    // Regular, bold, italic, bold italic: the chosen family's, if its
    // regular face can be read, else the platform's.
    let chosen = body.and_then(|spec| Some((spec, crate::fonts::read(&spec.regular)?)));
    let [regular, bold, italic, bold_italic] = match chosen {
        Some((spec, regular)) => {
            let face = |f: &Option<crate::fonts::FaceRef>| f.as_ref().and_then(crate::fonts::read);
            [
                Some(regular),
                face(&spec.bold),
                face(&spec.italic),
                face(&spec.bold_italic),
            ]
        }
        None => {
            let regular = read_data(FACES.regular);
            let styled = |files| regular.is_some().then(|| read_data(files)).flatten();
            let styles = [
                styled(FACES.bold),
                styled(FACES.italic),
                styled(FACES.bold_italic),
            ];
            let [bold, italic, bold_italic] = styles;
            [regular, bold, italic, bold_italic]
        }
    };
    let has_regular = insert("sys-regular", regular);
    let has_symbols = insert("sys-symbols", read_data(FACES.symbols));
    let mut styled_loaded = Vec::new();
    for (key, data) in [
        ("sys-bold", bold),
        ("sys-italic", italic),
        ("sys-bold-italic", bold_italic),
    ] {
        if has_regular && insert(key, data) {
            styled_loaded.push(key);
        }
    }
    let mono = mono
        .and_then(|spec| crate::fonts::read(&spec.regular))
        .or_else(|| read_data(FACES.mono));
    let has_mono = insert("sys-mono", mono);

    // Every family ends with the same fallbacks: the regular face, symbols,
    // then egui's bundled fonts (emoji).
    let chain = |first: &str| {
        let mut list = vec![first.to_owned()];
        if has_regular && first != "sys-regular" {
            list.push("sys-regular".to_owned());
        }
        if has_symbols {
            list.push("sys-symbols".to_owned());
        }
        list.extend(bundled.iter().cloned());
        list
    };
    if has_regular {
        defs.families
            .insert(FontFamily::Proportional, chain("sys-regular"));
    }
    for &key in &styled_loaded {
        let (name, slot) = match key {
            "sys-bold" => ("bold", &mut choice.bold),
            "sys-italic" => ("italic", &mut choice.italic),
            _ => ("bold_italic", &mut choice.bold_italic),
        };
        let family = FontFamily::Name(name.into());
        defs.families.insert(family.clone(), chain(key));
        *slot = family;
        if key == "sys-italic" {
            choice.fake_italic = false;
        }
    }
    if !styled_loaded.contains(&"sys-bold-italic") {
        // No bold italic face: the bold one slanted where italic is slanted
        // too, else the italic one.
        choice.bold_italic = if choice.fake_italic {
            choice.bold.clone()
        } else {
            choice.italic.clone()
        };
    }
    if has_mono {
        let mut mono = vec!["sys-mono".to_owned()];
        mono.extend(
            defs.families
                .get(&FontFamily::Monospace)
                .cloned()
                .unwrap_or_default(),
        );
        if has_symbols {
            mono.push("sys-symbols".to_owned());
        }
        defs.families.insert(FontFamily::Monospace, mono);
    }
    (defs, choice)
}

/// Font files that cover the scripts a document needs and egui's bundled
/// fonts do not. Loaded on a background thread only when a document asks.
pub fn cjk_font_candidates(scripts: Scripts) -> Vec<(&'static str, Vec<PathBuf>, u32)> {
    let mut wanted = Vec::new();
    #[cfg(windows)]
    {
        if scripts.han_or_kana {
            wanted.push((
                "sys-cjk",
                vec![
                    font_path("msyh.ttc"),
                    font_path("YuGothM.ttc"),
                    font_path("simsun.ttc"),
                ],
                0,
            ));
        }
        if scripts.hangul {
            wanted.push(("sys-hangul", vec![font_path("malgun.ttf")], 0));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if scripts.han_or_kana || scripts.hangul {
            wanted.push((
                "sys-cjk",
                vec![
                    PathBuf::from("/System/Library/Fonts/PingFang.ttc"),
                    PathBuf::from("/System/Library/Fonts/Hiragino Sans GB.ttc"),
                    PathBuf::from("/System/Library/Fonts/AppleSDGothicNeo.ttc"),
                ],
                0,
            ));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if scripts.han_or_kana || scripts.hangul {
            wanted.push((
                "sys-cjk",
                vec![
                    PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
                    PathBuf::from("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"),
                    PathBuf::from("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc"),
                    PathBuf::from("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc"),
                ],
                0,
            ));
        }
    }
    wanted
}

/// Reads the fallback fonts a document needs. Runs off the UI thread.
pub fn load_cjk_fonts(scripts: Scripts) -> Vec<(String, FontData)> {
    let mut loaded = Vec::new();
    for (key, paths, index) in cjk_font_candidates(scripts) {
        if let Some(bytes) = paths.iter().find_map(|p| std::fs::read(p).ok()) {
            let mut data = FontData::from_owned(bytes);
            data.index = index;
            loaded.push((key.to_owned(), data));
        }
    }
    loaded
}

/// Appends fallback fonts to every family.
pub fn with_fallbacks(
    mut defs: FontDefinitions,
    extra: &[(String, Arc<FontData>)],
) -> FontDefinitions {
    for (key, data) in extra {
        defs.font_data.insert(key.clone(), Arc::clone(data));
        for family in defs.families.values_mut() {
            if !family.contains(key) {
                family.push(key.clone());
            }
        }
    }
    defs
}
