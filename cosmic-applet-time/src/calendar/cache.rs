// SPDX-License-Identifier: GPL-3.0-only

//! Simple JSON-based event cache for fast startup.
//!
//! Cached events are written after each successful sync and loaded on applet
//! startup so the user sees calendar data immediately before the first sync
//! completes.

use crate::calendar::event::{CalendarEvent, CalendarTodo};
use jiff::{Timestamp, Zoned, civil::Date, tz::TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

pub(crate) fn save_cache_to_path(
    path: &Path,
    events: &[CalendarEvent],
    todos: &[CalendarTodo],
) -> Result<(), String> {
    let cache = EventCache {
        timestamp: Zoned::now().timestamp().to_string(),
        events: events.iter().map(event_to_cached).collect(),
        todos: todos.iter().map(todo_to_cached).collect(),
    };
    let json = serde_json::to_string(&cache).map_err(|e| format!("serialize: {e}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    std::fs::write(path, json).map_err(|e| format!("write: {e}"))?;
    Ok(())
}

pub(crate) fn load_cache_from_path(path: &Path) -> Option<(Vec<CalendarEvent>, Vec<CalendarTodo>)> {
    let data = std::fs::read_to_string(path).ok()?;
    let cache: EventCache = serde_json::from_str(&data).ok()?;
    let events: Vec<CalendarEvent> = cache
        .events
        .into_iter()
        .filter_map(cached_to_event)
        .collect();
    let todos: Vec<CalendarTodo> = cache.todos.into_iter().map(cached_to_todo).collect();
    Some((events, todos))
}

pub fn save_cache(events: &[CalendarEvent], todos: &[CalendarTodo]) {
    let Some(path) = cache_path() else {
        return;
    };
    if let Err(e) = save_cache_to_path(&path, events, todos) {
        tracing::warn!("Failed to save event cache: {e}");
    }
}

/// Returns `None` if the cache doesn't exist or is unreadable.
pub fn load_cache() -> Option<(Vec<CalendarEvent>, Vec<CalendarTodo>)> {
    let path = cache_path()?;
    load_cache_from_path(&path)
}

// ── Encrypted cache operations ────────────────────────────

use crate::calendar::crypto::EncryptionKey;

/// Save events+todos as encrypted bytes.
pub fn save_cache_encrypted(events: &[CalendarEvent], todos: &[CalendarTodo], key: &EncryptionKey) {
    let Some(path) = cache_path() else { return };
    let cache = EventCache {
        timestamp: jiff::Zoned::now().timestamp().to_string(),
        events: events.iter().map(event_to_cached).collect(),
        todos: todos.iter().map(todo_to_cached).collect(),
    };
    let json = match serde_json::to_string(&cache) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("Failed to serialize cache for encryption: {e}");
            return;
        }
    };
    let encrypted = crate::calendar::crypto::encrypt(json.as_bytes(), key);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, encrypted) {
        tracing::warn!("Failed to write encrypted cache: {e}");
    }
}

/// Load events+todos from an encrypted cache file.
///
/// Returns `None` on any failure (missing, wrong key, corrupt).
pub fn load_cache_encrypted(
    key: &EncryptionKey,
) -> Option<(Vec<CalendarEvent>, Vec<CalendarTodo>)> {
    let path = cache_path()?;
    let data = std::fs::read(&path).ok()?;
    let plaintext = match crate::calendar::crypto::decrypt(&data, key) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Cache decryption failed, will resync: {e}");
            // Invalid/wrong key — delete the stale file so next sync writes fresh
            let _ = std::fs::remove_file(&path);
            return None;
        }
    };
    let cache: EventCache = serde_json::from_slice(&plaintext).ok()?;
    let events = cache
        .events
        .into_iter()
        .filter_map(cached_to_event)
        .collect();
    let todos = cache.todos.into_iter().map(cached_to_todo).collect();
    Some((events, todos))
}

/// Dispatch to encrypted or plaintext save based on whether a key is available.
pub fn save_cache_dispatch(
    events: &[CalendarEvent],
    todos: &[CalendarTodo],
    key: Option<&EncryptionKey>,
) {
    match key {
        Some(k) => save_cache_encrypted(events, todos, k),
        None => save_cache(events, todos),
    }
}

