//! Application-owned assets embedded in the desktop binary.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub struct Assets;

const ASSETS: &[(&str, &[u8])] = &[
    (
        "icons/arrow-right.svg",
        include_bytes!("../../../assets/icons/arrow-right.svg"),
    ),
    (
        "icons/bot.svg",
        include_bytes!("../../../assets/icons/bot.svg"),
    ),
    (
        "icons/check.svg",
        include_bytes!("../../../assets/icons/check.svg"),
    ),
    (
        "icons/chevron-down.svg",
        include_bytes!("../../../assets/icons/chevron-down.svg"),
    ),
    (
        "icons/chevron-left.svg",
        include_bytes!("../../../assets/icons/chevron-left.svg"),
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
        "icons/edit.svg",
        include_bytes!("../../../assets/icons/edit.svg"),
    ),
    (
        "icons/floppy-disc.svg",
        include_bytes!("../../../assets/icons/floppy-disc.svg"),
    ),
    (
        "icons/folder-open.svg",
        include_bytes!("../../../assets/icons/folder-open.svg"),
    ),
    (
        "icons/maximize.svg",
        include_bytes!("../../../assets/icons/maximize.svg"),
    ),
    (
        "icons/more-horizontal.svg",
        include_bytes!("../../../assets/icons/more-horizontal.svg"),
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
        "icons/tool.svg",
        include_bytes!("../../../assets/icons/tool.svg"),
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
        "icons/window-close.svg",
        include_bytes!("../../../assets/icons/window-close.svg"),
    ),
    (
        "icons/window-maximize.svg",
        include_bytes!("../../../assets/icons/window-maximize.svg"),
    ),
    (
        "icons/window-minimize.svg",
        include_bytes!("../../../assets/icons/window-minimize.svg"),
    ),
    (
        "icons/window-restore.svg",
        include_bytes!("../../../assets/icons/window-restore.svg"),
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
    use crate::ui::icons::IconName;

    #[test]
    fn application_icons_are_embedded_and_discoverable() {
        let assets = Assets;
        let listed = assets.list("icons/").unwrap();

        assert_eq!(listed.len(), IconName::ALL.len());
        for icon in IconName::ALL {
            let path = icon.path();
            assert!(
                listed
                    .iter()
                    .any(|listed_path| listed_path.as_ref() == path)
            );

            let svg = assets.load(path).unwrap().expect("icon should be embedded");
            let svg = std::str::from_utf8(svg.as_ref()).expect("icon should be UTF-8 SVG");
            assert!(svg.contains("viewBox=\"0 0 24 24\""), "{path}");
            assert!(svg.contains("currentColor"), "{path}");
        }
    }
}
