// SPDX-License-Identifier: GPL-3.0-only

use crate::calendar::config::{AuthMethod, CalDavCalendar};
use crate::calendar::event::{parse_ics_events, parse_ics_todos, CalendarEvent, CalendarTodo};
use crate::calendar::secrets::{self, SecretKind};
use crate::calendar::{auth, SyncError};
use quick_xml::events::Event as XmlEvent;
use quick_xml::Reader;
use std::borrow::Cow;
use std::sync::LazyLock;

/// Shared HTTP client for CalDAV requests.
/// Reused across all requests to enable connection pooling and keep-alive.
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("cosmic-applet-time/1.0")
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("Failed to create HTTP client")
});

/// Return a reqwest client, optionally configured with a custom CA certificate.
///
/// When `ca_cert_path` is `None`, the shared static client is returned.
/// When a path is provided, a new client is built with the extra root certificate.
fn get_client(ca_cert_path: Option<&str>) -> Result<Cow<'static, reqwest::Client>, SyncError> {
    match ca_cert_path {
        None => Ok(Cow::Borrowed(&*HTTP_CLIENT)),
        Some(path) => {
            let pem = std::fs::read(path)
                .map_err(|e| SyncError::Other(format!("Failed to read CA cert {path}: {e}")))?;
            let cert = reqwest::tls::Certificate::from_pem(&pem)
                .map_err(|e| SyncError::Other(format!("Invalid CA cert PEM: {e}")))?;
            let client = reqwest::Client::builder()
                .user_agent("cosmic-applet-time/1.0")
                .redirect(reqwest::redirect::Policy::limited(5))
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .add_root_certificate(cert)
                .build()
                .map_err(|e| SyncError::Other(format!("Failed to build client with CA: {e}")))?;
            Ok(Cow::Owned(client))
        }
    }
}

/// XML body for a CalDAV REPORT calendar-query that fetches all VEVENT data.
const CALENDAR_QUERY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <d:getetag/>
    <c:calendar-data/>
  </d:prop>
  <c:filter>
    <c:comp-filter name="VCALENDAR">
      <c:comp-filter name="VEVENT"/>
    </c:comp-filter>
  </c:filter>
</c:calendar-query>"#;

/// Build a CalDAV REPORT calendar-query with a time-range filter.
fn calendar_query_with_range(start: &str, end: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <d:getetag/>
    <c:calendar-data/>
  </d:prop>
  <c:filter>
    <c:comp-filter name="VCALENDAR">
      <c:comp-filter name="VEVENT">
        <c:time-range start="{start}" end="{end}"/>
      </c:comp-filter>
    </c:comp-filter>
  </c:filter>
</c:calendar-query>"#
    )
}

/// CalDAV REPORT calendar-query for VTODO components.
const VTODO_QUERY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <d:getetag/>
    <c:calendar-data/>
  </d:prop>
  <c:filter>
    <c:comp-filter name="VCALENDAR">
      <c:comp-filter name="VTODO"/>
    </c:comp-filter>
  </c:filter>
</c:calendar-query>"#;

/// PROPFIND body to discover the current-user-principal.
const PROPFIND_PRINCIPAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:current-user-principal/>
  </d:prop>
</d:propfind>"#;

/// PROPFIND body to discover the calendar-home-set.
const PROPFIND_HOME_SET: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <c:calendar-home-set/>
  </d:prop>
</d:propfind>"#;

/// PROPFIND body to list calendars and their properties.
const PROPFIND_CALENDARS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"
            xmlns:cs="http://calendarserver.org/ns/"
            xmlns:ic="http://apple.com/ns/ical/">
  <d:prop>
    <d:resourcetype/>
    <d:displayname/>
    <ic:calendar-color/>
    <cs:getctag/>
  </d:prop>
</d:propfind>"#;

