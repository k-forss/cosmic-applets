// SPDX-License-Identifier: GPL-3.0-only

use cosmic_config::{CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};
use serde::{Deserialize, Serialize};

pub const CALENDAR_CONFIG_ID: &str = "com.system76.CosmicAppletTime.Calendar";

#[derive(Debug, Clone, PartialEq, CosmicConfigEntry, Deserialize, Serialize)]
#[version = 1]
pub struct CalendarConfig {
    pub sources: Vec<SourceConfig>,
    pub sync_interval_minutes: u32,
    pub upcoming_count: usize,
    pub calendar_app: String,
    pub sync_range_past_days: u32,
    pub sync_range_future_days: u32,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            sync_interval_minutes: 15,
            upcoming_count: 5,
            calendar_app: String::new(),
            sync_range_past_days: 30,
            sync_range_future_days: 90,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceConfig {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub color: String,
    pub source_type: SourceType,
    /// Optional path to a PEM-encoded CA certificate for self-hosted servers.
    #[serde(default)]
    pub ca_cert_path: Option<String>,
}

impl SourceConfig {
    pub fn new(name: String, color: String, source_type: SourceType) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            enabled: true,
            color,
            source_type,
            ca_cert_path: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SourceType {
    CalDav {
        url: String,
        auth: AuthMethod,
        /// Calendars discovered via PROPFIND.  New calendars are added
        /// with `enabled: false` so the user can opt-in.
        #[serde(default)]
        calendars: Vec<CalDavCalendar>,
    },
    IcsUrl {
        url: String,
        #[serde(default)]
        auth: AuthMethod,
    },
    IcsFile { path: String },
}

/// A single calendar collection discovered on a CalDAV server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalDavCalendar {
    /// The href / collection URL for this calendar.
    pub href: String,
    /// Human-readable name from the server (`displayname` property).
    pub display_name: String,
    /// Server-provided color (e.g. from `calendar-color` property).
    pub color: Option<String>,
    /// Whether the user has opted to sync this calendar.
    pub enabled: bool,
    /// Cached ctag from the server.  If unchanged across syncs the calendar
    /// does not need to be re-fetched.
    #[serde(default)]
    pub ctag: Option<String>,
    /// WebDAV sync-token for incremental sync (RFC 6578).
    #[serde(default)]
    pub sync_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AuthMethod {
    None,
    /// Basic auth – username stored here, password in Secret Service keyring.
    Basic { username: String },
    /// Bearer token – token stored in Secret Service keyring.
    Bearer,
    /// OIDC – issuer/client metadata here, tokens in Secret Service keyring.
    Oidc {
        issuer_url: String,
        client_id: String,
        /// Whether an access token has been saved in the keyring.
        #[serde(default)]
        has_token: bool,
        /// Whether a client secret has been saved in the keyring
        /// (confidential/private client).  When `false` the client is
        /// treated as a public OIDC client.
        #[serde(default)]
        has_client_secret: bool,
        /// OAuth2 scopes to request.  Defaults to `["openid", "offline_access"]`.
        #[serde(default = "default_oidc_scopes")]
        scopes: Vec<String>,
    },
}

impl Default for AuthMethod {
    fn default() -> Self {
        Self::None
    }
}

fn default_oidc_scopes() -> Vec<String> {
    vec!["openid".to_string(), "offline_access".to_string()]
}
