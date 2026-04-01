// SPDX-License-Identifier: GPL-3.0-only

//! Shared CalDAV discovery helpers: PROPFIND constants, XML parsing, HTTP client.

use super::CalDavCalendar;
use quick_xml::events::Event as XmlEvent;
use quick_xml::Reader;
use std::borrow::Cow;
use std::sync::LazyLock;

// ── Error type ─────────────────────────────────────────────────────

#[derive(Debug)]
pub enum DiscoveryError {
    Http(String),
    AuthExpired(String),
    Other(String),
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(s) => write!(f, "HTTP error: {s}"),
            Self::AuthExpired(s) => write!(f, "Authentication expired for source {s}"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

// ── HTTP client ────────────────────────────────────────────────────

static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("cosmic-calendar/1.0")
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
pub fn get_client(
    ca_cert_path: Option<&str>,
) -> Result<Cow<'static, reqwest::Client>, DiscoveryError> {
    match ca_cert_path {
        None => Ok(Cow::Borrowed(&*HTTP_CLIENT)),
        Some(path) => {
            let pem = std::fs::read(path).map_err(|e| {
                DiscoveryError::Other(format!("Failed to read CA cert {path}: {e}"))
            })?;
            let cert = reqwest::tls::Certificate::from_pem(&pem)
                .map_err(|e| DiscoveryError::Other(format!("Invalid CA cert PEM: {e}")))?;
            let client = reqwest::Client::builder()
                .user_agent("cosmic-calendar/1.0")
                .redirect(reqwest::redirect::Policy::limited(5))
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .add_root_certificate(cert)
                .build()
                .map_err(|e| {
                    DiscoveryError::Other(format!("Failed to build client with CA: {e}"))
                })?;
            Ok(Cow::Owned(client))
        }
    }
}

// ── PROPFIND XML bodies ────────────────────────────────────────────

/// PROPFIND body to discover the current-user-principal.
pub const PROPFIND_PRINCIPAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:current-user-principal/>
  </d:prop>
</d:propfind>"#;

/// PROPFIND body to discover the calendar-home-set.
pub const PROPFIND_HOME_SET: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <c:calendar-home-set/>
  </d:prop>
</d:propfind>"#;

/// PROPFIND body to list calendars and their properties.
pub const PROPFIND_CALENDARS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
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

// ── Inline credentials (no keyring) ────────────────────────────────

/// Credentials for discovery without keyring lookup.
pub enum InlineCredentials {
    None,
    Basic { username: String, password: String },
    Bearer { token: String },
}

/// Perform a PROPFIND request with inline credentials (no keyring).
pub async fn propfind_inline(
    client: &reqwest::Client,
    url: &str,
    depth: &str,
    body: &str,
    credentials: &InlineCredentials,
) -> Result<String, DiscoveryError> {
    let method = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND is a valid method");
    let mut request = client
        .request(method, url)
        .header("Depth", depth)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body.to_string());

    request = match credentials {
        InlineCredentials::None => request,
        InlineCredentials::Basic { username, password } => {
            request.basic_auth(username, Some(password))
        }
        InlineCredentials::Bearer { token } => request.bearer_auth(token),
    };

    let response = request
        .send()
        .await
        .map_err(|e| DiscoveryError::Other(format!("PROPFIND request to {url} failed: {e}")))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        let body = response.text().await.unwrap_or_default();
        tracing::error!("CalDAV 401 Unauthorized (inline) from {url}: {body}");
        return Err(DiscoveryError::AuthExpired("form".to_string()));
    }

    if !response.status().is_success() {
        return Err(DiscoveryError::Other(format!(
            "PROPFIND to {url} returned status {}",
            response.status()
        )));
    }

    response
        .text()
        .await
        .map_err(|e| DiscoveryError::Other(format!("Failed to read PROPFIND response: {e}")))
}

/// Discover all calendars on a CalDAV server using inline credentials.
///
/// Unlike keyring-based discovery, this does not read secrets from the system keyring.
pub async fn discover_calendars_inline(
    server_url: &str,
    credentials: &InlineCredentials,
    ca_cert_path: Option<&str>,
) -> Result<Vec<CalDavCalendar>, DiscoveryError> {
    let client = get_client(ca_cert_path)?;
    let base = server_url.trim_end_matches('/');

    let principal_body =
        propfind_inline(&client, base, "0", PROPFIND_PRINCIPAL, credentials).await?;
    let principal_href = xml_extract_inner(&principal_body, "current-user-principal")
        .and_then(|block| xml_extract_text(&block, "href"))
        .unwrap_or_default();
    let principal_url = resolve_url(base, &principal_href);

    let home_body =
        propfind_inline(&client, &principal_url, "0", PROPFIND_HOME_SET, credentials).await?;
    let home_href = xml_extract_inner(&home_body, "calendar-home-set")
        .and_then(|block| xml_extract_text(&block, "href"))
        .unwrap_or_default();
    let home_url = resolve_url(base, &home_href);

    let cal_body =
        propfind_inline(&client, &home_url, "1", PROPFIND_CALENDARS, credentials).await?;
    let calendars = parse_calendar_list(&cal_body, base);

    Ok(calendars)
}

// ── URL resolution ─────────────────────────────────────────────────

/// Resolve a potentially-relative href against the server base URL.
pub fn resolve_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        href.to_string()
    } else if href.starts_with('/') {
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

// ── Calendar list parsing ──────────────────────────────────────────

/// Parse the PROPFIND Depth:1 response to extract calendar collections.
pub fn parse_calendar_list(xml: &str, base: &str) -> Vec<CalDavCalendar> {
    let mut calendars = Vec::new();

    for block in xml_response_blocks(xml) {
        let is_calendar = block.contains("calendar")
            && block.contains("resourcetype")
            && block.contains("collection");

        if is_calendar {
            let href = xml_extract_text(&block, "href").unwrap_or_default();
            let display_name = xml_extract_text(&block, "displayname").unwrap_or_else(|| {
                href.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("Calendar")
                    .to_string()
            });
            let color = xml_extract_text(&block, "calendar-color").unwrap_or_default();
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
                    enabled: false,
                    ctag,
                    sync_token: None,
                });
            }
        }
    }

    calendars
}

// ── quick-xml helpers ──────────────────────────────────────────────

/// Split a WebDAV multistatus XML response into per-`<d:response>` blocks.
pub fn xml_response_blocks(xml: &str) -> Vec<String> {
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
pub fn xml_extract_text(xml: &str, local_name_target: &str) -> Option<String> {
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
pub fn xml_extract_inner(xml: &str, local_name_target: &str) -> Option<String> {
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
pub fn local_name(qname: &[u8]) -> &[u8] {
    match qname.iter().position(|&b| b == b':') {
        Some(pos) => &qname[pos + 1..],
        None => qname,
    }
}