/// Discover all calendars available on a CalDAV server.
///
/// Performs the standard CalDAV discovery chain:
///   1. PROPFIND on `server_url` → `current-user-principal`
///   2. PROPFIND on principal   → `calendar-home-set`
///   3. PROPFIND on home-set    → list of calendar collections
pub async fn discover_calendars(
    server_url: &str,
    auth: &AuthMethod,
    source_id: &str,
    ca_cert_path: Option<&str>,
) -> Result<Vec<CalDavCalendar>, SyncError> {
    let client = get_client(ca_cert_path)?;
    let base = server_url.trim_end_matches('/');

    // Step 1: Discover principal
    let principal_body = propfind(&client, base, "0", PROPFIND_PRINCIPAL, auth, source_id).await?;
    let principal_href = xml_extract_inner(&principal_body, "current-user-principal")
        .and_then(|block| xml_extract_text(&block, "href"))
        .unwrap_or_default();

    let principal_url = resolve_url(base, &principal_href);

    // Step 2: Discover calendar-home-set
    let home_body = propfind(&client, &principal_url, "0", PROPFIND_HOME_SET, auth, source_id).await?;
    let home_href = xml_extract_inner(&home_body, "calendar-home-set")
        .and_then(|block| xml_extract_text(&block, "href"))
        .unwrap_or_default();

    let home_url = resolve_url(base, &home_href);

    // Step 3: List calendars in the home-set
    let cal_body = propfind(&client, &home_url, "1", PROPFIND_CALENDARS, auth, source_id).await?;
    let calendars = parse_calendar_list(&cal_body, base);

    Ok(calendars)
}

/// Fetch calendar events from a single CalDAV calendar collection.
pub async fn fetch_events(
    calendar_href: &str,
    server_url: &str,
    auth: &AuthMethod,
    source_id: &str,
    color: &str,
    time_range: Option<(&str, &str)>,
    ca_cert_path: Option<&str>,
) -> Result<Vec<CalendarEvent>, SyncError> {
    let client = get_client(ca_cert_path)?;
    let base = server_url.trim_end_matches('/');
    let url = resolve_url(base, calendar_href);

    let query = match time_range {
        Some((start, end)) => calendar_query_with_range(start, end),
        None => CALENDAR_QUERY.to_string(),
    };

    let body = report_request(&client, &url, &query, auth, source_id).await?;

    let entries = extract_response_entries(&body);
    let mut all_events = Vec::new();

    for entry in entries {
        let mut events = parse_ics_events(&entry.calendar_data, source_id, color);
        for ev in &mut events {
            ev.etag = entry.etag.clone();
            ev.href = entry.href.clone();
        }
        all_events.extend(events);
    }

    Ok(all_events)
}

/// Fetch VTODO items from a single CalDAV calendar collection.
pub async fn fetch_todos(
    calendar_href: &str,
    server_url: &str,
    auth: &AuthMethod,
    source_id: &str,
    color: &str,
    ca_cert_path: Option<&str>,
) -> Result<Vec<CalendarTodo>, SyncError> {
    let client = get_client(ca_cert_path)?;
    let base = server_url.trim_end_matches('/');
    let url = resolve_url(base, calendar_href);

    let body = report_request(&client, &url, VTODO_QUERY, auth, source_id).await?;

    let entries = extract_response_entries(&body);
    let mut all_todos = Vec::new();

    for entry in entries {
        let mut todos = parse_ics_todos(&entry.calendar_data, source_id, color);
        for todo in &mut todos {
            todo.etag = entry.etag.clone();
            todo.href = entry.href.clone();
        }
        all_todos.extend(todos);
    }

    Ok(all_todos)
}

/// Create a new event on a CalDAV calendar via PUT.
///
/// The event is serialised to iCalendar format and PUT to
/// `{calendar_href}/{uid}.ics` with `If-None-Match: *` to ensure we only
/// create (not overwrite).
pub async fn create_event(
    calendar_href: &str,
    event: &CalendarEvent,
    auth: &AuthMethod,
    source_id: &str,
    ca_cert_path: Option<&str>,
) -> Result<(), SyncError> {
    let client = get_client(ca_cert_path)?;
    let href = calendar_href.trim_end_matches('/');
    let url = format!("{href}/{}.ics", event.uid);
    let ics_body = event.to_ics();

    let result = put_event(&client, &url, &ics_body, auth, source_id).await;

    match result {
        Ok(()) => Ok(()),
        Err(SyncError::AuthExpired(ref sid)) => {
            // Try OIDC token refresh and retry once
            if let Some(refreshed_token) = try_oidc_refresh(auth, sid).await? {
                put_event_direct(&client, &url, &ics_body, &refreshed_token).await
            } else {
                Err(SyncError::AuthExpired(sid.clone()))
            }
        }
        Err(e) => Err(e),
    }
}

/// Send a PUT request to create an event.  Returns `AuthExpired` on 401.
async fn put_event(
    client: &reqwest::Client,
    url: &str,
    ics_body: &str,
    auth: &AuthMethod,
    source_id: &str,
) -> Result<(), SyncError> {
    let mut request = client
        .put(url)
        .header("Content-Type", "text/calendar; charset=utf-8")
        .header("If-None-Match", "*")
        .body(ics_body.to_string());

    request = apply_auth(request, auth, source_id).await?;

    let response = request
        .send()
        .await
        .map_err(|e| SyncError::Other(format!("CalDAV PUT failed: {e}")))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(SyncError::AuthExpired(source_id.to_string()));
    }

    let status = response.status();
    if !status.is_success() && status != reqwest::StatusCode::CREATED {
        return Err(SyncError::Other(format!(
            "CalDAV PUT returned status {status}"
        )));
    }

    Ok(())
}

