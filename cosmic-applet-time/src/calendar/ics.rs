// SPDX-License-Identifier: GPL-3.0-only

use crate::calendar::config::AuthMethod;
use crate::calendar::event::{parse_ics_events, CalendarEvent};
use crate::calendar::secrets::{self, SecretKind};

/// Fetch and parse an ICS feed from a URL, optionally with authentication.
pub async fn fetch_ics_url(
    url: &str,
    source_id: &str,
    color: &str,
    auth: &AuthMethod,
    ca_cert_path: Option<&str>,
) -> Result<Vec<CalendarEvent>, String> {
    let mut builder = reqwest::Client::builder()
        .user_agent("cosmic-applet-time/1.0")
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10));

    if let Some(path) = ca_cert_path {
        let pem = std::fs::read(path)
            .map_err(|e| format!("Failed to read CA cert {path}: {e}"))?;
        let cert = reqwest::tls::Certificate::from_pem(&pem)
            .map_err(|e| format!("Invalid CA cert PEM: {e}"))?;
        builder = builder.add_root_certificate(cert);
    }

    let client = builder
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let mut request = client.get(url);
    request = apply_ics_auth(request, auth, source_id).await?;

    let response = request
        .send()
        .await
        .map_err(|e| format!("Failed to fetch ICS URL: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("ICS URL returned status {}", response.status()));
    }

    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read ICS response: {e}"))?;

    Ok(parse_ics_events(&body, source_id, color))
}

/// Apply authentication to an ICS request.
async fn apply_ics_auth(
    request: reqwest::RequestBuilder,
    auth: &AuthMethod,
    source_id: &str,
) -> Result<reqwest::RequestBuilder, String> {
    Ok(match auth {
        AuthMethod::None => request,
        AuthMethod::Basic { username } => {
            let password = secrets::load_secret(source_id, SecretKind::Password)
                .await
                .map_err(|e| format!("Failed to load password from keyring: {e}"))?
                .unwrap_or_default();
            request.basic_auth(username, Some(password.as_str()))
        }
        AuthMethod::Bearer => {
            let token = secrets::load_secret(source_id, SecretKind::BearerToken)
                .await
                .map_err(|e| format!("Failed to load bearer token: {e}"))?
                .ok_or_else(|| "Bearer token not found in keyring".to_string())?;
            request.bearer_auth(token.as_str())
        }
        AuthMethod::Oidc { has_token: true, .. } => {
            let token = secrets::load_secret(source_id, SecretKind::OidcAccessToken)
                .await
                .map_err(|e| format!("Failed to load OIDC token: {e}"))?
                .ok_or_else(|| "OIDC token not found".to_string())?;
            request.bearer_auth(token.as_str())
        }
        AuthMethod::Oidc { has_token: false, .. } => {
            return Err("OIDC authentication required".to_string());
        }
    })
}

/// Read and parse a local ICS file.
pub fn read_ics_file(
    path: &str,
    source_id: &str,
    color: &str,
) -> Result<Vec<CalendarEvent>, String> {
    let data =
        std::fs::read_to_string(path).map_err(|e| format!("Failed to read ICS file: {e}"))?;
    Ok(parse_ics_events(&data, source_id, color))
}