/// Dispatch to encrypted or plaintext load based on whether a key is available.
///
/// If decryption fails the cache file is deleted so the next sync rebuilds it.
pub fn load_cache_dispatch(
    key: Option<&EncryptionKey>,
) -> Option<(Vec<CalendarEvent>, Vec<CalendarTodo>)> {
    match key {
        Some(k) => load_cache_encrypted(k),
        None => load_cache(),
    }
}

/// Delete both cache files (event + ctag).  Used when switching encryption modes
/// so the next sync rebuilds the cache in the new format.
pub fn delete_cache_files() {
    if let Some(path) = cache_path() {
        let _ = std::fs::remove_file(&path);
    }
    if let Some(path) = ctag_cache_path() {
        let _ = std::fs::remove_file(&path);
    }
}

/// Encrypted ctag cache operations — same pattern as event cache.
pub fn save_ctag_cache_encrypted(
    cache: &HashMap<(String, String), (Option<String>, Option<String>)>,
    key: &EncryptionKey,
) {
    let Some(path) = ctag_cache_path() else {
        return;
    };
    let entries: HashMap<String, (Option<String>, Option<String>)> = cache
        .iter()
        .map(|((sid, href), v)| (format!("{sid}::{href}"), v.clone()))
        .collect();
    let data = CtagCache { entries };
    let json = match serde_json::to_string(&data) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("Failed to serialize ctag cache for encryption: {e}");
            return;
        }
    };
    let encrypted = crate::calendar::crypto::encrypt(json.as_bytes(), key);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, encrypted) {
        tracing::warn!("Failed to write encrypted ctag cache: {e}");
    }
}

pub fn load_ctag_cache_encrypted(
    key: &EncryptionKey,
) -> HashMap<(String, String), (Option<String>, Option<String>)> {
    let Some(path) = ctag_cache_path() else {
        return HashMap::new();
    };
    let Ok(data) = std::fs::read(&path) else {
        return HashMap::new();
    };
    let plaintext = match crate::calendar::crypto::decrypt(&data, key) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Ctag cache decryption failed: {e}");
            let _ = std::fs::remove_file(&path);
            return HashMap::new();
        }
    };
    let Ok(cache) = serde_json::from_slice::<CtagCache>(&plaintext) else {
        return HashMap::new();
    };
    cache
        .entries
        .into_iter()
        .filter_map(|(key, val)| {
            let (sid, href) = key.split_once("::")?;
            Some(((sid.to_string(), href.to_string()), val))
        })
        .collect()
}

pub fn save_ctag_cache_dispatch(
    cache: &HashMap<(String, String), (Option<String>, Option<String>)>,
    key: Option<&EncryptionKey>,
) {
    match key {
        Some(k) => save_ctag_cache_encrypted(cache, k),
        None => save_ctag_cache(cache),
    }
}

pub fn load_ctag_cache_dispatch(
    key: Option<&EncryptionKey>,
) -> HashMap<(String, String), (Option<String>, Option<String>)> {
    match key {
        Some(k) => load_ctag_cache_encrypted(k),
        None => load_ctag_cache(),
    }
}

// ── Ctag / Sync-Token cache ───────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default)]
struct CtagCache {
    /// Key: "source_id::calendar_href", Value: (ctag, sync_token)
    entries: HashMap<String, (Option<String>, Option<String>)>,
}

fn ctag_cache_path() -> Option<PathBuf> {
    let mut path = cache_path()?;
    path.set_file_name("ctag_cache.json");
    Some(path)
}

/// Path for the Manual-mode passphrase salt (not secret, stored alongside cache).
pub fn salt_path() -> Option<PathBuf> {
    let mut path = cache_path()?;
    path.set_file_name("encryption_salt.bin");
    Some(path)
}

pub fn save_ctag_cache(cache: &HashMap<(String, String), (Option<String>, Option<String>)>) {
    let Some(path) = ctag_cache_path() else {
        return;
    };
    let entries: HashMap<String, (Option<String>, Option<String>)> = cache
        .iter()
        .map(|((sid, href), v)| (format!("{sid}::{href}"), v.clone()))
        .collect();
    let data = CtagCache { entries };
    match serde_json::to_string(&data) {
        Ok(json) => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!("Failed to write ctag cache: {e}");
            }
        }
        Err(e) => tracing::warn!("Failed to serialize ctag cache: {e}"),
    }
}

