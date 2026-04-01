use crate::calendar::event::{expand_recurring, parse_ics_events, CalendarEvent};
use jiff::civil::Date;

/// Helper: build a minimal CalendarEvent with an RRULE, starting on the given date.
fn event_with_rrule(start: &str, rrule: &str) -> CalendarEvent {
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//TEST//EN\r\n\
         BEGIN:VEVENT\r\nUID:test-rrule\r\n\
         DTSTART:{start}\r\n\
         SUMMARY:Recurring\r\n\
         RRULE:{rrule}\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let mut events = parse_ics_events(&ics, "test", "#0000FF");
    assert_eq!(events.len(), 1, "expected exactly 1 event from ICS");
    events.remove(0)
}

fn event_with_rrule_and_exdate(start: &str, rrule: &str, exdate: &str) -> CalendarEvent {
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//TEST//EN\r\n\
         BEGIN:VEVENT\r\nUID:test-exdate\r\n\
         DTSTART:{start}\r\n\
         SUMMARY:Recurring\r\n\
         RRULE:{rrule}\r\n\
         EXDATE:{exdate}\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let mut events = parse_ics_events(&ics, "test", "#0000FF");
    assert_eq!(events.len(), 1);
    events.remove(0)
}

fn event_with_rdate(start: &str, rdate: &str) -> CalendarEvent {
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//TEST//EN\r\n\
         BEGIN:VEVENT\r\nUID:test-rdate\r\n\
         DTSTART:{start}\r\n\
         SUMMARY:Extra dates\r\n\
         RDATE:{rdate}\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let mut events = parse_ics_events(&ics, "test", "#0000FF");
    assert_eq!(events.len(), 1);
    events.remove(0)
}

/// Wide range for expansion.
fn wide_range() -> (Date, Date) {
    (
        Date::new(2026, 1, 1).unwrap(),
        Date::new(2030, 12, 31).unwrap(),
    )
}

// daily rule COUNT=5
#[test]
fn daily_rule_count_5() {
    let ev = event_with_rrule("20260110T090000Z", "FREQ=DAILY;COUNT=5");
    let (start, end) = wide_range();
    let expanded = expand_recurring(vec![ev], start, end);
    assert_eq!(expanded.len(), 5);
    let dates: Vec<Date> = expanded.iter().map(|e| e.date()).collect();
    assert_eq!(dates[0], Date::new(2026, 1, 10).unwrap());
    assert_eq!(dates[4], Date::new(2026, 1, 14).unwrap());
    // Consecutive days
    for w in dates.windows(2) {
        let diff = w[1].since(w[0]).unwrap().get_days();
        assert_eq!(diff, 1);
    }
}

// weekly BYDAY=MO,WE,FR COUNT=6
#[test]
fn weekly_byday_count_6() {
    // Start on Monday 2026-01-12
    let ev = event_with_rrule("20260112T090000Z", "FREQ=WEEKLY;BYDAY=MO,WE,FR;COUNT=6");
    let (start, end) = wide_range();
    let expanded = expand_recurring(vec![ev], start, end);
    assert_eq!(expanded.len(), 6, "expected 6 occurrences, got {}", expanded.len());
    for e in &expanded {
        let wd = e.date().weekday();
        assert!(
            wd == jiff::civil::Weekday::Monday
                || wd == jiff::civil::Weekday::Wednesday
                || wd == jiff::civil::Weekday::Friday,
            "unexpected weekday: {:?} on {}",
            wd,
            e.date()
        );
    }
}

// monthly BYMONTHDAY=15 COUNT=3
#[test]
fn monthly_bymonthday_15_count_3() {
    let ev = event_with_rrule("20260115T090000Z", "FREQ=MONTHLY;BYMONTHDAY=15;COUNT=3");
    let (start, end) = wide_range();
    let expanded = expand_recurring(vec![ev], start, end);
    assert_eq!(expanded.len(), 3);
    assert_eq!(expanded[0].date(), Date::new(2026, 1, 15).unwrap());
    assert_eq!(expanded[1].date(), Date::new(2026, 2, 15).unwrap());
    assert_eq!(expanded[2].date(), Date::new(2026, 3, 15).unwrap());
}

// yearly COUNT=3
#[test]
fn yearly_count_3() {
    let ev = event_with_rrule("20260601T120000Z", "FREQ=YEARLY;COUNT=3");
    let (start, end) = wide_range();
    let expanded = expand_recurring(vec![ev], start, end);
    assert_eq!(expanded.len(), 3);
    assert_eq!(expanded[0].date().year(), 2026);
    assert_eq!(expanded[1].date().year(), 2027);
    assert_eq!(expanded[2].date().year(), 2028);
}

