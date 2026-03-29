// SPDX-License-Identifier: GPL-3.0-only

use jiff::{civil::Date, fmt::strtime, tz::TimeZone, ToSpan, Zoned};

#[derive(Debug, Clone)]
pub struct CalendarEvent {
    pub uid: String,
    pub source_id: String,
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub dtstart: Zoned,
    pub dtend: Option<Zoned>,
    pub all_day: bool,
    pub url: Option<String>,
    pub color: String,
    /// ETag from the CalDAV server (needed for update/delete).
    pub etag: Option<String>,
    /// Resource href on the CalDAV server (needed for update/delete).
    pub href: Option<String>,
    /// Raw RRULE string, if this event recurs.
    pub rrule: Option<String>,
    /// Exception dates excluded from recurrence.
    pub exdates: Vec<Date>,
    /// Additional recurrence dates from RDATE.
    pub rdates: Vec<Date>,
}

impl CalendarEvent {
    pub fn date(&self) -> Date {
        self.dtstart.date()
    }

    pub fn time_display(&self) -> String {
        if self.all_day {
            String::from("All day")
        } else {
            let start = format!("{:02}:{:02}", self.dtstart.hour(), self.dtstart.minute());
            if let Some(end) = &self.dtend {
                format!("{start} – {:02}:{:02}", end.hour(), end.minute())
            } else {
                start
            }
        }
    }

    /// Serialize this event as a minimal valid iCalendar (RFC 5545) document.
    pub fn to_ics(&self) -> String {
        let dtstart = if self.all_day {
            format!(
                "DTSTART;VALUE=DATE:{}{:02}{:02}",
                self.dtstart.date().year(),
                self.dtstart.date().month(),
                self.dtstart.date().day()
            )
        } else {
            let ts = self.dtstart.timestamp();
            let utc = ts.to_zoned(TimeZone::UTC);
            format!(
                "DTSTART:{}{:02}{:02}T{:02}{:02}{:02}Z",
                utc.date().year(),
                utc.date().month(),
                utc.date().day(),
                utc.hour(),
                utc.minute(),
                utc.second()
            )
        };

        let dtend = if let Some(end) = &self.dtend {
            if self.all_day {
                format!(
                    "\r\nDTEND;VALUE=DATE:{}{:02}{:02}",
                    end.date().year(),
                    end.date().month(),
                    end.date().day()
                )
            } else {
                let ts = end.timestamp();
                let utc = ts.to_zoned(TimeZone::UTC);
                format!(
                    "\r\nDTEND:{}{:02}{:02}T{:02}{:02}{:02}Z",
                    utc.date().year(),
                    utc.date().month(),
                    utc.date().day(),
                    utc.hour(),
                    utc.minute(),
                    utc.second()
                )
            }
        } else {
            String::new()
        };

        let mut ics = format!(
            "BEGIN:VCALENDAR\r\n\
             VERSION:2.0\r\n\
             PRODID:-//COSMIC//cosmic-applet-time//EN\r\n\
             BEGIN:VEVENT\r\n\
             UID:{uid}\r\n\
             {dtstart}{dtend}\r\n\
             SUMMARY:{summary}\r\n",
            uid = self.uid,
            dtstart = dtstart,
            dtend = dtend,
            summary = ics_escape(&self.summary),
        );

        if let Some(desc) = &self.description {
            if !desc.is_empty() {
                ics.push_str(&format!("DESCRIPTION:{}\r\n", ics_escape(desc)));
            }
        }
        if let Some(loc) = &self.location {
            if !loc.is_empty() {
                ics.push_str(&format!("LOCATION:{}\r\n", ics_escape(loc)));
            }
        }

        ics.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
        ics
    }
}

/// Escape special iCalendar text characters (RFC 5545 §3.3.11).
fn ics_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

/// Parse iCalendar data into a list of calendar events.
pub fn parse_ics_events(ics_data: &str, source_id: &str, color: &str) -> Vec<CalendarEvent> {
    use std::io::BufReader;
    let reader = BufReader::new(ics_data.as_bytes());
    let parser = ical::IcalParser::new(reader);
    let mut events = Vec::new();

    for calendar in parser.flatten() {
        for event in calendar.events {
            if let Some(cal_event) = parse_event(&event.properties, source_id, color) {
                events.push(cal_event);
            }
        }
    }

    events
}

