use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

macro_rules! icon {
    ($file:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/icons/",
            $file
        ))
    };
}

/// Every icon the app's gpui-component controls request: Select (chevron,
/// check, search, empty inbox) and Radio (check).
const ICONS: [(&str, &[u8]); 4] = [
    ("check.svg", icon!("check.svg")),
    ("chevron-down.svg", icon!("chevron-down.svg")),
    ("inbox.svg", icon!("inbox.svg")),
    ("search.svg", icon!("search.svg")),
];

/// Repository-owned desktop assets embedded in every AgentDictate executable.
///
/// GPUI Component resolves its control icons through the application's asset
/// source. Keeping this source beside the UI makes missing icon paths a
/// compile- and test-visible contract instead of a packaging concern.
#[derive(Clone, Copy, Debug, Default)]
pub struct AgentDictateAssets;

impl AssetSource for AgentDictateAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(path.strip_prefix("icons/").and_then(|file| {
            ICONS
                .iter()
                .find(|(name, _)| *name == file)
                .map(|(_, bytes)| Cow::Borrowed(*bytes))
        }))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        if path.trim_end_matches('/') != "icons" {
            return Ok(Vec::new());
        }
        Ok(ICONS
            .iter()
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}
