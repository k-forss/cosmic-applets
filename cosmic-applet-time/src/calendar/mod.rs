// SPDX-License-Identifier: GPL-3.0-only

pub mod auth;
pub mod cache;
pub mod caldav;
pub mod config;
pub mod crypto;
pub mod event;
pub mod ics;
pub mod secrets;
#[cfg(test)]
mod tests;

pub use config::{CalendarConfig, CALENDAR_CONFIG_ID};
pub use event::{CalendarEvent, CalendarTodo};

use config::{CalDavCalendar, SourceConfig, SourceType};
use jiff::{civil::Date, ToSpan, Zoned};
use std::collections::BTreeMap;

/// Structured error type for calendar sync operations.
#[derive(Debug, Clone)]
pub enum SyncError {
    /// Authentication has expired for the given source ID – user must re-authenticate.
    AuthExpired(String),
    /// Any other error.
    Other(String),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthExpired(id) => write!(f, "Authentication expired for source {id}"),
            Self::Other(msg) => f.write_str(msg),
        }
    }
}

impl From<String> for SyncError {
    fn from(s: String) -> Self {
        Self::Other(s)
    }
}

/// The result of a sync_all operation.
pub struct SyncResult {
    /// Successfully fetched events (including expanded recurring occurrences).
    pub events: Vec<CalendarEvent>,
    /// Successfully fetched VTODO items.
    pub todos: Vec<CalendarTodo>,
    /// Source IDs whose authentication has expired and need re-auth.
    pub auth_expired: Vec<String>,
}

/// Per-calendar metadata updates produced by sync.
struct CalendarMetaUpdate {
    href: String,
    ctag: Option<String>,
    sync_token: Option<String>,
}

/// Compute the CalDAV time-range filter strings from today's date and the
/// configured past/future day counts.
///
/// Returns `(start_str, end_str)` in the format `YYYYMMDDTHHMMSSZ`.
pub fn compute_sync_range(today: Date, past_days: u32, future_days: u32) -> (String, String) {
    let range_start = today
        .checked_sub((past_days as i64).days())
        .unwrap_or(today);
    let range_end = today
        .checked_add((future_days as i64).days())
        .unwrap_or(today);
    (
        format!(
            "{}{:02}{:02}T000000Z",
            range_start.year(),
            range_start.month() as u8,
            range_start.day()
        ),
        format!(
            "{}{:02}{:02}T000000Z",
            range_end.year(),
            range_end.month() as u8,
            range_end.day()
        ),
    )
}

/// Sync events and todos from every enabled source in the configuration.
///
/// Returns the sync result and an updated copy of sources whose CalDAV
/// calendars had their `ctag` refreshed (caller should persist back to config).
pub async fn sync_all(config: &CalendarConfig, force_full: bool) -> (SyncResult, Vec<SourceConfig>) {
    let mut all_events = Vec::new();
    let mut all_todos = Vec::new();
    let mut auth_expired = Vec::new();
    let mut updated_sources = config.sources.clone();

    let today = Zoned::now().date();
    let range_start = today
        .checked_sub((config.sync_range_past_days as i64).days())
        .unwrap_or(today);
    let range_end = today
        .checked_add((config.sync_range_future_days as i64).days())
        .unwrap_or(today);
    let time_range_str = compute_sync_range(
        today,
        config.sync_range_past_days,
        config.sync_range_future_days,
    );

    for (idx, source) in config.sources.iter().enumerate() {
        if !source.enabled {
            continue;
        }

        match sync_source_with_ctag(source, Some((&time_range_str.0, &time_range_str.1)), force_full).await {
            Ok((events, todos, meta_updates)) => {
                all_events.extend(events);
                all_todos.extend(todos);
                // Apply ctag/sync-token updates to the cloned sources
                if !meta_updates.is_empty() {
                    if let SourceType::CalDav { calendars, .. } =
                        &mut updated_sources[idx].source_type
                    {
                        for update in meta_updates {
                            if let Some(cal) = calendars.iter_mut().find(|c| c.href == update.href) {
                                if let Some(new_ctag) = update.ctag {
                                    cal.ctag = Some(new_ctag);
                                }
                                if update.sync_token.is_some() {
                                    cal.sync_token = update.sync_token;
                                }
                            }
                        }
                    }
                }
            }
            Err(SyncError::AuthExpired(source_id)) => {
                tracing::warn!("Auth expired for source '{}' ({})", source.name, source_id);
                auth_expired.push(source_id);
            }
            Err(SyncError::Other(e)) => {
                tracing::error!("Calendar sync failed for '{}': {e}", source.name);
            }
        }
    }

    all_events = event::expand_recurring(all_events, range_start, range_end);

    all_events.sort_by(|a, b| a.dtstart.cmp(&b.dtstart));
    (
        SyncResult {
            events: all_events,
            todos: all_todos,
            auth_expired,
        },
        updated_sources,
    )
}