pub fn load_ctag_cache() -> HashMap<(String, String), (Option<String>, Option<String>)> {
    let Some(path) = ctag_cache_path() else {
        return HashMap::new();
    };
    let Ok(data) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let Ok(cache) = serde_json::from_str::<CtagCache>(&data) else {
        return HashMap::new();
    };
    cache
        .entries
        .into_iter()
        .filter_map(|(key, val)| {
            let (sid, href) = key.split_once("::")?;
            Some(((sid.to_string(), href.to_string()), val))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::crypto::EncryptionKey;

    fn test_event() -> CalendarEvent {
        let now = jiff::Zoned::now();
        CalendarEvent {
            uid: "test-uid-1".into(),
            source_id: "src-1".into(),
            summary: "Team Meeting".into(),
            description: Some("Weekly sync".into()),
            location: Some("Room 42".into()),
            dtstart: now.clone(),
            dtend: Some(now.checked_add(jiff::Span::new().hours(1)).unwrap()),
            all_day: false,
            url: None,
            color: "#ff0000".into(),
            etag: Some("\"etag-abc\"".into()),
            href: Some("/cal/event1.ics".into()),
            rrule: None,
            exdates: vec![],
            rdates: vec![],
        }
    }

    fn test_todo() -> CalendarTodo {
        CalendarTodo {
            uid: "todo-uid-1".into(),
            source_id: "src-1".into(),
            summary: "Fix bug".into(),
            description: None,
            due: Some(jiff::Zoned::now()),
            completed: false,
            priority: Some(1),
            color: "#00ff00".into(),
            etag: None,
            href: None,
        }
    }

    #[test]
    fn cache_encrypted_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("event_cache.json");

        let events = vec![test_event()];
        let todos = vec![test_todo()];
        let key = EncryptionKey::from_bytes([42u8; 32]);

        // Serialize, encrypt, write
        let cache = EventCache {
            timestamp: jiff::Zoned::now().timestamp().to_string(),
            events: events.iter().map(event_to_cached).collect(),
            todos: todos.iter().map(todo_to_cached).collect(),
        };
        let json = serde_json::to_string(&cache).unwrap();
        let encrypted = crate::calendar::crypto::encrypt(json.as_bytes(), &key);
        std::fs::write(&path, &encrypted).unwrap();

        // Read, decrypt, deserialize
        let data = std::fs::read(&path).unwrap();
        let plaintext = crate::calendar::crypto::decrypt(&data, &key).unwrap();
        let loaded: EventCache = serde_json::from_slice(&plaintext).unwrap();

        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.events[0].summary, "Team Meeting");
        assert_eq!(loaded.events[0].description.as_deref(), Some("Weekly sync"));
        assert_eq!(loaded.todos.len(), 1);
        assert_eq!(loaded.todos[0].summary, "Fix bug");
    }

    #[test]
    fn cache_wrong_key_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("event_cache.json");

        let key_a = EncryptionKey::from_bytes([1u8; 32]);
        let key_b = EncryptionKey::from_bytes([2u8; 32]);

        let cache = EventCache {
            timestamp: jiff::Zoned::now().timestamp().to_string(),
            events: vec![],
            todos: vec![],
        };
        let json = serde_json::to_string(&cache).unwrap();
        let encrypted = crate::calendar::crypto::encrypt(json.as_bytes(), &key_a);
        std::fs::write(&path, &encrypted).unwrap();

        // Decrypting with wrong key must fail
        let data = std::fs::read(&path).unwrap();
        assert!(crate::calendar::crypto::decrypt(&data, &key_b).is_err());
    }

    #[test]
    fn plaintext_cache_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("event_cache.json");

        let events = vec![test_event()];
        let todos = vec![test_todo()];

        save_cache_to_path(&path, &events, &todos).unwrap();
        let (loaded_events, loaded_todos) = load_cache_from_path(&path).unwrap();

        assert_eq!(loaded_events.len(), 1);
        assert_eq!(loaded_events[0].summary, "Team Meeting");
        assert_eq!(loaded_todos.len(), 1);
        assert_eq!(loaded_todos[0].summary, "Fix bug");
    }
}
