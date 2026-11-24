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
        Bookmark,
        BrushCleaning,
        Clock,
        Cloud,
        Compass,
        Download,
        Eraser,
        Highlighter,
        Languages,
        Laptop,
        LayoutGrid,
        LibraryBig,
        List,
        ListChecks,
        Lock,
        MessagesSquare,
        ScanText,
        Server,
        Settings,
        ShieldCheck,
        Sparkles,
        SquarePen,
        SunMoon,
        TabletSmartphone,
        Tag,
        TextQuote,
        TextSearch,
        Timer,
        Usb,
        Users,
        WandSparkles,
        Wrench,
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
    use gpui_kit::assets::IconName;
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
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let this_file = include_str!("assets.rs");
        let default_bundle = [
            "ArrowRight",
            "ArrowUp",
            "BookOpen",
            "Check",
            "CircleCheck",
            "Copy",
            "FileText",
            "Info",
            "Moon",
            "Network",
            "Search",
            "Square",
            "Sun",
            "TriangleAlert",
            "User",
        ];
        let mut sources: Vec<_> = std::fs::read_dir(src.join("ui"))
            .expect("src/ui exists")
            .map(|entry| entry.expect("readable entry").path())
            .collect();
        // Views that live beside their state rather than under ui/.
        sources.push(src.join("studio.rs"));
        for path in sources {
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
