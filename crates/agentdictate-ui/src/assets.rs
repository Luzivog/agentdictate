use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

const CHEVRON_DOWN: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/chevron-down.svg"
));
const EYE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/eye.svg"
));
const INBOX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/inbox.svg"
));
const MINUS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/minus.svg"
));
const PLUS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/plus.svg"
));

const ICON_FILES: [&str; 5] = [
    "chevron-down.svg",
    "eye.svg",
    "inbox.svg",
    "minus.svg",
    "plus.svg",
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
        let bytes = match path {
            "icons/chevron-down.svg" => CHEVRON_DOWN,
            "icons/eye.svg" => EYE,
            "icons/inbox.svg" => INBOX,
            "icons/minus.svg" => MINUS,
            "icons/plus.svg" => PLUS,
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        if path.trim_end_matches('/') != "icons" {
            return Ok(Vec::new());
        }

        Ok(ICON_FILES.into_iter().map(SharedString::from).collect())
    }
}
