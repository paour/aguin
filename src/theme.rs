use std::sync::Arc;

use eframe::egui::{
    self, FontData, FontDefinitions, FontFamily, FontId, RichText, TextStyle,
    epaint::text::VariationCoords,
};
use egui_native_look as look;
use fontique::{
    Attributes, Collection, CollectionOptions, FontStyle, FontWeight, FontWidth, GenericFamily,
    QueryFamily, QueryStatus, SourceCache,
};

const REGULAR: &str = "system-regular";
const SEMIBOLD: &str = "system-semibold";
const MONOSPACE: &str = "system-monospace";

/// Takes the system's UI fonts, its accent colour and the look of its controls.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut collection = Collection::new(CollectionOptions::default());
    add_system_fonts(&mut fonts, &mut collection);
    ctx.set_fonts(fonts);
    look::install(ctx, look::Theme::system());
    // The look sets the body and button sizes; these follow the platform's other text sizes.
    ctx.all_styles_mut(|style| {
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::new(15.0, look::bold_family()));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(11.0));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(12.0));
    });
}

/// Keeps the accent and light or dark mode in step with the system while the app runs.
pub fn follow_system(ctx: &egui::Context) {
    look::follow_system(ctx);
}

/// Text in the semibold weight that macOS uses for emphasis, where egui would only brighten it.
pub fn semibold(text: impl Into<String>) -> RichText {
    RichText::new(text).family(look::bold_family())
}

/// Puts the system's UI and monospace fonts in front of egui's own, which stay as fallbacks
/// for the characters the system fonts lack. Returns whether the UI font was found.
fn add_system_fonts(fonts: &mut FontDefinitions, collection: &mut Collection) -> bool {
    let mut cache = SourceCache::default();
    let ui = [
        GenericFamily::SystemUi.into(),
        GenericFamily::UiSansSerif.into(),
        GenericFamily::SansSerif.into(),
    ];
    let system_mono = system_monospace_family();
    let mono: Vec<QueryFamily> = system_mono
        .as_deref()
        .map(QueryFamily::Named)
        .into_iter()
        .chain([
            GenericFamily::UiMonospace.into(),
            GenericFamily::Monospace.into(),
        ])
        .collect();
    let regular = find(collection, &mut cache, &ui, FontWeight::NORMAL);
    let semibold = find(collection, &mut cache, &ui, FontWeight::SEMI_BOLD);
    let monospace = find(collection, &mut cache, &mono, FontWeight::NORMAL);

    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    // The look's controls ask for this family, so it must exist even without a semibold font.
    let mut bold = fallbacks.clone();
    let found = regular.is_some();
    if let Some(regular) = regular {
        fonts.font_data.insert(REGULAR.into(), regular.into());
        put_first(fonts, FontFamily::Proportional, REGULAR);
        bold.insert(0, REGULAR.into());
    }
    if let Some(semibold) = semibold {
        fonts.font_data.insert(SEMIBOLD.into(), semibold.into());
        bold.insert(0, SEMIBOLD.into());
    }
    if let Some(monospace) = monospace {
        fonts.font_data.insert(MONOSPACE.into(), monospace.into());
        put_first(fonts, FontFamily::Monospace, MONOSPACE);
    }
    fonts.families.insert(look::bold_family(), bold);
    found
}

fn put_first(fonts: &mut FontDefinitions, family: FontFamily, name: &str) {
    fonts
        .families
        .entry(family)
        .or_default()
        .insert(0, name.into());
}

/// The fixed-pitch font of the macOS settings, Menlo unless the user chose another.
/// `fontique` has no macOS mapping for the UI monospace font, and its generic monospace
/// there is Courier. The monospaced system font of `AppKit`, SF Mono, is an alias without
/// a font file.
#[cfg(target_os = "macos")]
fn system_monospace_family() -> Option<String> {
    use objc2_core_text::{CTFont, CTFontUIFontType};
    // SAFETY: a null language asks for the current locale's font, as the API allows.
    let font =
        unsafe { CTFont::new_ui_font_for_language(CTFontUIFontType::UserFixedPitch, 0.0, None) }?;
    // SAFETY: the font is a valid CTFont that Core Text just created.
    Some(unsafe { font.family_name() }.to_string())
}

#[cfg(not(target_os = "macos"))]
fn system_monospace_family() -> Option<String> {
    None
}

/// The first font of the first of `families` the system has, at `weight`. A variable font,
/// such as San Francisco, gets the axis values for that weight.
fn find(
    collection: &mut Collection,
    cache: &mut SourceCache,
    families: &[QueryFamily],
    weight: FontWeight,
) -> Option<FontData> {
    let mut query = collection.query(cache);
    query.set_families(families.iter().copied());
    query.set_attributes(Attributes::new(
        FontWidth::NORMAL,
        FontStyle::Normal,
        weight,
    ));
    let mut found = None;
    query.matches_with(|font| {
        let mut data = FontData::from_blob(Arc::new(font.blob.clone()), font.index);
        data.tweak.coords =
            VariationCoords::new(font.synthesis.variation_settings().iter().copied());
        found = Some(data);
        QueryStatus::Stop
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_family_exists_even_without_system_fonts() {
        let mut fonts = FontDefinitions::default();
        let mut collection = Collection::new(CollectionOptions {
            system_fonts: false,
            ..CollectionOptions::default()
        });
        assert!(!add_system_fonts(&mut fonts, &mut collection));
        assert_eq!(
            fonts.families.get(&look::bold_family()),
            fonts.families.get(&FontFamily::Proportional)
        );
    }

    #[test]
    fn uses_the_system_ui_font_first() {
        let mut fonts = FontDefinitions::default();
        let mut collection = Collection::new(CollectionOptions::default());
        if add_system_fonts(&mut fonts, &mut collection) {
            assert_eq!(fonts.families[&FontFamily::Proportional][0], REGULAR);
            assert_eq!(fonts.families[&look::bold_family()][1], REGULAR);
        }
    }
}
