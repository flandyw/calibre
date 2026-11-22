//! Asset source for the app: gpui-kit's default component icons plus the
//! extra Lucide icons the UI uses.
//!
//! The default bundle only covers the icons gpui-kit's own components need.
//! Any `gpui_kit::assets::IconName` used in `ui/` that is not in it must be
//! listed in `ExtraIcons` below, or it renders as an empty square. The tests
//! check this.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        AArrowDown,
        AArrowUp,
        BookCopy,
        Clock,
        Download,
        LayoutGrid,
        LibraryBig,
        List,
        SquarePen,
        SunMoon,
        TabletSmartphone,
        Tag,
        Usb,
    ]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        // `ExtraIcons` answers Ok(None) for paths it lacks, but the default
        // bundle answers Err, so the extras must be asked first.
        if let Some(data) = ExtraIcons.load(path)? {
            return Ok(Some(data));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::assets::{IconName, IconNamed as _};
    use gpui_kit::AssetSource as _;

    use super::AppAssets;

    fn loads(path: &str) -> bool {
        matches!(AppAssets.load(path), Ok(Some(data)) if !data.is_empty())
    }

    #[core::prelude::v1::test]
    fn extra_and_default_icons_both_load() {
        for icon in [
            IconName::AArrowDown,
            IconName::AArrowUp,
            IconName::BookCopy,
            IconName::Clock,
            IconName::Download,
            IconName::LayoutGrid,
            IconName::LibraryBig,
            IconName::List,
            IconName::SquarePen,
            IconName::SunMoon,
            IconName::TabletSmartphone,
            IconName::Tag,
            IconName::Usb,
            // From the default bundle.
            IconName::Search,
            IconName::User,
            IconName::FileText,
            IconName::Sun,
            IconName::Moon,
        ] {
            let path = icon.path();
            assert!(loads(&path), "icon {path} does not load");
        }
    }

    /// Every `Lucide::Name` in the UI sources must be in the bundle.
    #[core::prelude::v1::test]
    fn every_icon_used_by_the_ui_is_bundled() {
        let ui = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui");
        let this_file = include_str!("assets.rs");
        let default_bundle = ["FileText", "Moon", "Sun", "User"];
        for entry in std::fs::read_dir(ui).expect("src/ui exists") {
            let path = entry.expect("readable entry").path();
            let source = std::fs::read_to_string(&path).expect("readable source");
            for (index, _) in source.match_indices("Lucide::") {
                let name: String = source[index + "Lucide::".len()..]
                    .chars()
                    .take_while(|ch| ch.is_ascii_alphanumeric())
                    .collect();
                let listed = this_file.contains(&format!("        {name},\n"));
                assert!(
                    listed || default_bundle.contains(&name.as_str()),
                    "{} uses Lucide::{name}, which is not in ExtraIcons",
                    path.display()
                );
            }
        }
    }
}