/// PUT with a direct bearer token (used after OIDC refresh).
async fn put_event_direct(
    client: &reqwest::Client,
    url: &str,
    ics_body: &str,
    access_token: &str,
) -> Result<(), SyncError> {
    let request = client
        .put(url)
        .header("Content-Type", "text/calendar; charset=utf-8")
        .header("If-None-Match", "*")
        .body(ics_body.to_string());

    let request = apply_auth_direct(request, access_token);

    let response = request
        .send()
        .await
        .map_err(|e| SyncError::Other(format!("CalDAV PUT retry failed: {e}")))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(SyncError::AuthExpired("unknown".to_string()));
    }

    let status = response.status();
    if !status.is_success() && status != reqwest::StatusCode::CREATED {
        return Err(SyncError::Other(format!(
            "CalDAV PUT retry returned status {status}"
        )));
    }

    Ok(())
}

/// Send a REPORT request with the given query body.  On 401, attempt OIDC
/// token refresh (if applicable) and retry once.
async fn report_request(
    client: &reqwest::Client,
    url: &str,
    query_body: &str,
    auth: &AuthMethod,
    source_id: &str,
) -> Result<String, SyncError> {
    let method = reqwest::Method::from_bytes(b"REPORT").expect("REPORT is a valid method");

    let mut request = client
        .request(method, url)
        .header("Depth", "1")
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(query_body.to_string());

    request = apply_auth(request, auth, source_id).await?;

    let response = request
        .send()
        .await
        .map_err(|e| SyncError::Other(format!("CalDAV request failed: {e}")))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        // Try refreshing OIDC token if applicable
        if let Some(refreshed_auth) = try_oidc_refresh(auth, source_id).await? {
            let method = reqwest::Method::from_bytes(b"REPORT").expect("REPORT is a valid method");
            let mut retry = client
                .request(method, url)
                .header("Depth", "1")
                .header("Content-Type", "application/xml; charset=utf-8")
                .body(query_body.to_string());

            retry = apply_auth_direct(retry, &refreshed_auth);

            let retry_response = retry
                .send()
                .await
                .map_err(|e| SyncError::Other(format!("CalDAV retry failed: {e}")))?;

            if retry_response.status() == reqwest::StatusCode::UNAUTHORIZED {
                return Err(SyncError::AuthExpired(source_id.to_string()));
            }

            if !retry_response.status().is_success() {
                return Err(SyncError::Other(format!(
                    "CalDAV server returned status {}",
                    retry_response.status()
                )));
            }

            return retry_response
                .text()
                .await
                .map_err(|e| SyncError::Other(format!("Failed to read CalDAV response: {e}")));
        }

        return Err(SyncError::AuthExpired(source_id.to_string()));
    }

    if !response.status().is_success() {
        return Err(SyncError::Other(format!(
            "CalDAV server returned status {}",
            response.status()
        )));
    }

    response
        .text()
        .await
        .map_err(|e| SyncError::Other(format!("Failed to read CalDAV response: {e}")))
}

/// Update an existing CalDAV event via PUT with ETag-based concurrency control.
pub async fn update_event(
    event_href: &str,
    event: &CalendarEvent,
    etag: &str,
    auth: &AuthMethod,
    source_id: &str,
    ca_cert_path: Option<&str>,
) -> Result<(), SyncError> {
    let client = get_client(ca_cert_path)?;
    let ics_body = event.to_ics();

    let mut request = client
        .put(event_href)
        .header("Content-Type", "text/calendar; charset=utf-8")
        .header("If-Match", format!("\"{etag}\""))
        .body(ics_body.clone());

    request = apply_auth(request, auth, source_id).await?;

    let response = request
        .send()
        .await
        .map_err(|e| SyncError::Other(format!("CalDAV PUT (update) failed: {e}")))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        if let Some(refreshed) = try_oidc_refresh(auth, source_id).await? {
            let retry = apply_auth_direct(
                client
                    .put(event_href)
                    .header("Content-Type", "text/calendar; charset=utf-8")
                    .header("If-Match", format!("\"{etag}\""))
                    .body(ics_body),
                &refreshed,
            );
            let retry_response = retry
                .send()
                .await
                .map_err(|e| SyncError::Other(format!("CalDAV PUT retry failed: {e}")))?;
            if !retry_response.status().is_success() {
                return Err(SyncError::Other(format!(
                    "CalDAV PUT returned {}",
                    retry_response.status()
                )));
            }
            return Ok(());
        }
        return Err(SyncError::AuthExpired(source_id.to_string()));
    }

    if response.status() == reqwest::StatusCode::PRECONDITION_FAILED {
        return Err(SyncError::Other(
            "Event was modified on the server. Please refresh and try again.".into(),
        ));
    }

    if !response.status().is_success() {
        return Err(SyncError::Other(format!(
            "CalDAV PUT returned {}",
            response.status()
        )));
    }

    Ok(())
}

