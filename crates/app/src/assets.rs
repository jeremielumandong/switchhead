//! The app's assets: gpui-kit's component icons plus switchhead's own (Lucide, ISC:
//! `assets/icons/LICENSE-LUCIDE`), served under `icons/sh-*.svg`.

use std::borrow::Cow;

use gpui_kit::{AssetSource, SharedString};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        [$((
            concat!("icons/sh-", $name, ".svg"),
            include_bytes!(concat!("../assets/icons/sh-", $name, ".svg")).as_slice(),
        )),*]
    };
}

const ICONS: [(&str, &[u8]); 16] = icons!(
    "archive",
    "archive-restore",
    "arrow-down",
    "arrow-down-to-line",
    "arrow-up",
    "cloud",
    "file-diff",
    "folder-git-2",
    "git-branch",
    "git-commit-horizontal",
    "git-fork",
    "git-merge",
    "lock",
    "refresh-cw",
    "sliders-horizontal",
    "tag",
);

/// gpui-kit's assets plus [`ICONS`].
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        match ICONS.iter().find(|(p, _)| *p == path) {
            Some((_, bytes)) => Ok(Some(Cow::Borrowed(bytes))),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut out = gpui_kit::assets::Assets.list(path)?;
        out.extend(
            ICONS
                .iter()
                .filter(|(p, _)| p.starts_with(path))
                .map(|(p, _)| SharedString::from(*p)),
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_own_and_component_icons() {
        let a = AppAssets;
        assert!(a.load("icons/sh-git-branch.svg").unwrap().is_some());
        assert!(a.load("icons/bot.svg").unwrap().is_some());
        assert!(
            a.list("icons/")
                .unwrap()
                .iter()
                .any(|p| p.as_ref() == "icons/sh-tag.svg")
        );
    }
}
