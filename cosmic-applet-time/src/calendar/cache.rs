// SPDX-License-Identifier: GPL-3.0-only

//! Simple JSON-based event cache for fast startup.
//!
//! Cached events are written after each successful sync and loaded on applet
//! startup so the user sees calendar data immediately before the first sync
//! completes.

use crate::calendar::event::{CalendarEvent, CalendarTodo};
use jiff::{civil::Date, tz::TimeZone, Timestamp, Zoned};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
struct EventCache {
    /// ISO-8601 timestamp of when the cache was written.
    timestamp: String,
    events: Vec<CachedEvent>,
    todos: Vec<CachedTodo>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedEvent {
    uid: String,
    source_id: String,
    summary: String,
    description: Option<String>,
    location: Option<String>,
    /// RFC 3339 timestamp
    dtstart: String,
    dtend: Option<String>,
    all_day: bool,
    url: Option<String>,
    color: String,
    etag: Option<String>,
    href: Option<String>,
    rrule: Option<String>,
    /// ISO-8601 date strings
    exdates: Vec<String>,
    #[serde(default)]
    rdates: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedTodo {
    uid: String,
    source_id: String,
    summary: String,
    description: Option<String>,
    due: Option<String>,
    completed: bool,
    priority: Option<u32>,
    color: String,
    etag: Option<String>,
    href: Option<String>,
}

fn cache_path() -> Option<PathBuf> {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".config"))
        })?;
    Some(
        config_dir
            .join("cosmic")
            .join("com.system76.CosmicAppletTime.Calendar")
            .join("v1")
            .join("event_cache.json"),
    )
}

fn zoned_to_rfc3339(z: &Zoned) -> String {
    z.timestamp().to_string()
}

fn rfc3339_to_zoned(s: &str) -> Option<Zoned> {
    s.parse::<Timestamp>()
        .ok()
        .map(|ts| ts.to_zoned(TimeZone::system()))
}

fn date_to_string(d: &Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month() as u8, d.day())
}

fn string_to_date(s: &str) -> Option<Date> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let y: i16 = parts[0].parse().ok()?;
    let m: i8 = parts[1].parse().ok()?;
    let d: i8 = parts[2].parse().ok()?;
    Date::new(y, m, d).ok()
}

fn event_to_cached(e: &CalendarEvent) -> CachedEvent {
    CachedEvent {
        uid: e.uid.clone(),
        source_id: e.source_id.clone(),
        summary: e.summary.clone(),
        description: e.description.clone(),
        location: e.location.clone(),
        dtstart: zoned_to_rfc3339(&e.dtstart),
        dtend: e.dtend.as_ref().map(zoned_to_rfc3339),
        all_day: e.all_day,
        url: e.url.clone(),
        color: e.color.clone(),
        etag: e.etag.clone(),
        href: e.href.clone(),
        rrule: e.rrule.clone(),
        exdates: e.exdates.iter().map(date_to_string).collect(),
        rdates: e.rdates.iter().map(date_to_string).collect(),
    }
}

fn cached_to_event(c: CachedEvent) -> Option<CalendarEvent> {
    let dtstart = rfc3339_to_zoned(&c.dtstart)?;
    Some(CalendarEvent {
        uid: c.uid,
        source_id: c.source_id,
        summary: c.summary,
        description: c.description,
        location: c.location,
        dtstart,
        dtend: c.dtend.as_deref().and_then(rfc3339_to_zoned),
        all_day: c.all_day,
        url: c.url,
        color: c.color,
        etag: c.etag,
        href: c.href,
        rrule: c.rrule,
        exdates: c.exdates.iter().filter_map(|s| string_to_date(s)).collect(),
        rdates: c.rdates.iter().filter_map(|s| string_to_date(s)).collect(),
    })
}

fn todo_to_cached(t: &CalendarTodo) -> CachedTodo {
    CachedTodo {
        uid: t.uid.clone(),
        source_id: t.source_id.clone(),
        summary: t.summary.clone(),
        description: t.description.clone(),
        due: t.due.as_ref().map(zoned_to_rfc3339),
        completed: t.completed,
        priority: t.priority,
        color: t.color.clone(),
        etag: t.etag.clone(),
        href: t.href.clone(),
    }
}

fn cached_to_todo(c: CachedTodo) -> CalendarTodo {
    CalendarTodo {
        uid: c.uid,
        source_id: c.source_id,
        summary: c.summary,
        description: c.description,
        due: c.due.as_deref().and_then(rfc3339_to_zoned),
        completed: c.completed,
        priority: c.priority,
        color: c.color,
        etag: c.etag,
        href: c.href,
    }
}

pub fn save_cache(events: &[CalendarEvent], todos: &[CalendarTodo]) {
    let Some(path) = cache_path() else {
        return;
    };
    let cache = EventCache {
        timestamp: Zoned::now().timestamp().to_string(),
        events: events.iter().map(event_to_cached).collect(),
        todos: todos.iter().map(todo_to_cached).collect(),
    };
    let json = match serde_json::to_string(&cache) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("Failed to serialize event cache: {e}");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("Failed to create cache directory: {e}");
            return;
        }
    }
    if let Err(e) = std::fs::write(&path, json) {
        tracing::warn!("Failed to write event cache: {e}");
    }
}

/// Returns `None` if the cache doesn't exist or is unreadable.
pub fn load_cache() -> Option<(Vec<CalendarEvent>, Vec<CalendarTodo>)> {
    let path = cache_path()?;
    let data = std::fs::read_to_string(&path).ok()?;
    let cache: EventCache = serde_json::from_str(&data).ok()?;
    let events: Vec<CalendarEvent> = cache.events.into_iter().filter_map(cached_to_event).collect();
    let todos: Vec<CalendarTodo> = cache.todos.into_iter().map(cached_to_todo).collect();
    Some((events, todos))
}