fn parse_event(
    props: &[ical::property::Property],
    source_id: &str,
    color: &str,
) -> Option<CalendarEvent> {
    let uid = get_prop(props, "UID")?;
    let summary = get_prop(props, "SUMMARY").unwrap_or_default();
    let description = get_prop(props, "DESCRIPTION");
    let location = get_prop(props, "LOCATION");
    let url = get_prop(props, "URL");
    let rrule = get_prop(props, "RRULE");
    let exdates = parse_exdates(props);
    let rdates = parse_rdates(props);

    let dtstart_prop = props.iter().find(|p| p.name == "DTSTART")?;
    let dtstart_value = dtstart_prop.value.as_deref()?;
    let dtstart_tzid = get_param(dtstart_prop, "TZID");

    let all_day = dtstart_value.len() == 8;
    let dtstart = parse_ics_datetime(dtstart_value, dtstart_tzid.as_deref())?;

    let dtend = props
        .iter()
        .find(|p| p.name == "DTEND")
        .and_then(|p| {
            let value = p.value.as_deref()?;
            let tzid = get_param(p, "TZID");
            parse_ics_datetime(value, tzid.as_deref())
        });

    Some(CalendarEvent {
        uid,
        source_id: source_id.to_string(),
        summary,
        description,
        location,
        dtstart,
        dtend,
        all_day,
        url,
        color: color.to_string(),
        etag: None,
        href: None,
        rrule,
        exdates,
        rdates,
    })
}

fn get_prop(props: &[ical::property::Property], name: &str) -> Option<String> {
    props
        .iter()
        .find(|p| p.name == name)
        .and_then(|p| p.value.clone())
}

fn get_param(prop: &ical::property::Property, param_name: &str) -> Option<String> {
    prop.params
        .as_ref()?
        .iter()
        .find(|(k, _)| k == param_name)
        .and_then(|(_, v)| v.first().cloned())
}

/// Parse an iCalendar date/datetime value into a [`Zoned`].
///
/// Handles the common formats:
/// - `20241225`           – all-day date
/// - `20241225T100000Z`   – UTC datetime
/// - `20241225T100000`    – with optional TZID param
pub fn parse_ics_datetime(value: &str, tzid: Option<&str>) -> Option<Zoned> {
    if value.len() == 8 {
        // All-day: YYYYMMDD
        let tm = strtime::parse("%Y%m%d", value).ok()?;
        let date = tm.to_date().ok()?;
        Some(date.at(0, 0, 0, 0).to_zoned(TimeZone::UTC).ok()?)
    } else if value.ends_with('Z') {
        // UTC: YYYYMMDDTHHMMSSz
        let tm = strtime::parse("%Y%m%dT%H%M%SZ", value).ok()?;
        let ts = tm.to_timestamp().ok()?;
        Some(ts.to_zoned(TimeZone::UTC))
    } else if let Some(tzid) = tzid {
        // Localised with TZID parameter
        let tm = strtime::parse("%Y%m%dT%H%M%S", value).ok()?;
        let dt = tm.to_datetime().ok()?;
        let tz = TimeZone::get(tzid).ok()?;
        Some(dt.to_zoned(tz).ok()?)
    } else {
        // Floating – assume UTC
        let tm = strtime::parse("%Y%m%dT%H%M%S", value).ok()?;
        let dt = tm.to_datetime().ok()?;
        Some(dt.to_zoned(TimeZone::UTC).ok()?)
    }
}

// ── EXDATE parsing ─────────────────────────────────────────

fn parse_exdates(props: &[ical::property::Property]) -> Vec<Date> {
    let mut dates = Vec::new();
    for prop in props.iter().filter(|p| p.name == "EXDATE") {
        if let Some(value) = &prop.value {
            let tzid = get_param(prop, "TZID");
            for part in value.split(',') {
                if let Some(zoned) = parse_ics_datetime(part.trim(), tzid.as_deref()) {
                    dates.push(zoned.date());
                }
            }
        }
    }
    dates
}