/// Delete a CalDAV event via DELETE with ETag-based concurrency control.
pub async fn delete_event(
    event_href: &str,
    etag: &str,
    auth: &AuthMethod,
    source_id: &str,
    ca_cert_path: Option<&str>,
) -> Result<(), SyncError> {
    let client = get_client(ca_cert_path)?;

    let mut request = client
        .delete(event_href)
        .header("If-Match", format!("\"{etag}\""));

    request = apply_auth(request, auth, source_id).await?;

    let response = request
        .send()
        .await
        .map_err(|e| SyncError::Other(format!("CalDAV DELETE failed: {e}")))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        if let Some(refreshed) = try_oidc_refresh(auth, source_id).await? {
            let retry = apply_auth_direct(
                client
                    .delete(event_href)
                    .header("If-Match", format!("\"{etag}\"")),
                &refreshed,
            );
            let retry_response = retry
                .send()
                .await
                .map_err(|e| SyncError::Other(format!("CalDAV DELETE retry failed: {e}")))?;
            let s = retry_response.status();
            if !s.is_success() && s != reqwest::StatusCode::NO_CONTENT {
                return Err(SyncError::Other(format!("CalDAV DELETE returned {s}")));
            }
            return Ok(());
        }
        return Err(SyncError::AuthExpired(source_id.to_string()));
    }

    let s = response.status();
    if !s.is_success() && s != reqwest::StatusCode::NO_CONTENT {
        return Err(SyncError::Other(format!("CalDAV DELETE returned {s}")));
    }

    Ok(())
}

/// Toggle a VTODO's completion status via CalDAV PUT.
pub async fn complete_todo(
    todo_href: &str,
    todo: &CalendarTodo,
    etag: &str,
    auth: &AuthMethod,
    source_id: &str,
    ca_cert_path: Option<&str>,
) -> Result<(), SyncError> {
    let mut toggled = todo.clone();
    toggled.completed = !toggled.completed;
    let ics_body = toggled.to_ics();

    let client = get_client(ca_cert_path)?;

    let mut request = client
        .put(todo_href)
        .header("Content-Type", "text/calendar; charset=utf-8")
        .header("If-Match", format!("\"{etag}\""))
        .body(ics_body.clone());

    request = apply_auth(request, auth, source_id).await?;

    let response = request
        .send()
        .await
        .map_err(|e| SyncError::Other(format!("CalDAV PUT (todo) failed: {e}")))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        if let Some(refreshed) = try_oidc_refresh(auth, source_id).await? {
            let retry = apply_auth_direct(
                client
                    .put(todo_href)
                    .header("Content-Type", "text/calendar; charset=utf-8")
                    .header("If-Match", format!("\"{etag}\""))
                    .body(ics_body),
                &refreshed,
            );
            let retry_response = retry
                .send()
                .await
                .map_err(|e| SyncError::Other(format!("CalDAV PUT retry failed: {e}")))?;
            if !retry_response.status().is_success() {
                return Err(SyncError::Other(format!(
                    "CalDAV PUT returned {}",
                    retry_response.status()
                )));
            }
            return Ok(());
        }
        return Err(SyncError::AuthExpired(source_id.to_string()));
    }

    if !response.status().is_success() {
        return Err(SyncError::Other(format!(
            "CalDAV PUT returned {}",
            response.status()
        )));
    }

    Ok(())
}

