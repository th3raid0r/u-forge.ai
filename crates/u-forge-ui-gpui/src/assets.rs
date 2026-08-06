//! Application-owned assets embedded in the desktop binary.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub struct Assets;

const TAB_CLOSE: &[u8] = include_bytes!("../../../assets/icons/tab-close.svg");
const TAB_PIN_OUTLINE: &[u8] = include_bytes!("../../../assets/icons/tab-pin-outline.svg");
const TAB_PIN_FILLED: &[u8] = include_bytes!("../../../assets/icons/tab-pin-filled.svg");

const ASSETS: &[(&str, &[u8])] = &[
    ("icons/tab-close.svg", TAB_CLOSE),
    ("icons/tab-pin-outline.svg", TAB_PIN_OUTLINE),
    ("icons/tab-pin-filled.svg", TAB_PIN_FILLED),
];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ASSETS
            .iter()
            .find_map(|(asset_path, bytes)| (*asset_path == path).then_some(Cow::Borrowed(*bytes))))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ASSETS
            .iter()
            .filter_map(|(asset_path, _)| {
                asset_path.starts_with(path).then_some((*asset_path).into())
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource;

    use super::Assets;

    #[test]
    fn tab_icons_are_embedded_and_discoverable() {
        let assets = Assets;
        let listed = assets.list("icons/tab-").unwrap();

        assert_eq!(listed.len(), 3);
        for path in listed {
            assert!(assets.load(path.as_ref()).unwrap().is_some());
        }
    }
}
