//! The aleno widget settings (API key, restaurant id, API domain), read from the booking widget
//! embedded on a restaurant's website and cached per restaurant in
//! `$XDG_CACHE_HOME/bytesctl/widget-<restaurant>.json`.
//!
//! The page embeds `https://mytools.aleno.me/reservations/v2.0/reservations.html?k=<b64>`,
//! where `<b64>` decodes to `{"k": <api key>, "r": <restaurant id>, "s": <api domain>}`.

use crate::config::Restaurant;
use crate::menu::cache_dir;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Widget {
    pub key: String,
    pub restaurant_id: String,
    /// e.g. `https://mytools.aleno.me/api`
    pub api_base: String,
    /// The page these settings were read from.
    pub page: String,
    pub fetched_at: DateTime<Utc>,
}

/// The `?k=` payload of the widget URL.
#[derive(Deserialize)]
struct Payload {
    k: String,
    r: String,
    s: String,
}

/// Cached settings if they still match the restaurant, else fetched from its page (and cached).
pub fn load(http: &reqwest::blocking::Client, restaurant: &Restaurant) -> Result<Widget> {
    let cached = load_cache(restaurant).filter(|w| {
        w.page == restaurant.url
            && restaurant
                .aleno_id
                .as_ref()
                .is_none_or(|id| *id == w.restaurant_id)
    });
    match cached {
        Some(w) => Ok(w),
        None => refresh(http, restaurant),
    }
}

/// Fetch the settings from the restaurant's page and update the cache.
pub fn refresh(http: &reqwest::blocking::Client, restaurant: &Restaurant) -> Result<Widget> {
    let html = http
        .get(&restaurant.url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .with_context(|| format!("cannot load {} to find the booking widget", restaurant.url))?;
    let w = from_html(&html, restaurant, Utc::now())?;
    store_cache(restaurant, &w);
    Ok(w)
}

/// Extract the widget settings from a page. Several different widgets are an error unless the
/// restaurant names one, so that a page for several locations never books the wrong one.
pub fn from_html(html: &str, restaurant: &Restaurant, now: DateTime<Utc>) -> Result<Widget> {
    const MARKER: &str = "reservations.html?k=";
    let url = &restaurant.url;
    let mut found: Vec<Payload> = Vec::new();
    for (at, _) in html.match_indices(MARKER) {
        let p = decode(&html[at + MARKER.len()..])?;
        if !found.iter().any(|f| f.r == p.r) {
            found.push(p);
        }
    }
    let p = match (found.len(), &restaurant.aleno_id) {
        (0, _) => bail!("no aleno booking widget found on {url}"),
        (_, Some(id)) => found
            .into_iter()
            .find(|p| p.r == *id)
            .ok_or_else(|| anyhow!("no aleno widget for restaurant_id {id} on {url}"))?,
        (1, None) => found.remove(0),
        (_, None) => bail!(
            "{url} embeds several aleno widgets ({}); pick one with --restaurant-id when adding \
             it, or set restaurant_id in [restaurants.{}]",
            found
                .iter()
                .map(|p| p.r.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            restaurant.slug
        ),
    };
    Ok(Widget {
        key: p.k,
        restaurant_id: p.r,
        api_base: format!("{}/api", p.s.trim_end_matches('/')),
        page: url.clone(),
        fetched_at: now,
    })
}

/// Decode the base64 `?k=` value at the start of `text`.
fn decode(text: &str) -> Result<Payload> {
    let raw: String = text
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
    serde_json::from_slice(&bytes).context("unexpected aleno widget settings format")
}

fn cache_path(restaurant: &Restaurant) -> Option<PathBuf> {
    Some(cache_dir()?.join(format!("widget-{}.json", restaurant.slug)))
}

fn load_cache(restaurant: &Restaurant) -> Option<Widget> {
    serde_json::from_slice(&std::fs::read(cache_path(restaurant)?).ok()?).ok()
}

/// Drop the cached settings of a restaurant that was removed (best effort).
pub fn forget(slug: &str) {
    if let Some(dir) = cache_dir() {
        let _ = std::fs::remove_file(dir.join(format!("widget-{slug}.json")));
    }
}

/// Best effort: failing to write the cache never fails the command.
fn store_cache(restaurant: &Restaurant, w: &Widget) {
    let Some(path) = cache_path(restaurant) else {
        return;
    };
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

    fn restaurant(id: Option<&str>) -> Restaurant {
        Restaurant {
            slug: "test".into(),
            name: "Test".into(),
            url: "https://example.com/".into(),
            phone: None,
            aleno_id: id.map(str::to_string),
        }
    }

    fn embed(json: &str) -> String {
        let b64 = base64::engine::general_purpose::STANDARD.encode(json);
        format!(r#"<a href="https://x/reservations.html?k={b64}&amp;locale=de">"#)
    }

    #[test]
    fn parses_widget_url() {
        // {"k":"abc123","r":"REST1","s":"https://mytools.aleno.me/"}
        let html = r#"<iframe src="https://mytools.aleno.me/reservations/v2.0/reservations.html?k=eyJrIjoiYWJjMTIzIiwiciI6IlJFU1QxIiwicyI6Imh0dHBzOi8vbXl0b29scy5hbGVuby5tZS8ifQ%3D%3D&amp;x=1">"#;
        let w = from_html(html, &restaurant(None), Utc::now()).unwrap();
        assert_eq!(w.key, "abc123");
        assert_eq!(w.restaurant_id, "REST1");
        assert_eq!(w.api_base, "https://mytools.aleno.me/api");
        assert_eq!(w.page, "https://example.com/");
    }

    #[test]
    fn missing_widget() {
        assert!(from_html("<html></html>", &restaurant(None), Utc::now()).is_err());
    }

    #[test]
    fn several_widgets_need_an_explicit_id() {
        let a = embed(r#"{"k":"key","r":"A","s":"https://a/"}"#);
        let b = embed(r#"{"k":"key","r":"B","s":"https://a/"}"#);
        let now = Utc::now();

        // The same widget linked twice is fine.
        let same = format!("{a}{a}");
        assert_eq!(
            from_html(&same, &restaurant(None), now)
                .unwrap()
                .restaurant_id,
            "A"
        );

        let both = format!("{a}{b}");
        let err = from_html(&both, &restaurant(None), now).unwrap_err();
        assert!(err.to_string().contains("A, B"), "{err}");
        assert_eq!(
            from_html(&both, &restaurant(Some("B")), now)
                .unwrap()
                .restaurant_id,
            "B"
        );
        assert!(from_html(&both, &restaurant(Some("C")), now).is_err());
    }
}
