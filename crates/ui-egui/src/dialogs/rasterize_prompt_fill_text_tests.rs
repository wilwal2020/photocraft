//! #12538: a Paint Bucket click on a fill layer asks the reference application's question and OK
//! only rasterizes the fill; the catalogs that translated the old text translate the new one.

use crate::i18n::{Lang, tr};

const FILL: &str = "The fill for this layer must be rasterized before proceeding. It will no longer be editable as a fill. Rasterize the fill?";
const OLD: &str = "This fill layer must be rasterized before proceeding. Its fill content will no longer be editable. Rasterize the layer?";

#[test]
fn catalogs_that_had_a_translation_keep_one() {
    if !crate::i18n::needs_catalogs() {
        return;
    }
    let fr = Lang::from_code("fr").expect("French");
    assert_ne!(tr(fr, FILL), FILL, "French still translates the prompt");
    let de = Lang::from_code("de").expect("German");
    assert_eq!(tr(de, FILL), FILL, "a language without an entry falls back to English");
    for (code, _, catalog) in crate::i18n::CRAFT_I18N {
        // Check source columns, not comments or translations, in the embedded app catalogs.
        let mut keys = catalog.lines().filter(|l| !l.starts_with('#')).filter_map(|l| l.split('\t').nth(1));
        assert!(keys.all(|k| k != OLD), "{code} still has the old key");
    }
}