async fn propfind(
    client: &reqwest::Client,
    url: &str,
    depth: &str,
    body: &str,
    auth: &AuthMethod,
    source_id: &str,
) -> Result<String, SyncError> {
    let method = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND is a valid method");
    let mut request = client
        .request(method, url)
        .header("Depth", depth)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body.to_string());

    request = apply_auth(request, auth, source_id).await?;

    let response = request
        .send()
        .await
        .map_err(|e| SyncError::Other(format!("PROPFIND request to {url} failed: {e}")))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        // Try refreshing OIDC token if applicable
        if let Some(refreshed_auth) = try_oidc_refresh(auth, source_id).await? {
            let method = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND is a valid method");
            let mut retry = client
                .request(method, url)
                .header("Depth", depth)
                .header("Content-Type", "application/xml; charset=utf-8")
                .body(body.to_string());

            retry = apply_auth_direct(retry, &refreshed_auth);

            let retry_response = retry
                .send()
                .await
                .map_err(|e| SyncError::Other(format!("PROPFIND retry to {url} failed: {e}")))?;

            if retry_response.status() == reqwest::StatusCode::UNAUTHORIZED {
                return Err(SyncError::AuthExpired(source_id.to_string()));
            }

            if !retry_response.status().is_success() {
                return Err(SyncError::Other(format!(
                    "PROPFIND to {url} returned status {}",
                    retry_response.status()
                )));
            }

            return retry_response
                .text()
                .await
                .map_err(|e| SyncError::Other(format!("Failed to read PROPFIND response: {e}")));
        }

        return Err(SyncError::AuthExpired(source_id.to_string()));
    }

    if !response.status().is_success() {
        return Err(SyncError::Other(format!(
            "PROPFIND to {url} returned status {}",
            response.status()
        )));
    }

    response
        .text()
        .await
        .map_err(|e| SyncError::Other(format!("Failed to read PROPFIND response: {e}")))
}

/// Resolve a potentially-relative href against the server base URL.
fn resolve_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        href.to_string()
    } else if href.starts_with('/') {
        // Absolute path – combine with origin
        if let Some(origin_end) = base.find("://").map(|i| {
            base[i + 3..]
                .find('/')
                .map_or(base.len(), |slash| i + 3 + slash)
        }) {
            format!("{}{}", &base[..origin_end], href)
        } else {
            format!("{base}{href}")
        }
    } else {
        format!("{base}/{href}")
    }
}

/// Parse the PROPFIND Depth:1 response to extract calendar collections.
fn parse_calendar_list(xml: &str, base: &str) -> Vec<CalDavCalendar> {
    let mut calendars = Vec::new();

    for block in xml_response_blocks(xml) {
        // Only include calendar collections (resourcetype contains <calendar/>)
        let is_calendar = block.contains("calendar")
            && block.contains("resourcetype")
            && block.contains("collection");

        if is_calendar {
            let href = xml_extract_text(&block, "href").unwrap_or_default();
            let display_name = xml_extract_text(&block, "displayname")
                .unwrap_or_else(|| href.trim_end_matches('/').rsplit('/').next().unwrap_or("Calendar").to_string());
            let color = xml_extract_text(&block, "calendar-color");
            let ctag = xml_extract_text(&block, "getctag");

            if !href.is_empty() {
                let full_href = if href.starts_with("http://") || href.starts_with("https://") {
                    href
                } else {
                    resolve_url(base, &href)
                };

                calendars.push(CalDavCalendar {
                    href: full_href,
                    display_name,
                    color,
                    enabled: false, // new calendars are disabled until user opts in
                    ctag,
                    sync_token: None,
                });
            }
        }
    }

    calendars
}

/// Fetch the current ctag for a single CalDAV calendar collection.
///
/// Returns `None` if the server doesn't support ctag or the property
/// is missing from the response.
pub async fn fetch_ctag(
    calendar_href: &str,
    server_url: &str,
    auth: &AuthMethod,
    source_id: &str,
    ca_cert_path: Option<&str>,
) -> Result<Option<String>, SyncError> {
    const PROPFIND_CTAG: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/">
  <d:prop>
    <cs:getctag/>
  </d:prop>
</d:propfind>"#;

    let client = get_client(ca_cert_path)?;
    let base = server_url.trim_end_matches('/');
    let url = resolve_url(base, calendar_href);
    let body = propfind(&client, &url, "0", PROPFIND_CTAG, auth, source_id).await?;

    Ok(xml_extract_text(&body, "getctag"))
}

/// Result of a sync-collection REPORT (RFC 6578).
pub struct SyncCollectionResult {
    pub events: Vec<CalendarEvent>,
    pub todos: Vec<CalendarTodo>,
    /// Hrefs that have been deleted on the server.
    pub removed_hrefs: Vec<String>,
    /// New sync-token to store for the next incremental sync.
    pub new_sync_token: Option<String>,
}