fn parse_rdates(props: &[ical::property::Property]) -> Vec<Date> {
    let mut dates = Vec::new();
    for prop in props.iter().filter(|p| p.name == "RDATE") {
        if let Some(value) = &prop.value {
            let tzid = get_param(prop, "TZID");
            for part in value.split(',') {
                if let Some(zoned) = parse_ics_datetime(part.trim(), tzid.as_deref()) {
                    dates.push(zoned.date());
                }
            }
        }
    }
    dates
}

// ── CalendarTodo ───────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CalendarTodo {
    pub uid: String,
    pub source_id: String,
    pub summary: String,
    pub description: Option<String>,
    pub due: Option<Zoned>,
    pub completed: bool,
    pub priority: Option<u32>,
    pub color: String,
    pub etag: Option<String>,
    pub href: Option<String>,
}

impl CalendarTodo {
    pub fn to_ics(&self) -> String {
        let mut ics = format!(
            "BEGIN:VCALENDAR\r\n\
             VERSION:2.0\r\n\
             PRODID:-//COSMIC//cosmic-applet-time//EN\r\n\
             BEGIN:VTODO\r\n\
             UID:{uid}\r\n\
             SUMMARY:{summary}\r\n",
            uid = self.uid,
            summary = ics_escape(&self.summary),
        );

        if let Some(due) = &self.due {
            let ts = due.timestamp();
            let utc = ts.to_zoned(TimeZone::UTC);
            ics.push_str(&format!(
                "DUE:{}{:02}{:02}T{:02}{:02}{:02}Z\r\n",
                utc.date().year(),
                utc.date().month(),
                utc.date().day(),
                utc.hour(),
                utc.minute(),
                utc.second()
            ));
        }

        let status = if self.completed {
            "COMPLETED"
        } else {
            "NEEDS-ACTION"
        };
        ics.push_str(&format!("STATUS:{status}\r\n"));

        if self.completed {
            let now = Zoned::now();
            let ts = now.timestamp();
            let utc = ts.to_zoned(TimeZone::UTC);
            ics.push_str(&format!(
                "COMPLETED:{}{:02}{:02}T{:02}{:02}{:02}Z\r\n",
                utc.date().year(),
                utc.date().month(),
                utc.date().day(),
                utc.hour(),
                utc.minute(),
                utc.second()
            ));
        }

        if let Some(prio) = self.priority {
            ics.push_str(&format!("PRIORITY:{prio}\r\n"));
        }

        ics.push_str("END:VTODO\r\nEND:VCALENDAR\r\n");
        ics
    }

    pub fn due_display(&self) -> String {
        if let Some(due) = &self.due {
            format!(
                "{}{:02}{:02}",
                due.date().year(),
                due.date().month(),
                due.date().day()
            )
        } else {
            String::new()
        }
    }
}

/// Parse iCalendar data for VTODO components.
pub fn parse_ics_todos(ics_data: &str, source_id: &str, color: &str) -> Vec<CalendarTodo> {
    use std::io::BufReader;
    let reader = BufReader::new(ics_data.as_bytes());
    let parser = ical::IcalParser::new(reader);
    let mut todos = Vec::new();

    for calendar in parser.flatten() {
        for todo in calendar.todos {
            if let Some(cal_todo) = parse_todo(&todo.properties, source_id, color) {
                todos.push(cal_todo);
            }
        }
    }

    todos
}

fn parse_todo(
    props: &[ical::property::Property],
    source_id: &str,
    color: &str,
) -> Option<CalendarTodo> {
    let uid = get_prop(props, "UID")?;
    let summary = get_prop(props, "SUMMARY").unwrap_or_default();
    let description = get_prop(props, "DESCRIPTION");
    let status = get_prop(props, "STATUS").unwrap_or_default();
    let priority = get_prop(props, "PRIORITY").and_then(|p| p.parse().ok());

    let due = props
        .iter()
        .find(|p| p.name == "DUE")
        .and_then(|p| {
            let value = p.value.as_deref()?;
            let tzid = get_param(p, "TZID");
            parse_ics_datetime(value, tzid.as_deref())
        });

    Some(CalendarTodo {
        uid,
        source_id: source_id.to_string(),
        summary,
        description,
        due,
        completed: status == "COMPLETED",
        priority,
        color: color.to_string(),
        etag: None,
        href: None,
    })
}

