use crate::window::{parse_hex_color, parse_hhmm, parse_todo_due};

// ── Color parsing ──────────────────────────────────────────

#[test]
fn hex_color_full() {
    let c = parse_hex_color("#FF0000");
    // Red channel should be 1.0
    assert!((c.r - 1.0).abs() < 0.01);
    assert!(c.g.abs() < 0.01);
    assert!(c.b.abs() < 0.01);
}

#[test]
fn hex_color_blue() {
    let c = parse_hex_color("#0000FF");
    assert!(c.r.abs() < 0.01);
    assert!(c.g.abs() < 0.01);
    assert!((c.b - 1.0).abs() < 0.01);
}

#[test]
fn hex_color_short_returns_fallback() {
    // Short hex like "#fff" has only 3 chars after #, < 6
    let c = parse_hex_color("#fff");
    // Fallback is rgb8(128,128,128)
    let expected = 128.0 / 255.0;
    assert!((c.r - expected).abs() < 0.01);
    assert!((c.g - expected).abs() < 0.01);
    assert!((c.b - expected).abs() < 0.01);
}

#[test]
fn hex_color_empty_returns_fallback() {
    let c = parse_hex_color("");
    let expected = 128.0 / 255.0;
    assert!((c.r - expected).abs() < 0.01);
}

#[test]
fn hex_color_garbage_returns_fallback() {
    let c = parse_hex_color("not-a-color");
    let expected = 128.0 / 255.0;
    // With garbage, from_str_radix fails → unwrap_or(128)
    assert!((c.r - expected).abs() < 0.01);
}

// ── HH:MM parsing ─────────────────────────────────────────

#[test]
fn parse_hhmm_valid() {
    assert_eq!(parse_hhmm("09:30"), Some((9, 30)));
}

#[test]
fn parse_hhmm_midnight() {
    assert_eq!(parse_hhmm("00:00"), Some((0, 0)));
}

#[test]
fn parse_hhmm_end_of_day() {
    assert_eq!(parse_hhmm("23:59"), Some((23, 59)));
}

#[test]
fn parse_hhmm_24_invalid() {
    // 24:00 is out of range 0..24
    assert_eq!(parse_hhmm("24:00"), None);
}

#[test]
fn parse_hhmm_negative_invalid() {
    assert_eq!(parse_hhmm("-1:00"), None);
}

#[test]
fn parse_hhmm_garbage() {
    assert_eq!(parse_hhmm("abc"), None);
}

#[test]
fn parse_hhmm_empty() {
    assert_eq!(parse_hhmm(""), None);
}

// ── parse_todo_due ────────────────────────────────────────

#[test]
fn parse_todo_due_date_only() {
    let z = parse_todo_due("2026-03-15", "");
    assert!(z.is_some());
    let z = z.unwrap();
    assert_eq!(z.date(), jiff::civil::Date::new(2026, 3, 15).unwrap());
}

#[test]
fn parse_todo_due_with_time() {
    let z = parse_todo_due("2026-03-15", "14:30");
    assert!(z.is_some());
    let z = z.unwrap();
    assert_eq!(z.date(), jiff::civil::Date::new(2026, 3, 15).unwrap());
    assert_eq!(z.time().hour(), 14);
    assert_eq!(z.time().minute(), 30);
}

#[test]
fn parse_todo_due_empty_date() {
    assert!(parse_todo_due("", "").is_none());
}

#[test]
fn parse_todo_due_invalid_date() {
    assert!(parse_todo_due("not-a-date", "").is_none());
}

#[test]
fn parse_todo_due_invalid_time() {
    // Valid date but invalid time
    assert!(parse_todo_due("2026-03-15", "25:00").is_none());
}

// ── compute_sync_range ───────────────────────────────────────

use crate::calendar::compute_sync_range;
use crate::calendar::event::CalendarEvent;
use crate::calendar::{events_by_date, upcoming_events};
use jiff::{civil::Date, tz::TimeZone};

