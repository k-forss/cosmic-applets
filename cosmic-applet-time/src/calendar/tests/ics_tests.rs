use crate::calendar::event::{parse_ics_events, parse_ics_todos, CalendarEvent, CalendarTodo};
use jiff::{civil::Date, tz::TimeZone, Zoned};

fn make_event(uid: &str, summary: &str, start: Zoned, end: Option<Zoned>, all_day: bool) -> CalendarEvent {
    CalendarEvent {
        uid: uid.to_string(),
        source_id: "test".to_string(),
        summary: summary.to_string(),
        description: Some("A description".to_string()),
        location: Some("Room 101".to_string()),
        dtstart: start,
        dtend: end,
        all_day,
        url: None,
        color: "#FF0000".to_string(),
        etag: None,
        href: None,
        rrule: None,
        exdates: Vec::new(),
        rdates: Vec::new(),
    }
}

fn utc(y: i16, m: i8, d: i8, h: i8, min: i8) -> Zoned {
    Date::new(y, m, d)
        .unwrap()
        .at(h, min, 0, 0)
        .to_zoned(TimeZone::UTC)
        .unwrap()
}

// round-trip CalendarEvent
#[test]
fn event_round_trip() {
    let start = utc(2026, 3, 15, 10, 0);
    let end = utc(2026, 3, 15, 11, 30);
    let ev = make_event("round-trip-1", "Team Meeting", start.clone(), Some(end.clone()), false);
    let ics = ev.to_ics();
    let parsed = parse_ics_events(&ics, "test", "#FF0000");
    assert_eq!(parsed.len(), 1);
    let p = &parsed[0];
    assert_eq!(p.uid, "round-trip-1");
    assert_eq!(p.summary, "Team Meeting");
    assert_eq!(p.dtstart.date(), start.date());
    assert_eq!(p.dtstart.hour(), 10);
    assert_eq!(p.dtend.as_ref().unwrap().hour(), 11);
    assert_eq!(p.dtend.as_ref().unwrap().minute(), 30);
    assert!(!p.all_day);
    assert_eq!(p.description.as_deref(), Some("A description"));
    assert_eq!(p.location.as_deref(), Some("Room 101"));
}

// all-day event round-trip
#[test]
fn all_day_event_round_trip() {
    let start = Date::new(2026, 6, 1).unwrap().at(0, 0, 0, 0).to_zoned(TimeZone::UTC).unwrap();
    let end = Date::new(2026, 6, 2).unwrap().at(0, 0, 0, 0).to_zoned(TimeZone::UTC).unwrap();
    let ev = make_event("allday-1", "Holiday", start.clone(), Some(end), true);
    let ics = ev.to_ics();
    let parsed = parse_ics_events(&ics, "test", "#00FF00");
    assert_eq!(parsed.len(), 1);
    let p = &parsed[0];
    assert!(p.all_day);
    assert_eq!(p.date(), Date::new(2026, 6, 1).unwrap());
}

// description escaping
#[test]
fn description_escaping() {
    let start = utc(2026, 1, 1, 9, 0);
    let mut ev = make_event("escape-1", "Test", start, None, false);
    ev.description = Some("Line 1\nLine 2; commas, here".to_string());
    let ics = ev.to_ics();
    let parsed = parse_ics_events(&ics, "test", "#0000FF");
    assert_eq!(parsed.len(), 1);
    let desc = parsed[0].description.as_ref().unwrap();
    // After round-trip, newlines should be preserved (escaped as \n in ICS, then unescaped by parser)
    assert!(desc.contains("Line 1") && desc.contains("Line 2"));
}

// VTODO round-trip
#[test]
fn todo_round_trip() {
    let todo = CalendarTodo {
        uid: "todo-rt-1".to_string(),
        source_id: "test".to_string(),
        summary: "Buy groceries".to_string(),
        description: Some("Milk, eggs, bread".to_string()),
        due: Some(utc(2026, 4, 15, 17, 0)),
        completed: false,
        priority: Some(1),
        color: "#FFAA00".to_string(),
        etag: None,
        href: None,
    };
    let ics = todo.to_ics();
    let parsed = parse_ics_todos(&ics, "test", "#FFAA00");
    assert_eq!(parsed.len(), 1);
    let p = &parsed[0];
    assert_eq!(p.uid, "todo-rt-1");
    assert_eq!(p.summary, "Buy groceries");
    assert!(!p.completed);
    assert!(p.due.is_some());
    assert_eq!(p.due.as_ref().unwrap().date(), Date::new(2026, 4, 15).unwrap());
}

// parse real-world ICS samples from fixture files
#[test]
fn parse_vevent_with_rrule_and_exdate() {
    let ics = include_str!("fixtures/nextcloud_rrule_exdate.ics");
    let events = parse_ics_events(ics, "nc", "#3366CC");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO;COUNT=8"));
    assert_eq!(events[0].exdates.len(), 1);
    assert_eq!(events[0].exdates[0], Date::new(2026, 1, 19).unwrap());
}

#[test]
fn parse_vevent_with_rdate() {
    let ics = include_str!("fixtures/rdate_event.ics");
    let events = parse_ics_events(ics, "test", "#00CC00");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].rdates.len(), 1);
    assert_eq!(events[0].rdates[0], Date::new(2026, 4, 15).unwrap());
}

#[test]
fn parse_vtodo_with_due() {
    let ics = include_str!("fixtures/todo_with_due.ics");
    let todos = parse_ics_todos(ics, "test", "#FF0000");
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].summary, "File taxes");
    assert!(!todos[0].completed);
    assert!(todos[0].due.is_some());
}

#[test]
fn parse_vtodo_completed_no_due() {
    let ics = include_str!("fixtures/todo_completed_no_due.ics");
    let todos = parse_ics_todos(ics, "test", "#888888");
    assert_eq!(todos.len(), 1);
    assert!(todos[0].completed);
    assert!(todos[0].due.is_none());
}
