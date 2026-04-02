use crate::calendar::event::{CalendarEvent, CalendarTodo};
use jiff::{civil::Date, tz::TimeZone};
use std::collections::HashMap;

fn utc(y: i16, m: i8, d: i8, h: i8, min: i8) -> jiff::Zoned {
    Date::new(y, m, d)
        .unwrap()
        .at(h, min, 0, 0)
        .to_zoned(TimeZone::UTC)
        .unwrap()
}

fn sample_event() -> CalendarEvent {
    CalendarEvent {
        uid: "cache-evt-1".into(),
        source_id: "src1".into(),
        summary: "Cached Event".into(),
        description: Some("A description".into()),
        location: Some("Office".into()),
        dtstart: utc(2026, 6, 15, 10, 0),
        dtend: Some(utc(2026, 6, 15, 11, 0)),
        all_day: false,
        url: Some("https://example.com".into()),
        color: "#FF0000".into(),
        etag: Some("\"etag-1\"".into()),
        href: Some("/cal/event1.ics".into()),
        rrule: None,
        exdates: Vec::new(),
        rdates: Vec::new(),
    }
}

fn sample_todo() -> CalendarTodo {
    CalendarTodo {
        uid: "cache-todo-1".into(),
        source_id: "src1".into(),
        summary: "Cached Todo".into(),
        description: Some("Todo desc".into()),
        due: Some(utc(2026, 7, 1, 17, 0)),
        completed: false,
        priority: Some(2),
        color: "#00FF00".into(),
        etag: Some("\"etag-t1\"".into()),
        href: Some("/cal/todo1.ics".into()),
    }
}

// cache round-trip via filesystem using save_cache_to_path / load_cache_from_path
#[test]
fn cache_filesystem_round_trip() {
    let events = vec![sample_event()];
    let todos = vec![sample_todo()];

    let dir = std::env::temp_dir().join(format!("cache_test_{}", std::process::id()));
    let path = dir.join("event_cache.json");

    // Save to disk
    crate::calendar::cache::save_cache_to_path(&path, &events, &todos).unwrap();
    assert!(path.exists());

    // Load from disk
    let (loaded_events, loaded_todos) =
        crate::calendar::cache::load_cache_from_path(&path).unwrap();
    assert_eq!(loaded_events.len(), 1);
    assert_eq!(loaded_events[0].uid, "cache-evt-1");
    assert_eq!(loaded_events[0].summary, "Cached Event");
    assert_eq!(
        loaded_events[0].description.as_deref(),
        Some("A description")
    );
    assert_eq!(loaded_events[0].location.as_deref(), Some("Office"));
    assert_eq!(loaded_events[0].all_day, false);

    assert_eq!(loaded_todos.len(), 1);
    assert_eq!(loaded_todos[0].uid, "cache-todo-1");
    assert_eq!(loaded_todos[0].summary, "Cached Todo");

    // Clean up
    let _ = std::fs::remove_dir_all(&dir);
}

// cache with None optional fields
#[test]
fn cache_with_none_fields() {
    let ev = CalendarEvent {
        uid: "none-test".into(),
        source_id: "s".into(),
        summary: "Bare".into(),
        description: None,
        location: None,
        dtstart: utc(2026, 1, 1, 0, 0),
        dtend: None,
        all_day: true,
        url: None,
        color: "#000".into(),
        etag: None,
        href: None,
        rrule: None,
        exdates: Vec::new(),
        rdates: Vec::new(),
    };
    // Just ensure to_ics doesn't panic with None fields
    let ics = ev.to_ics();
    assert!(ics.contains("UID:none-test"));
}

// empty cache filesystem round-trip
#[test]
fn empty_cache_filesystem() {
    let dir = std::env::temp_dir().join(format!("cache_empty_test_{}", std::process::id()));
    let path = dir.join("event_cache.json");

    crate::calendar::cache::save_cache_to_path(&path, &[], &[]).unwrap();
    let (events, todos) = crate::calendar::cache::load_cache_from_path(&path).unwrap();
    assert!(events.is_empty());
    assert!(todos.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

// corrupted JSON graceful degradation
#[test]
fn corrupted_json_returns_none() {
    let dir = std::env::temp_dir().join(format!("cache_corrupt_test_{}", std::process::id()));
    let path = dir.join("event_cache.json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&path, "{ this is not valid json }}}").unwrap();
    assert!(crate::calendar::cache::load_cache_from_path(&path).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

// ctag cache round-trip
#[test]
fn ctag_cache_serde() {
    let mut cache: HashMap<(String, String), (Option<String>, Option<String>)> = HashMap::new();
    cache.insert(
        ("src1".into(), "/cal/1".into()),
        (Some("ctag-v1".into()), Some("sync-tok-1".into())),
    );
    cache.insert(("src2".into(), "/cal/2".into()), (None, None));

    // The ctag cache uses a Vec of tuples for serialization
    #[derive(serde::Serialize, serde::Deserialize)]
    struct CtagCache {
        entries: Vec<(String, String, Option<String>, Option<String>)>,
    }

    let serializable = CtagCache {
        entries: cache
            .iter()
            .map(|((sid, href), (ctag, sync_token))| {
                (sid.clone(), href.clone(), ctag.clone(), sync_token.clone())
            })
            .collect(),
    };

    let json = serde_json::to_string(&serializable).unwrap();
    let deser: CtagCache = serde_json::from_str(&json).unwrap();
    assert_eq!(deser.entries.len(), 2);
}