#[test]
fn sync_range_basic() {
    let today = Date::new(2026, 6, 15).unwrap();
    let (start, end) = compute_sync_range(today, 30, 90);
    assert_eq!(start, "20260516T000000Z");
    assert_eq!(end, "20260913T000000Z");
}

#[test]
fn sync_range_zero_days() {
    let today = Date::new(2026, 1, 1).unwrap();
    let (start, end) = compute_sync_range(today, 0, 0);
    assert_eq!(start, "20260101T000000Z");
    assert_eq!(end, "20260101T000000Z");
}

#[test]
fn sync_range_across_year_boundary() {
    let today = Date::new(2026, 1, 15).unwrap();
    let (start, _end) = compute_sync_range(today, 30, 30);
    // 30 days before 2026-01-15 = 2025-12-16
    assert_eq!(start, "20251216T000000Z");
}

// ── upcoming_events ────────────────────────────────────────

fn make_test_event(uid: &str, year: i16, month: i8, day: i8) -> CalendarEvent {
    let dtstart = Date::new(year, month, day)
        .unwrap()
        .at(10, 0, 0, 0)
        .to_zoned(TimeZone::UTC)
        .unwrap();
    CalendarEvent {
        uid: uid.into(),
        source_id: "test".into(),
        summary: format!("Event {uid}"),
        description: None,
        location: None,
        dtstart,
        dtend: None,
        all_day: false,
        url: None,
        color: "#0078D4".into(),
        etag: None,
        href: None,
        rrule: None,
        exdates: Vec::new(),
        rdates: Vec::new(),
    }
}

#[test]
fn upcoming_events_count_limit() {
    let events: Vec<CalendarEvent> = (1..=10)
        .map(|i| make_test_event(&format!("e{i}"), 2026, 6, i as i8))
        .collect();
    let from = Date::new(2026, 6, 1).unwrap();
    let result = upcoming_events(&events, from, 5);
    assert_eq!(result.len(), 5);
    assert_eq!(result[0].uid, "e1");
    assert_eq!(result[4].uid, "e5");
}

#[test]
fn upcoming_events_excludes_past() {
    let events = vec![
        make_test_event("past1", 2026, 5, 1),
        make_test_event("past2", 2026, 5, 15),
        make_test_event("future1", 2026, 6, 10),
        make_test_event("future2", 2026, 6, 20),
    ];
    let from = Date::new(2026, 6, 1).unwrap();
    let result = upcoming_events(&events, from, 10);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].uid, "future1");
    assert_eq!(result[1].uid, "future2");
}

#[test]
fn upcoming_events_empty() {
    let from = Date::new(2026, 6, 1).unwrap();
    let result = upcoming_events(&[], from, 5);
    assert!(result.is_empty());
}

// ── events_by_date ───────────────────────────────────────

#[test]
fn events_by_date_grouping() {
    let events = vec![
        make_test_event("a", 2026, 6, 1),
        make_test_event("b", 2026, 6, 5),
        make_test_event("c", 2026, 6, 10),
    ];
    let map = events_by_date(&events);
    assert_eq!(map.len(), 3);
    assert!(map.contains_key(&Date::new(2026, 6, 1).unwrap()));
    assert!(map.contains_key(&Date::new(2026, 6, 5).unwrap()));
    assert!(map.contains_key(&Date::new(2026, 6, 10).unwrap()));
    assert_eq!(map[&Date::new(2026, 6, 1).unwrap()].len(), 1);
}

#[test]
fn events_by_date_same_day() {
    let events = vec![
        make_test_event("x1", 2026, 7, 4),
        make_test_event("x2", 2026, 7, 4),
        make_test_event("x3", 2026, 7, 4),
    ];
    let map = events_by_date(&events);
    assert_eq!(map.len(), 1);
    let day = &map[&Date::new(2026, 7, 4).unwrap()];
    assert_eq!(day.len(), 3);
}