/// Perform a WebDAV sync-collection REPORT (RFC 6578) for incremental sync.
///
/// Sends the stored `sync_token` and receives only changes since that token.
/// If the server returns 403/409 (invalid token), returns `Ok(None)` so the
/// caller can fall back to a full fetch.
pub async fn sync_collection(
    calendar_href: &str,
    server_url: &str,
    auth: &AuthMethod,
    source_id: &str,
    color: &str,
    sync_token: &str,
    ca_cert_path: Option<&str>,
) -> Result<Option<SyncCollectionResult>, SyncError> {
    let query = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<d:sync-collection xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:sync-token>{sync_token}</d:sync-token>
  <d:sync-level>1</d:sync-level>
  <d:prop>
    <d:getetag/>
    <c:calendar-data/>
  </d:prop>
</d:sync-collection>"#
    );

    let client = get_client(ca_cert_path)?;
    let base = server_url.trim_end_matches('/');
    let url = resolve_url(base, calendar_href);

    let method = reqwest::Method::from_bytes(b"REPORT").expect("REPORT is a valid method");
    let mut request = client
        .request(method, &url)
        .header("Depth", "1")
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(query);

    request = apply_auth(request, auth, source_id).await?;

    let response = request
        .send()
        .await
        .map_err(|e| SyncError::Other(format!("sync-collection request failed: {e}")))?;

    let status = response.status();

    // 403 or 409 means the token is stale/invalid — fall back to full sync
    if status == reqwest::StatusCode::FORBIDDEN
        || status == reqwest::StatusCode::CONFLICT
    {
        tracing::info!("sync-token invalid for '{calendar_href}', falling back to full sync");
        return Ok(None);
    }

    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(SyncError::AuthExpired(source_id.to_string()));
    }

    if !status.is_success() && status.as_u16() != 207 {
        return Err(SyncError::Other(format!(
            "sync-collection returned status {status}"
        )));
    }

    let body = response
        .text()
        .await
        .map_err(|e| SyncError::Other(format!("Failed to read sync-collection response: {e}")))?;

    let new_sync_token = xml_extract_text(&body, "sync-token");
    let mut events = Vec::new();
    let mut todos = Vec::new();
    let mut removed_hrefs = Vec::new();

    for block in xml_response_blocks(&body) {
        let href = xml_extract_text(&block, "href");
        let status_text = xml_extract_text(&block, "status");

        // A 404 status in the response means the resource was deleted
        let is_deleted = status_text
            .as_deref()
            .map(|s| s.contains("404"))
            .unwrap_or(false);

        if is_deleted {
            if let Some(h) = href {
                removed_hrefs.push(h);
            }
            continue;
        }

        let data = xml_extract_text(&block, "calendar-data");
        let etag = xml_extract_text(&block, "getetag")
            .map(|e| e.trim_matches('"').to_string());

        if let Some(calendar_data) = data {
            let mut parsed_events = parse_ics_events(&calendar_data, source_id, color);
            for ev in &mut parsed_events {
                ev.etag = etag.clone();
                ev.href = href.clone();
            }
            events.extend(parsed_events);

            let mut parsed_todos = parse_ics_todos(&calendar_data, source_id, color);
            for todo in &mut parsed_todos {
                todo.etag = etag.clone();
                todo.href = href.clone();
            }
            todos.extend(parsed_todos);
        }
    }

    Ok(Some(SyncCollectionResult {
        events,
        todos,
        removed_hrefs,
        new_sync_token,
    }))
}

async fn apply_auth(
    request: reqwest::RequestBuilder,
    auth: &AuthMethod,
    source_id: &str,
) -> Result<reqwest::RequestBuilder, SyncError> {
    Ok(match auth {
        AuthMethod::None => request,
        AuthMethod::Basic { username } => {
            let password = secrets::load_secret(source_id, SecretKind::Password)
                .await
                .map_err(|e| SyncError::Other(format!("Failed to load password from keyring: {e}")))?
                .unwrap_or_default();
            request.basic_auth(username, Some(password))
        }
        AuthMethod::Bearer => {
            let token = secrets::load_secret(source_id, SecretKind::BearerToken)
                .await
                .map_err(|e| SyncError::Other(format!("Failed to load bearer token from keyring: {e}")))?
                .ok_or_else(|| SyncError::Other("Bearer token not found in keyring".into()))?;
            request.bearer_auth(token)
        }
        AuthMethod::Oidc { has_token: true, .. } => {
            let token = secrets::load_secret(source_id, SecretKind::OidcAccessToken)
                .await
                .map_err(|e| SyncError::Other(format!("Failed to load OIDC token from keyring: {e}")))?
                .ok_or_else(|| SyncError::AuthExpired(source_id.to_string()))?;
            request.bearer_auth(token)
        }
        AuthMethod::Oidc {
            has_token: false, ..
        } => {
            return Err(SyncError::AuthExpired(source_id.to_string()));
        }
    })
}

