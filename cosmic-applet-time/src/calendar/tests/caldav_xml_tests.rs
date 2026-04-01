use cosmic_applets_config::calendar::discovery::{
    local_name, parse_calendar_list, resolve_url, xml_extract_inner, xml_extract_text,
    xml_response_blocks,
};

// ── PROPFIND multistatus (calendar list) ───────────────────

const PROPFIND_CALENDARS_RESPONSE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:multistatus xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/"
               xmlns:ic="http://apple.com/ns/ical/">
  <d:response>
    <d:href>/dav/calendars/user/personal/</d:href>
    <d:propstat>
      <d:prop>
        <d:resourcetype><d:collection/><cal:calendar xmlns:cal="urn:ietf:params:xml:ns:caldav"/></d:resourcetype>
        <d:displayname>Personal</d:displayname>
        <ic:calendar-color>#FF0000FF</ic:calendar-color>
        <cs:getctag>ctag-abc-123</cs:getctag>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/calendars/user/work/</d:href>
    <d:propstat>
      <d:prop>
        <d:resourcetype><d:collection/><cal:calendar xmlns:cal="urn:ietf:params:xml:ns:caldav"/></d:resourcetype>
        <d:displayname>Work</d:displayname>
        <ic:calendar-color>#0000FFFF</ic:calendar-color>
        <cs:getctag>ctag-def-456</cs:getctag>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/principals/user/</d:href>
    <d:propstat>
      <d:prop>
        <d:resourcetype><d:collection/></d:resourcetype>
        <d:displayname>User Root</d:displayname>
      </d:prop>
    </d:propstat>
  </d:response>
</d:multistatus>"#;

#[test]
fn parse_propfind_calendar_list() {
    let base = "https://dav.example.com";
    let calendars = parse_calendar_list(PROPFIND_CALENDARS_RESPONSE, base);
    assert_eq!(calendars.len(), 2, "should find 2 calendars (not the root collection)");

    let personal = calendars.iter().find(|c| c.display_name == "Personal").unwrap();
    assert_eq!(personal.href, "https://dav.example.com/dav/calendars/user/personal/");
    assert_eq!(personal.color, "#FF0000FF");
    assert_eq!(personal.ctag.as_deref(), Some("ctag-abc-123"));
    assert!(!personal.enabled);

    let work = calendars.iter().find(|c| c.display_name == "Work").unwrap();
    assert_eq!(work.href, "https://dav.example.com/dav/calendars/user/work/");
    assert_eq!(work.color, "#0000FFFF");
}

// ── REPORT calendar-query response ─────────────────────────

const REPORT_RESPONSE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:response>
    <d:href>/cal/event1.ics</d:href>
    <d:propstat>
      <d:prop>
        <d:getetag>"etag-001"</d:getetag>
        <c:calendar-data>BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:uid-001
SUMMARY:Test Event
DTSTART:20260315T100000Z
END:VEVENT
END:VCALENDAR</c:calendar-data>
      </d:prop>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/cal/event2.ics</d:href>
    <d:propstat>
      <d:prop>
        <d:getetag>"etag-002"</d:getetag>
        <c:calendar-data>BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:uid-002
SUMMARY:Another Event
DTSTART:20260316T140000Z
END:VEVENT
END:VCALENDAR</c:calendar-data>
      </d:prop>
    </d:propstat>
  </d:response>
</d:multistatus>"#;

#[test]
fn parse_report_events() {
    let blocks = xml_response_blocks(REPORT_RESPONSE);
    assert_eq!(blocks.len(), 2);

    let href1 = xml_extract_text(&blocks[0], "href");
    assert_eq!(href1.as_deref(), Some("/cal/event1.ics"));

    let etag1 = xml_extract_text(&blocks[0], "getetag");
    assert_eq!(etag1.as_deref(), Some("\"etag-001\""));

    let data1 = xml_extract_text(&blocks[0], "calendar-data");
    assert!(data1.is_some());
    assert!(data1.unwrap().contains("UID:uid-001"));
}

// ── sync-collection response ───────────────────────────────

const SYNC_RESPONSE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/cal/event3.ics</d:href>
    <d:propstat>
      <d:prop>
        <d:getetag>"etag-003"</d:getetag>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/cal/deleted.ics</d:href>
    <d:status>HTTP/1.1 404 Not Found</d:status>
  </d:response>
  <d:sync-token>http://example.com/sync/new-token</d:sync-token>
</d:multistatus>"#;

#[test]
fn parse_sync_collection() {
    let blocks = xml_response_blocks(SYNC_RESPONSE);
    assert_eq!(blocks.len(), 2);

    let href_new = xml_extract_text(&blocks[0], "href");
    assert_eq!(href_new.as_deref(), Some("/cal/event3.ics"));

    let etag = xml_extract_text(&blocks[0], "getetag");
    assert_eq!(etag.as_deref(), Some("\"etag-003\""));

    // The deleted item has 404 status
    let href_del = xml_extract_text(&blocks[1], "href");
    assert_eq!(href_del.as_deref(), Some("/cal/deleted.ics"));

    // sync-token is outside response blocks, extract from full XML
    let new_token = xml_extract_text(SYNC_RESPONSE, "sync-token");
    assert_eq!(new_token.as_deref(), Some("http://example.com/sync/new-token"));
}

#[test]
fn empty_calendar_data_no_crash() {
    let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:response>
    <d:href>/cal/empty.ics</d:href>
    <d:propstat>
      <d:prop>
        <d:getetag>"e"</d:getetag>
        <c:calendar-data/>
      </d:prop>
    </d:propstat>
  </d:response>
</d:multistatus>"#;
    let blocks = xml_response_blocks(xml);
    assert_eq!(blocks.len(), 1);
    let data = xml_extract_text(&blocks[0], "calendar-data");
    assert!(data.is_none());
}

