//! The aleno widget settings (API key, restaurant id, API domain), read from the booking widget
//! embedded on bytes-chur.ch and cached in `$XDG_CACHE_HOME/bytesctl/widget.json`.
//!
//! The homepage embeds `https://mytools.aleno.me/reservations/v2.0/reservations.html?k=<b64>`,
//! where `<b64>` decodes to `{"k": <api key>, "r": <restaurant id>, "s": <api domain>}`.

use crate::menu::{HOMEPAGE, cache_dir};
use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const CACHE_FILE: &str = "widget.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Widget {
    pub key: String,
    pub restaurant_id: String,
    /// e.g. `https://mytools.aleno.me/api`
    pub api_base: String,
    pub fetched_at: DateTime<Utc>,
}

/// The `?k=` payload of the widget URL.
#[derive(Deserialize)]
struct Payload {
    k: String,
    r: String,
    s: String,
}

/// Cached settings if present, else fetched from the homepage (and cached).
pub fn load(http: &reqwest::blocking::Client) -> Result<Widget> {
    if let Some(w) = load_cache() {
        return Ok(w);
    }
    refresh(http)
}

/// Fetch the settings from the homepage and update the cache.
pub fn refresh(http: &reqwest::blocking::Client) -> Result<Widget> {
    let html = http
        .get(HOMEPAGE)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .context("cannot load bytes-chur.ch to find the aleno booking widget")?;
    let w = from_html(&html, Utc::now())?;
    store_cache(&w);
    Ok(w)
}

/// Extract the widget settings from the homepage HTML.
pub fn from_html(html: &str, now: DateTime<Utc>) -> Result<Widget> {
    let start = html
        .find("reservations.html?k=")
        .map(|i| i + "reservations.html?k=".len())
        .ok_or_else(|| anyhow!("no aleno booking widget found on {HOMEPAGE}"))?;
    let raw: String = html[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_' | '%'))
        .collect();
    let b64 = raw.replace("%3D", "=").replace("%3d", "=");
    let engine = |alphabet| {
        GeneralPurpose::new(
            alphabet,
            GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
        )
    };
    let bytes = engine(&base64::alphabet::STANDARD)
        .decode(&b64)
        .or_else(|_| engine(&base64::alphabet::URL_SAFE).decode(&b64))
        .context("cannot decode the aleno widget settings")?;
    let p: Payload =
        serde_json::from_slice(&bytes).context("unexpected aleno widget settings format")?;
    Ok(Widget {
        key: p.k,
        restaurant_id: p.r,
        api_base: format!("{}/api", p.s.trim_end_matches('/')),
        fetched_at: now,
    })
}

fn cache_path() -> Option<PathBuf> {
    Some(cache_dir()?.join(CACHE_FILE))
}

fn load_cache() -> Option<Widget> {
    serde_json::from_slice(&std::fs::read(cache_path()?).ok()?).ok()
}

/// Best effort: failing to write the cache never fails the command.
fn store_cache(w: &Widget) {
    let Some(path) = cache_path() else { return };
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(w)?)?;
        std::fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        eprintln!("warning: cannot write {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_widget_url() {
        // {"k":"abc123","r":"REST1","s":"https://mytools.aleno.me/"}
        let html = r#"<iframe src="https://mytools.aleno.me/reservations/v2.0/reservations.html?k=eyJrIjoiYWJjMTIzIiwiciI6IlJFU1QxIiwicyI6Imh0dHBzOi8vbXl0b29scy5hbGVuby5tZS8ifQ%3D%3D&amp;x=1">"#;
        let w = from_html(html, Utc::now()).unwrap();
        assert_eq!(w.key, "abc123");
        assert_eq!(w.restaurant_id, "REST1");
        assert_eq!(w.api_base, "https://mytools.aleno.me/api");
    }

    #[test]
    fn missing_widget() {
        assert!(from_html("<html></html>", Utc::now()).is_err());
    }
}