/// Apply a bearer token directly without keyring lookup (used after refresh).
fn apply_auth_direct(
    request: reqwest::RequestBuilder,
    access_token: &str,
) -> reqwest::RequestBuilder {
    request.bearer_auth(access_token)
}

/// Attempt to refresh an OIDC access token.
///
/// Returns `Some(new_access_token)` on success, `None` if this auth method
/// doesn't support refresh (not OIDC, or no refresh token stored).
/// Returns `Err(SyncError::AuthExpired)` if refresh was attempted but failed.
async fn try_oidc_refresh(
    auth: &AuthMethod,
    source_id: &str,
) -> Result<Option<String>, SyncError> {
    let AuthMethod::Oidc {
        issuer_url,
        client_id,
        has_client_secret,
        ..
    } = auth
    else {
        return Ok(None);
    };

    // Load the stored refresh token
    let refresh_token = match secrets::load_secret(source_id, SecretKind::OidcRefreshToken).await {
        Ok(Some(rt)) => rt,
        _ => return Ok(None), // No refresh token → can't refresh
    };

    // Load client secret if needed
    let client_secret = if *has_client_secret {
        secrets::load_secret(source_id, SecretKind::OidcClientSecret)
            .await
            .ok()
            .flatten()
    } else {
        None
    };

    match auth::oidc_refresh(issuer_url, client_id, client_secret.as_deref(), &refresh_token).await
    {
        Ok(tokens) => {
            // Persist the new tokens
            if let Err(e) =
                secrets::store_secret(source_id, SecretKind::OidcAccessToken, &tokens.access_token)
                    .await
            {
                tracing::warn!("Failed to store refreshed access token: {e}");
            }
            if let Some(ref rt) = tokens.refresh_token {
                if let Err(e) =
                    secrets::store_secret(source_id, SecretKind::OidcRefreshToken, rt).await
                {
                    tracing::warn!("Failed to store refreshed refresh token: {e}");
                }
            }
            tracing::info!("Successfully refreshed OIDC token for source {source_id}");
            Ok(Some(tokens.access_token))
        }
        Err(e) => {
            tracing::warn!("OIDC token refresh failed for source {source_id}: {e}");
            // Refresh failed — the user must re-authenticate
            Err(SyncError::AuthExpired(source_id.to_string()))
        }
    }
}

/// A parsed CalDAV `<d:response>` entry with href, etag, and calendar data.
struct ResponseEntry {
    href: Option<String>,
    etag: Option<String>,
    calendar_data: String,
}

/// Extract all response entries (href, etag, calendar-data) from a CalDAV
/// multistatus XML response using quick-xml.
fn extract_response_entries(xml: &str) -> Vec<ResponseEntry> {
    let mut entries = Vec::new();

    for block in xml_response_blocks(xml) {
        let href = xml_extract_text(&block, "href");
        let etag = xml_extract_text(&block, "getetag")
            .map(|e| e.trim_matches('"').to_string());
        let data = xml_extract_text(&block, "calendar-data");

        if let Some(calendar_data) = data {
            entries.push(ResponseEntry {
                href,
                etag,
                calendar_data,
            });
        }
    }

    entries
}

// ── quick-xml helpers ──────────────────────────────────────────────

