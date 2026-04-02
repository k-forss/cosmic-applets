// SPDX-License-Identifier: GPL-3.0-only

//! Secure secret storage via the freedesktop Secret Service API (D-Bus).
//!
//! Secrets (passwords, bearer tokens, OIDC access/refresh tokens) are stored in
//! the user's default keyring rather than in the plaintext cosmic-config file.
//! Each secret is keyed by `(application, source_id, secret_kind)`.

use secret_service::{EncryptionType, SecretService};
use std::collections::HashMap;
use zeroize::Zeroizing;

const APP_LABEL: &str = super::CALENDAR_CONFIG_ID;

/// The kinds of secrets we store per calendar source.
#[derive(Debug, Clone, Copy)]
pub enum SecretKind {
    Password,
    BearerToken,
    OidcAccessToken,
    OidcRefreshToken,
    OidcClientSecret,
    CacheEncryptionKey,
}

impl SecretKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::BearerToken => "bearer_token",
            Self::OidcAccessToken => "oidc_access_token",
            Self::OidcRefreshToken => "oidc_refresh_token",
            Self::OidcClientSecret => "oidc_client_secret",
            Self::CacheEncryptionKey => "cache_encryption_key",
        }
    }
}

fn attributes(source_id: &str, kind: SecretKind) -> HashMap<&str, &str> {
    HashMap::from([
        ("application", APP_LABEL),
        ("source_id", source_id),
        ("secret_kind", kind.as_str()),
    ])
}

/// Store a secret in the default keyring collection.
pub async fn store_secret(source_id: &str, kind: SecretKind, secret: &str) -> Result<(), String> {
    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| format!("Failed to connect to Secret Service: {e}"))?;

    let collection = ss
        .get_default_collection()
        .await
        .map_err(|e| format!("Failed to get default keyring: {e}"))?;

    // Unlock if locked
    if collection.is_locked().await.unwrap_or(true) {
        collection
            .unlock()
            .await
            .map_err(|e| format!("Failed to unlock keyring: {e}"))?;
    }

    let label = format!("COSMIC Calendar – {source_id} – {}", kind.as_str());
    let attrs = attributes(source_id, kind);

    collection
        .create_item(&label, attrs, secret.as_bytes(), true, "text/plain")
        .await
        .map_err(|e| format!("Failed to store secret: {e}"))?;

    Ok(())
}

/// Load a secret from the default keyring collection.
///
/// Returns `None` if no matching secret is found.
pub async fn load_secret(
    source_id: &str,
    kind: SecretKind,
) -> Result<Option<Zeroizing<String>>, String> {
    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| format!("Failed to connect to Secret Service: {e}"))?;

    let attrs = attributes(source_id, kind);
    let results = ss
        .search_items(attrs)
        .await
        .map_err(|e| format!("Failed to search keyring: {e}"))?;

    let item = match results.unlocked.first() {
        Some(item) => item,
        None => match results.locked.first() {
            Some(item) => {
                item.unlock()
                    .await
                    .map_err(|e| format!("Failed to unlock item: {e}"))?;
                item
            }
            None => return Ok(None),
        },
    };

    let secret_bytes = item
        .get_secret()
        .await
        .map_err(|e| format!("Failed to read secret: {e}"))?;

    String::from_utf8(secret_bytes)
        .map(|s| Some(Zeroizing::new(s)))
        .map_err(|e| format!("Secret is not valid UTF-8: {e}"))
}

/// Delete all secrets for a given source from the keyring.
pub async fn delete_secrets(source_id: &str) -> Result<(), String> {
    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| format!("Failed to connect to Secret Service: {e}"))?;

    let kinds = [
        SecretKind::Password,
        SecretKind::BearerToken,
        SecretKind::OidcAccessToken,
        SecretKind::OidcRefreshToken,
        SecretKind::OidcClientSecret,
        SecretKind::CacheEncryptionKey,
    ];

    for kind in kinds {
        let attrs = attributes(source_id, kind);
        if let Ok(results) = ss.search_items(attrs).await {
            for item in results.unlocked.iter().chain(results.locked.iter()) {
                let _ = item.delete().await;
            }
        }
    }

    Ok(())
}

const CACHE_KEY_SOURCE_ID: &str = "__cache_encryption__";

/// Store the Auto-mode encryption key in the keyring.
pub async fn store_encryption_key(key: &[u8]) -> Result<(), String> {
    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| format!("Failed to connect to Secret Service: {e}"))?;

    let collection = ss
        .get_default_collection()
        .await
        .map_err(|e| format!("Failed to get default keyring: {e}"))?;

    if collection.is_locked().await.unwrap_or(true) {
        collection
            .unlock()
            .await
            .map_err(|e| format!("Failed to unlock keyring: {e}"))?;
    }

    let label = "COSMIC Calendar – cache encryption key";
    let attrs = attributes(CACHE_KEY_SOURCE_ID, SecretKind::CacheEncryptionKey);

    collection
        .create_item(label, attrs, key, true, "application/octet-stream")
        .await
        .map_err(|e| format!("Failed to store encryption key: {e}"))?;

    Ok(())
}

/// Load the Auto-mode encryption key from the keyring.
///
/// Returns `None` if no key is stored.
pub async fn load_encryption_key() -> Result<Option<Vec<u8>>, String> {
    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| format!("Failed to connect to Secret Service: {e}"))?;

    let attrs = attributes(CACHE_KEY_SOURCE_ID, SecretKind::CacheEncryptionKey);
    let results = ss
        .search_items(attrs)
        .await
        .map_err(|e| format!("Failed to search keyring: {e}"))?;

    let item = match results.unlocked.first() {
        Some(item) => item,
        None => match results.locked.first() {
            Some(item) => {
                item.unlock()
                    .await
                    .map_err(|e| format!("Failed to unlock item: {e}"))?;
                item
            }
            None => return Ok(None),
        },
    };

    let bytes = item
        .get_secret()
        .await
        .map_err(|e| format!("Failed to read encryption key: {e}"))?;

    Ok(Some(bytes))
}

/// Delete the Auto-mode encryption key from the keyring.
pub async fn delete_encryption_key() -> Result<(), String> {
    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| format!("Failed to connect to Secret Service: {e}"))?;

    let attrs = attributes(CACHE_KEY_SOURCE_ID, SecretKind::CacheEncryptionKey);
    if let Ok(results) = ss.search_items(attrs).await {
        for item in results.unlocked.iter().chain(results.locked.iter()) {
            let _ = item.delete().await;
        }
    }

    Ok(())
}
