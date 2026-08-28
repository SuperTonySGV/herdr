use serde::{Deserialize, Serialize};

/// Where a remembered directory came from.
///
/// Neutral names on purpose: this is a runtime fact, not a description of any
/// UI surface that happens to render it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlaceSourceInfo {
    Explicit,
    PromotedFromPin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlaceInfo {
    pub label: String,
    pub path: String,
    pub source: PlaceSourceInfo,
    /// The path does not currently resolve. Reported rather than filtered:
    /// dropping an entry because a drive is unplugged is exactly the silent
    /// loss this feature exists to prevent.
    pub missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RecentPlaceInfo {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Seconds since the Unix epoch, so the wire format does not depend on the
    /// host's clock representation.
    pub last_used_unix: u64,
    pub missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlaceAddParams {
    /// Must be absolute. The server is long-lived and its working directory is
    /// unrelated to the caller's, so there is no sane directory to resolve a
    /// relative path against; clients expand before calling.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlaceRemoveParams {
    pub path: String,
}