// ── RRULE expansion ────────────────────────────────────────

enum RRuleFreq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

struct ParsedRRule {
    freq: RRuleFreq,
    interval: i32,
    count: Option<i32>,
    until: Option<Date>,
    by_day: Vec<(Option<i8>, jiff::civil::Weekday)>,
    by_month: Vec<u8>,
    by_monthday: Vec<i8>,
}

fn parse_rrule_string(rrule: &str) -> Option<ParsedRRule> {
    let mut freq = None;
    let mut interval = 1;
    let mut count = None;
    let mut until = None;
    let mut by_day = Vec::new();
    let mut by_month = Vec::new();
    let mut by_monthday = Vec::new();

    for part in rrule.split(';') {
        let mut kv = part.splitn(2, '=');
        let key = kv.next()?;
        let value = kv.next().unwrap_or("");

        match key {
            "FREQ" => {
                freq = match value {
                    "DAILY" => Some(RRuleFreq::Daily),
                    "WEEKLY" => Some(RRuleFreq::Weekly),
                    "MONTHLY" => Some(RRuleFreq::Monthly),
                    "YEARLY" => Some(RRuleFreq::Yearly),
                    _ => None,
                }
            }
            "INTERVAL" => interval = value.parse().unwrap_or(1),
            "COUNT" => count = value.parse().ok(),
            "UNTIL" => {
                if let Some(zoned) = parse_ics_datetime(value, None) {
                    until = Some(zoned.date());
                }
            }
            "BYDAY" => {
                for day_str in value.split(',') {
                    if let Some(parsed) = parse_byday_token(day_str.trim()) {
                        by_day.push(parsed);
                    }
                }
            }
            "BYMONTH" => {
                for m in value.split(',') {
                    if let Ok(month) = m.trim().parse::<u8>() {
                        if (1..=12).contains(&month) {
                            by_month.push(month);
                        }
                    }
                }
            }
            "BYMONTHDAY" => {
                for d in value.split(',') {
                    if let Ok(day) = d.trim().parse::<i8>() {
                        if (-31..=31).contains(&day) && day != 0 {
                            by_monthday.push(day);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Some(ParsedRRule {
        freq: freq?,
        interval,
        count,
        until,
        by_day,
        by_month,
        by_monthday,
    })
}

/// Parse a BYDAY token like "MO", "2TU", "-1FR" into (optional ordinal, weekday).
fn parse_byday_token(s: &str) -> Option<(Option<i8>, jiff::civil::Weekday)> {
    use jiff::civil::Weekday;
    let day_part = if s.len() >= 2 { &s[s.len() - 2..] } else { return None };
    let weekday = match day_part {
        "MO" => Weekday::Monday,
        "TU" => Weekday::Tuesday,
        "WE" => Weekday::Wednesday,
        "TH" => Weekday::Thursday,
        "FR" => Weekday::Friday,
        "SA" => Weekday::Saturday,
        "SU" => Weekday::Sunday,
        _ => return None,
    };
    let prefix = &s[..s.len() - 2];
    let ordinal = if prefix.is_empty() {
        None
    } else {
        Some(prefix.parse::<i8>().ok()?)
    };
    Some((ordinal, weekday))
}

/// Expand recurring events into individual occurrences within a date range.
///
/// Non-recurring events are passed through unchanged.
pub fn expand_recurring(
    events: Vec<CalendarEvent>,
    range_start: Date,
    range_end: Date,
) -> Vec<CalendarEvent> {
    let mut result = Vec::new();

    for event in events {
        let Some(ref rrule_str) = event.rrule else {
            // No RRULE — still check for RDATE
            if !event.rdates.is_empty() {
                result.push(event.clone());
                for rdate in &event.rdates {
                    if *rdate >= range_start && *rdate <= range_end && !event.exdates.contains(rdate) {
                        if *rdate != event.date() {
                            if let Some(occ) = make_occurrence(&event, *rdate) {
                                result.push(occ);
                            }
                        }
                    }
                }
            } else {
                result.push(event);
            }
            continue;
        };

        let Some(rule) = parse_rrule_string(rrule_str) else {
            // Couldn't parse RRULE — include original as-is
            result.push(event);
            continue;
        };

        let has_by_rules = !rule.by_day.is_empty() || !rule.by_month.is_empty() || !rule.by_monthday.is_empty();

        let original_date = event.date();
        let mut current = original_date;
        let mut count_remaining = rule.count;
        let max_iterations = 1000;

        for _ in 0..max_iterations {
            if current > range_end {
                break;
            }
            if let Some(until) = rule.until {
                if current > until {
                    break;
                }
            }

            if has_by_rules {
                // Expand BY* rules within the current period
                let candidates = expand_by_rules(current, &rule);
                for candidate in candidates {
                    if candidate > range_end {
                        break;
                    }
                    if let Some(until) = rule.until {
                        if candidate > until { break; }
                    }
                    if let Some(ref mut remaining) = count_remaining {
                        if *remaining <= 0 { break; }
                        *remaining -= 1;
                    }
                    if candidate >= range_start && !event.exdates.contains(&candidate) {
                        if candidate == original_date {
                            result.push(event.clone());
                        } else if let Some(occ) = make_occurrence(&event, candidate) {
                            result.push(occ);
                        }
                    }
                }
            } else {
                if let Some(ref mut remaining) = count_remaining {
                    if *remaining <= 0 {
                        break;
                    }
                    *remaining -= 1;
                }

                if current >= range_start && !event.exdates.contains(&current) {
                    if current == original_date {
                        result.push(event.clone());
                    } else if let Some(occ) = make_occurrence(&event, current) {
                        result.push(occ);
                    }
                }
            }

            current = match advance_date(current, &rule) {
                Some(d) => d,
                None => break,
            };
        }

        // RDATE: add extra occurrences
        for rdate in &event.rdates {
            if *rdate >= range_start && *rdate <= range_end && !event.exdates.contains(rdate) {
                if *rdate != original_date {
                    if let Some(occ) = make_occurrence(&event, *rdate) {
                        result.push(occ);
                    }
                }
            }
        }
    }

    result
}

/// Expand BYDAY/BYMONTH/BYMONTHDAY within a single recurrence period starting at `base`.
fn expand_by_rules(base: Date, rule: &ParsedRRule) -> Vec<Date> {
    let mut dates = Vec::new();

    match rule.freq {
        RRuleFreq::Weekly => {
            // BYDAY within the week of `base`
            if !rule.by_day.is_empty() {
                // Find Monday of `base`'s week
                let weekday_num = base.weekday().to_monday_zero_offset() as i64;
                let monday = base.checked_sub(weekday_num.days()).unwrap_or(base);
                for &(_, wd) in &rule.by_day {
                    let offset = wd.to_monday_zero_offset() as i64;
                    if let Ok(d) = monday.checked_add(offset.days()) {
                        dates.push(d);
                    }
                }
                dates.sort();
            } else {
                dates.push(base);
            }
        }
        RRuleFreq::Monthly => {
            let year = base.year();
            let month = base.month();
            if !rule.by_monthday.is_empty() {
                let days_in_month = days_in_month(year, month as u8);
                for &md in &rule.by_monthday {
                    let day = if md > 0 {
                        md as u8
                    } else {
                        let d = days_in_month as i8 + 1 + md;
                        if d < 1 { continue; }
                        d as u8
                    };
                    if day >= 1 && day <= days_in_month {
                        if let Ok(d) = Date::new(year, month as i8, day as i8) {
                            dates.push(d);
                        }
                    }
                }
            } else if !rule.by_day.is_empty() {
                for &(ordinal, wd) in &rule.by_day {
                    if let Some(d) = nth_weekday_in_month(year, month as u8, wd, ordinal) {
                        dates.push(d);
                    }
                }
            } else {
                dates.push(base);
            }
            dates.sort();
        }
        RRuleFreq::Yearly => {
            let year = base.year();
            let months: Vec<u8> = if !rule.by_month.is_empty() {
                rule.by_month.clone()
            } else {
                vec![base.month() as u8]
            };
            for &m in &months {
                if !rule.by_monthday.is_empty() {
                    let dim = days_in_month(year, m);
                    for &md in &rule.by_monthday {
                        let day = if md > 0 { md as u8 } else {
                            let d = dim as i8 + 1 + md;
                            if d < 1 { continue; }
                            d as u8
                        };
                        if day >= 1 && day <= dim {
                            if let Ok(d) = Date::new(year, m as i8, day as i8) {
                                dates.push(d);
                            }
                        }
                    }
                } else if !rule.by_day.is_empty() {
                    for &(ordinal, wd) in &rule.by_day {
                        if let Some(d) = nth_weekday_in_month(year, m, wd, ordinal) {
                            dates.push(d);
                        }
                    }
                } else {
                    let day = base.day().min(days_in_month(year, m) as i8);
                    if let Ok(d) = Date::new(year, m as i8, day) {
                        dates.push(d);
                    }
                }
            }
            dates.sort();
        }
        RRuleFreq::Daily => {
            // BY* rules don't typically apply to DAILY, just return base
            dates.push(base);
        }
    }

    dates
}

fn days_in_month(year: i16, month: u8) -> u8 {
    let m = month.clamp(1, 12);
    let next = if m == 12 {
        Date::new(year + 1, 1, 1)
    } else {
        Date::new(year, (m + 1) as i8, 1)
    };
    let this = Date::new(year, m as i8, 1);
    match (this, next) {
        (Ok(a), Ok(b)) => {
            b.since(a).map(|s| s.get_days() as u8).unwrap_or(30)
        }
        _ => 30,
    }
}

/// Find the nth occurrence of a weekday in a given month.
/// `ordinal`: None = all matching days, Some(n) = nth (1-based, negative counts from end).
fn nth_weekday_in_month(
    year: i16,
    month: u8,
    weekday: jiff::civil::Weekday,
    ordinal: Option<i8>,
) -> Option<Date> {
    let dim = days_in_month(year, month);
    let mut matches = Vec::new();
    for day in 1..=dim {
        if let Ok(d) = Date::new(year, month as i8, day as i8) {
            if d.weekday() == weekday {
                matches.push(d);
            }
        }
    }
    match ordinal {
        None | Some(0) => matches.first().copied(),
        Some(n) if n > 0 => matches.get((n - 1) as usize).copied(),
        Some(n) => {
            let idx = matches.len() as i8 + n;
            if idx >= 0 {
                matches.get(idx as usize).copied()
            } else {
                None
            }
        }
    }
}

fn advance_date(current: Date, rule: &ParsedRRule) -> Option<Date> {
    match rule.freq {
        RRuleFreq::Daily => current.checked_add(rule.interval.days()).ok(),
        RRuleFreq::Weekly => current.checked_add((rule.interval * 7).days()).ok(),
        RRuleFreq::Monthly => current.checked_add(rule.interval.months()).ok(),
        RRuleFreq::Yearly => current.checked_add(rule.interval.years()).ok(),
    }
}

fn make_occurrence(event: &CalendarEvent, new_date: Date) -> Option<CalendarEvent> {
    let day_diff = new_date.since(event.date()).ok()?;
    let new_start = event.dtstart.checked_add(day_diff).ok()?;
    let new_end = event
        .dtend
        .as_ref()
        .and_then(|end| end.checked_add(day_diff).ok());

    Some(CalendarEvent {
        uid: format!("{}_{}", event.uid, new_date),
        dtstart: new_start,
        dtend: new_end,
        rrule: None,
        exdates: Vec::new(),
        rdates: Vec::new(),
        ..event.clone()
    })
}