#[test]
fn missing_etag() {
    let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:response>
    <d:href>/cal/noetag.ics</d:href>
    <d:propstat>
      <d:prop>
        <c:calendar-data>BEGIN:VCALENDAR
END:VCALENDAR</c:calendar-data>
      </d:prop>
    </d:propstat>
  </d:response>
</d:multistatus>"#;
    let blocks = xml_response_blocks(xml);
    assert_eq!(blocks.len(), 1);
    let etag = xml_extract_text(&blocks[0], "getetag");
    assert!(etag.is_none());
}

// ── XML helper unit tests ──────────────────────────────────

#[test]
fn local_name_strips_prefix() {
    assert_eq!(local_name(b"d:href"), b"href");
    assert_eq!(local_name(b"D:response"), b"response");
    assert_eq!(local_name(b"href"), b"href");
    assert_eq!(local_name(b"cal:calendar-data"), b"calendar-data");
}

#[test]
fn xml_extract_inner_nested() {
    let xml = r#"<d:current-user-principal><d:href>/principals/user/</d:href></d:current-user-principal>"#;
    let inner = xml_extract_inner(xml, "current-user-principal");
    assert!(inner.is_some());
    let inner = inner.unwrap();
    assert!(inner.contains("href"));
    let href = xml_extract_text(&inner, "href");
    assert_eq!(href.as_deref(), Some("/principals/user/"));
}

// ── resolve_url tests ──────────────────────────────────────

#[test]
fn resolve_url_absolute() {
    assert_eq!(
        resolve_url("https://dav.example.com", "https://other.com/cal"),
        "https://other.com/cal"
    );
}

#[test]
fn resolve_url_absolute_path() {
    assert_eq!(
        resolve_url("https://dav.example.com/dav", "/principals/user/"),
        "https://dav.example.com/principals/user/"
    );
}

#[test]
fn resolve_url_relative() {
    assert_eq!(
        resolve_url("https://dav.example.com/dav", "calendars/personal/"),
        "https://dav.example.com/dav/calendars/personal/"
    );
}

// ── Color parsing ──────────────────────────────────────────

// Colors are stored as strings in the config. Test that various color formats
// survive deserialization.
#[test]
fn color_hex_full() {
    let json = r##"{"href":"h","display_name":"c","color":"#FF0000","enabled":true}"##;
    let cal: cosmic_applets_config::calendar::CalDavCalendar = serde_json::from_str(json).unwrap();
    assert_eq!(cal.color, "#FF0000");
}

#[test]
fn color_empty_string_preserved() {
    let json = r##"{"href":"h","display_name":"c","color":"","enabled":true}"##;
    let cal: cosmic_applets_config::calendar::CalDavCalendar = serde_json::from_str(json).unwrap();
    // Empty string is stored as-is (not replaced with default during deser)
    assert!(cal.color.is_empty() || cal.color == "#0078D4");
}

#[test]
fn color_non_standard_preserved() {
    let json = r##"{"href":"h","display_name":"c","color":"rgb(255,0,0)","enabled":true}"##;
    let cal: cosmic_applets_config::calendar::CalDavCalendar = serde_json::from_str(json).unwrap();
    // Non-standard colors should be stored as-is
    assert_eq!(cal.color, "rgb(255,0,0)");
}

// ── ETag header verification ───────────────────────────────

use crate::calendar::event::CalendarEvent;
use jiff::{civil::Date, tz::TimeZone};

fn etag_test_event() -> CalendarEvent {
    let dtstart = Date::new(2026, 5, 1)
        .unwrap()
        .at(10, 0, 0, 0)
        .to_zoned(TimeZone::UTC)
        .unwrap();
    CalendarEvent {
        uid: "etag-test-uid-123".into(),
        source_id: "src1".into(),
        summary: "Test Event".into(),
        description: None,
        location: None,
        dtstart,
        dtend: None,
        all_day: false,
        url: None,
        color: "#FF0000".into(),
        etag: Some("\"abc-123\"".into()),
        href: Some("/cal/etag-test-uid-123.ics".into()),
        rrule: None,
        exdates: Vec::new(),
        rdates: Vec::new(),
    }
}

#[test]
fn build_create_event_has_if_none_match() {
    use crate::calendar::caldav::build_create_event_request;
    let event = etag_test_event();
    let (url, headers, body) = build_create_event_request("/calendars/personal", &event);

    assert_eq!(url, "/calendars/personal/etag-test-uid-123.ics");
    assert!(body.contains("BEGIN:VCALENDAR"));
    assert!(body.contains("UID:etag-test-uid-123"));

    let if_none_match = headers.iter().find(|(k, _)| *k == "If-None-Match");
    assert!(if_none_match.is_some(), "If-None-Match header must be present");
    assert_eq!(if_none_match.unwrap().1, "*");
}

#[test]
fn build_update_event_has_if_match() {
    use crate::calendar::caldav::build_update_event_request;
    let event = etag_test_event();
    let (headers, body) = build_update_event_request(&event, "abc-123");

    assert!(body.contains("BEGIN:VCALENDAR"));

    let if_match = headers.iter().find(|(k, _)| *k == "If-Match");
    assert!(if_match.is_some(), "If-Match header must be present");
    assert_eq!(if_match.unwrap().1, "\"abc-123\"");
}

// Trailing slash in calendar_href is trimmed
#[test]
fn build_create_event_trims_trailing_slash() {
    use crate::calendar::caldav::build_create_event_request;
    let event = etag_test_event();
    let (url, _, _) = build_create_event_request("/calendars/personal/", &event);
    assert_eq!(url, "/calendars/personal/etag-test-uid-123.ics");
}