/// Split a WebDAV multistatus XML response into per-`<d:response>` blocks.
///
/// Returns the raw XML string for each response block, including tags.
fn xml_response_blocks(xml: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut depth: u32 = 0;
    let mut in_response = false;
    let mut block_buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Start(ref e)) => {
                let name_bytes = e.name().as_ref().to_vec();
                let local = local_name(&name_bytes);
                if local == b"response" && !in_response {
                    in_response = true;
                    depth = 1;
                    block_buf.clear();
                    block_buf.extend_from_slice(b"<response>");
                } else if in_response {
                    depth += 1;
                    block_buf.extend_from_slice(b"<");
                    block_buf.extend_from_slice(e.name().as_ref());
                    for attr in e.attributes().flatten() {
                        block_buf.extend_from_slice(b" ");
                        block_buf.extend_from_slice(attr.key.as_ref());
                        block_buf.extend_from_slice(b"=\"");
                        block_buf.extend_from_slice(&attr.value);
                        block_buf.extend_from_slice(b"\"");
                    }
                    block_buf.extend_from_slice(b">");
                }
            }
            Ok(XmlEvent::End(ref e)) => {
                let name_bytes = e.name().as_ref().to_vec();
                let local = local_name(&name_bytes);
                if in_response {
                    if local == b"response" && depth == 1 {
                        block_buf.extend_from_slice(b"</response>");
                        if let Ok(s) = String::from_utf8(block_buf.clone()) {
                            blocks.push(s);
                        }
                        in_response = false;
                    } else {
                        block_buf.extend_from_slice(b"</");
                        block_buf.extend_from_slice(e.name().as_ref());
                        block_buf.extend_from_slice(b">");
                        depth -= 1;
                    }
                }
            }
            Ok(XmlEvent::Empty(ref e)) => {
                if in_response {
                    block_buf.extend_from_slice(b"<");
                    block_buf.extend_from_slice(e.name().as_ref());
                    for attr in e.attributes().flatten() {
                        block_buf.extend_from_slice(b" ");
                        block_buf.extend_from_slice(attr.key.as_ref());
                        block_buf.extend_from_slice(b"=\"");
                        block_buf.extend_from_slice(&attr.value);
                        block_buf.extend_from_slice(b"\"");
                    }
                    block_buf.extend_from_slice(b"/>");
                }
            }
            Ok(XmlEvent::Text(ref e)) => {
                if in_response {
                    if let Ok(t) = e.unescape() {
                        // Re-escape so inner parsing works consistently
                        block_buf.extend_from_slice(t.as_bytes());
                    }
                }
            }
            Ok(XmlEvent::CData(ref e)) => {
                if in_response {
                    block_buf.extend_from_slice(e.as_ref());
                }
            }
            Ok(XmlEvent::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    blocks
}

/// Extract the text content of a simple element by its local name from an XML fragment.
///
/// Namespace-aware: matches `<d:href>`, `<D:href>`, `<href>` etc.
fn xml_extract_text(xml: &str, local_name_target: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let target = local_name_target.as_bytes();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Start(ref e)) => {
                if local_name(e.name().as_ref()) == target {
                    let mut text_buf = Vec::new();
                    match reader.read_event_into(&mut text_buf) {
                        Ok(XmlEvent::Text(t)) => {
                            if let Ok(s) = t.unescape() {
                                let trimmed = s.trim();
                                if !trimmed.is_empty() {
                                    return Some(trimmed.to_string());
                                }
                            }
                        }
                        Ok(XmlEvent::CData(t)) => {
                            if let Ok(s) = std::str::from_utf8(t.as_ref()) {
                                let trimmed = s.trim();
                                if !trimmed.is_empty() {
                                    return Some(trimmed.to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(XmlEvent::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    None
}

/// Extract the inner XML/text content of a named element (namespace-agnostic).
///
/// Unlike `xml_extract_text`, this captures all inner content including nested tags,
/// which is needed for elements like `<current-user-principal>` that contain an `<href>`.
fn xml_extract_inner(xml: &str, local_name_target: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let target = local_name_target.as_bytes();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Start(ref e)) => {
                if local_name(e.name().as_ref()) == target {
                    let mut inner = Vec::new();
                    let mut depth: u32 = 1;
                    let mut inner_buf = Vec::new();
                    loop {
                        match reader.read_event_into(&mut inner_buf) {
                            Ok(XmlEvent::Start(ref ie)) => {
                                depth += 1;
                                inner.extend_from_slice(b"<");
                                inner.extend_from_slice(ie.name().as_ref());
                                inner.extend_from_slice(b">");
                            }
                            Ok(XmlEvent::End(ref ie)) => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                                inner.extend_from_slice(b"</");
                                inner.extend_from_slice(ie.name().as_ref());
                                inner.extend_from_slice(b">");
                            }
                            Ok(XmlEvent::Empty(ref ie)) => {
                                inner.extend_from_slice(b"<");
                                inner.extend_from_slice(ie.name().as_ref());
                                inner.extend_from_slice(b"/>");
                            }
                            Ok(XmlEvent::Text(ref t)) => {
                                if let Ok(s) = t.unescape() {
                                    inner.extend_from_slice(s.as_bytes());
                                }
                            }
                            Ok(XmlEvent::Eof) => break,
                            Err(_) => break,
                            _ => {}
                        }
                        inner_buf.clear();
                    }
                    if let Ok(s) = String::from_utf8(inner) {
                        let trimmed = s.trim().to_string();
                        if !trimmed.is_empty() {
                            return Some(trimmed);
                        }
                    }
                }
            }
            Ok(XmlEvent::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    None
}

/// Strip namespace prefix from a qualified XML name, returning the local part.
fn local_name(qname: &[u8]) -> &[u8] {
    match qname.iter().position(|&b| b == b':') {
        Some(pos) => &qname[pos + 1..],
        None => qname,
    }
}
