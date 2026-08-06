//! Application-owned assets embedded in the desktop binary.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub struct Assets;

const ASSETS: &[(&str, &[u8])] = &[
    (
        "icons/chevron-down.svg",
        include_bytes!("../../../assets/icons/chevron-down.svg"),
    ),
    (
        "icons/chevron-right.svg",
        include_bytes!("../../../assets/icons/chevron-right.svg"),
    ),
    (
        "icons/close-circle.svg",
        include_bytes!("../../../assets/icons/close-circle.svg"),
    ),
    (
        "icons/close.svg",
        include_bytes!("../../../assets/icons/close.svg"),
    ),
    (
        "icons/copy.svg",
        include_bytes!("../../../assets/icons/copy.svg"),
    ),
    (
        "icons/bot.svg",
        include_bytes!("../../../assets/icons/bot.svg"),
    ),
    (
        "icons/edit.svg",
        include_bytes!("../../../assets/icons/edit.svg"),
    ),
    (
        "icons/floppy-disc.svg",
        include_bytes!("../../../assets/icons/floppy-disc.svg"),
    ),
    (
        "icons/maximize.svg",
        include_bytes!("../../../assets/icons/maximize.svg"),
    ),
    (
        "icons/minus-circle.svg",
        include_bytes!("../../../assets/icons/minus-circle.svg"),
    ),
    (
        "icons/minus.svg",
        include_bytes!("../../../assets/icons/minus.svg"),
    ),
    (
        "icons/plus-circle.svg",
        include_bytes!("../../../assets/icons/plus-circle.svg"),
    ),
    (
        "icons/plus.svg",
        include_bytes!("../../../assets/icons/plus.svg"),
    ),
    (
        "icons/refresh.svg",
        include_bytes!("../../../assets/icons/refresh.svg"),
    ),
    (
        "icons/save-all.svg",
        include_bytes!("../../../assets/icons/save-all.svg"),
    ),
    (
        "icons/search.svg",
        include_bytes!("../../../assets/icons/search.svg"),
    ),
    (
        "icons/send.svg",
        include_bytes!("../../../assets/icons/send.svg"),
    ),
    (
        "icons/tab-close.svg",
        include_bytes!("../../../assets/icons/tab-close.svg"),
    ),
    (
        "icons/tab-pin-filled.svg",
        include_bytes!("../../../assets/icons/tab-pin-filled.svg"),
    ),
    (
        "icons/tab-pin-outline.svg",
        include_bytes!("../../../assets/icons/tab-pin-outline.svg"),
    ),
    (
        "icons/thinking.svg",
        include_bytes!("../../../assets/icons/thinking.svg"),
    ),
    (
        "icons/trash.svg",
        include_bytes!("../../../assets/icons/trash.svg"),
    ),
    (
        "icons/user.svg",
        include_bytes!("../../../assets/icons/user.svg"),
    ),
    (
        "icons/warning-triangle.svg",
        include_bytes!("../../../assets/icons/warning-triangle.svg"),
    ),
    (
        "icons/world.svg",
        include_bytes!("../../../assets/icons/world.svg"),
    ),
    (
        "icons/zoom-in.svg",
        include_bytes!("../../../assets/icons/zoom-in.svg"),
    ),
    (
        "icons/zoom-out.svg",
        include_bytes!("../../../assets/icons/zoom-out.svg"),
    ),
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
    fn application_icons_are_embedded_and_discoverable() {
        let assets = Assets;
        let listed = assets.list("icons/").unwrap();

        assert_eq!(listed.len(), 27);
        for path in listed {
            assert!(assets.load(path.as_ref()).unwrap().is_some());
        }
    }
}