// INTERVAL=3 daily
#[test]
fn daily_interval_3_count_4() {
    let ev = event_with_rrule("20260110T090000Z", "FREQ=DAILY;INTERVAL=3;COUNT=4");
    let (start, end) = wide_range();
    let expanded = expand_recurring(vec![ev], start, end);
    assert_eq!(expanded.len(), 4);
    let dates: Vec<Date> = expanded.iter().map(|e| e.date()).collect();
    for w in dates.windows(2) {
        assert_eq!(w[1].since(w[0]).unwrap().get_days(), 3);
    }
}

// UNTIL
#[test]
fn daily_until() {
    let ev = event_with_rrule("20260110T090000Z", "FREQ=DAILY;UNTIL=20260115T000000Z");
    let (start, end) = wide_range();
    let expanded = expand_recurring(vec![ev], start, end);
    assert!(!expanded.is_empty());
    for e in &expanded {
        assert!(e.date() <= Date::new(2026, 1, 15).unwrap());
    }
    // Last occurrence should be on or before Jan 15
    let last = expanded.last().unwrap().date();
    assert!(last <= Date::new(2026, 1, 15).unwrap());
}

// EXDATE
#[test]
fn daily_exdate_skipped() {
    let ev = event_with_rrule_and_exdate(
        "20260110T090000Z",
        "FREQ=DAILY;COUNT=5",
        "20260112T090000Z",
    );
    let (start, end) = wide_range();
    let expanded = expand_recurring(vec![ev], start, end);
    // 5 occurrences minus the excluded Jan 12 = 4
    assert_eq!(expanded.len(), 4);
    let dates: Vec<Date> = expanded.iter().map(|e| e.date()).collect();
    assert!(!dates.contains(&Date::new(2026, 1, 12).unwrap()));
}

// RDATE
#[test]
fn rdate_extra_occurrence() {
    let ev = event_with_rdate("20260110T090000Z", "20260220T090000Z");
    let (start, end) = wide_range();
    let expanded = expand_recurring(vec![ev], start, end);
    // Original + 1 RDATE = 2
    assert_eq!(expanded.len(), 2);
    let dates: Vec<Date> = expanded.iter().map(|e| e.date()).collect();
    assert!(dates.contains(&Date::new(2026, 1, 10).unwrap()));
    assert!(dates.contains(&Date::new(2026, 2, 20).unwrap()));
}

// iteration cap at 1000
#[test]
fn iteration_cap_1000() {
    let ev = event_with_rrule("20260110T090000Z", "FREQ=DAILY");
    let start = Date::new(2026, 1, 1).unwrap();
    let end = Date::new(2050, 12, 31).unwrap();
    let expanded = expand_recurring(vec![ev], start, end);
    assert!(expanded.len() <= 1000, "should cap at 1000, got {}", expanded.len());
}

// leap year handling: monthly starting Jan 31 through Feb
#[test]
fn monthly_through_february() {
    let ev = event_with_rrule("20260131T120000Z", "FREQ=MONTHLY;COUNT=3");
    let (start, end) = wide_range();
    let expanded = expand_recurring(vec![ev], start, end);
    assert_eq!(expanded.len(), 3);
    assert_eq!(expanded[0].date(), Date::new(2026, 1, 31).unwrap());
    // Feb has 28 days in 2026 — jiff may clamp to Feb 28
    let feb = expanded[1].date();
    assert_eq!(feb.month(), 2);
    assert!(feb.day() == 28 || feb.day() == 29); // clamped
}

// BYDAY with positive position: 2TU = second Tuesday
#[test]
fn monthly_byday_2tu() {
    let ev = event_with_rrule("20260113T090000Z", "FREQ=MONTHLY;BYDAY=2TU;COUNT=3");
    let (start, end) = wide_range();
    let expanded = expand_recurring(vec![ev], start, end);
    assert_eq!(expanded.len(), 3);
    for e in &expanded {
        assert_eq!(e.date().weekday(), jiff::civil::Weekday::Tuesday);
        // Should be the 8th-14th day of the month (second week)
        assert!(e.date().day() >= 8 && e.date().day() <= 14);
    }
}

// BYDAY with negative position: -1FR = last Friday
#[test]
fn monthly_byday_last_friday() {
    let ev = event_with_rrule("20260130T090000Z", "FREQ=MONTHLY;BYDAY=-1FR;COUNT=3");
    let (start, end) = wide_range();
    let expanded = expand_recurring(vec![ev], start, end);
    assert_eq!(expanded.len(), 3);
    for e in &expanded {
        assert_eq!(e.date().weekday(), jiff::civil::Weekday::Friday);
        // Last Friday — day should be >= 22
        assert!(e.date().day() >= 22, "last Friday of month should be day >= 22, got {}", e.date().day());
    }
}