/// Returns (events, todos, ctag_updates) where ctag_updates maps href → new ctag.
async fn sync_source_with_ctag(
    source: &SourceConfig,
    time_range: Option<(&str, &str)>,
    force_full: bool,
) -> Result<(Vec<CalendarEvent>, Vec<CalendarTodo>, Vec<CalendarMetaUpdate>), SyncError> {
    match &source.source_type {
        SourceType::CalDav {
            url,
            auth,
            calendars,
        } => {
            let mut events = Vec::new();
            let mut todos = Vec::new();
            let mut meta_updates = Vec::new();
            let enabled: Vec<&CalDavCalendar> =
                calendars.iter().filter(|c| c.enabled).collect();
            let ca = source.ca_cert_path.as_deref();

            if enabled.is_empty() {
                return Ok((events, todos, meta_updates));
            }

            for cal in enabled {
                let color = if cal.color.is_empty() { &source.color } else { &cal.color };

                // Try incremental sync via sync-collection if we have a sync-token
                if !force_full {
                if let Some(ref token) = cal.sync_token {
                    match caldav::sync_collection(
                        &cal.href, url, auth, &source.id, color, token, ca,
                    )
                    .await
                    {
                        Ok(Some(result)) => {
                            events.extend(result.events);
                            todos.extend(result.todos);
                            meta_updates.push(CalendarMetaUpdate {
                                href: cal.href.clone(),
                                ctag: None,
                                sync_token: result.new_sync_token,
                            });
                            continue;
                        }
                        Ok(None) => {
                            // Token invalid, fall through to full sync
                            tracing::info!(
                                "sync-token expired for '{}', doing full sync",
                                cal.display_name
                            );
                        }
                        Err(e @ SyncError::AuthExpired(_)) => return Err(e),
                        Err(SyncError::Other(e)) => {
                            tracing::warn!(
                                "sync-collection failed for '{}': {e}, falling back",
                                cal.display_name
                            );
                        }
                    }
                }
                } // !force_full

                // Check server ctag — skip fetch if unchanged
                let server_ctag = if force_full {
                    None
                } else {
                    caldav::fetch_ctag(&cal.href, url, auth, &source.id, ca)
                        .await
                        .unwrap_or(None)
                };
                if let (Some(cached), Some(server)) = (&cal.ctag, &server_ctag) {
                    if cached == server {
                        tracing::debug!(
                            "ctag unchanged for '{}', skipping fetch",
                            cal.display_name
                        );
                        continue;
                    }
                }

                match caldav::fetch_events(&cal.href, url, auth, &source.id, color, time_range, ca)
                    .await
                {
                    Ok(cal_events) => {
                        events.extend(cal_events);
                        meta_updates.push(CalendarMetaUpdate {
                            href: cal.href.clone(),
                            ctag: server_ctag,
                            sync_token: None,
                        });
                    }
                    Err(e @ SyncError::AuthExpired(_)) => return Err(e),
                    Err(SyncError::Other(e)) => {
                        tracing::error!(
                            "CalDAV fetch failed for '{}' / '{}': {e}",
                            source.name,
                            cal.display_name
                        );
                    }
                }
                match caldav::fetch_todos(&cal.href, url, auth, &source.id, color, ca).await {
                    Ok(cal_todos) => todos.extend(cal_todos),
                    Err(SyncError::AuthExpired(_)) => { /* already handled above */ }
                    Err(SyncError::Other(e)) => {
                        tracing::error!(
                            "VTODO fetch failed for '{}' / '{}': {e}",
                            source.name,
                            cal.display_name
                        );
                    }
                }
            }
            Ok((events, todos, meta_updates))
        }
        SourceType::IcsUrl { url, auth } => {
            let events = ics::fetch_ics_url(url, &source.id, &source.color, auth, source.ca_cert_path.as_deref())
                .await
                .map_err(SyncError::Other)?;
            Ok((events, Vec::new(), Vec::new()))
        }
        SourceType::IcsFile { path } => {
            let events =
                ics::read_ics_file(path, &source.id, &source.color).map_err(SyncError::Other)?;
            Ok((events, Vec::new(), Vec::new()))
        }
    }
}

/// Run CalDAV calendar discovery on a source and merge newly found calendars.
///
/// Already-known calendars keep their `enabled` state.  Brand-new calendars
/// are added with `enabled: false` so the user can opt-in.
pub async fn discover_and_merge(source: &mut SourceConfig) -> Result<(), SyncError> {
    let source_id = source.id.clone();
    let SourceType::CalDav {
        url,
        auth,
        calendars,
    } = &mut source.source_type
    else {
        return Ok(());
    };

    let discovered = caldav::discover_calendars(url, auth, &source_id, source.ca_cert_path.as_deref()).await?;

    for new_cal in discovered {
        if !calendars.iter().any(|c| c.href == new_cal.href) {
            tracing::info!(
                "Discovered new calendar '{}' on '{}'",
                new_cal.display_name,
                source.name
            );
            calendars.push(new_cal);
        }
    }

    Ok(())
}

/// Group events by date for calendar grid display.
pub fn events_by_date(events: &[CalendarEvent]) -> BTreeMap<Date, Vec<CalendarEvent>> {
    let mut map: BTreeMap<Date, Vec<CalendarEvent>> = BTreeMap::new();
    for event in events {
        map.entry(event.date()).or_default().push(event.clone());
    }
    map
}

/// Return the first `count` events that start on or after `from`.
pub fn upcoming_events(events: &[CalendarEvent], from: Date, count: usize) -> Vec<CalendarEvent> {
    events
        .iter()
        .filter(|e| e.date() >= from)
        .take(count)
        .cloned()
        .collect()
}
