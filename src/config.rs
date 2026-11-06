//! TOML config file with defaults for bookings, contact details and restaurants.
//!
//! Location: `--config <path>`, `$BYTES_CONFIG`, or `$XDG_CONFIG_HOME/bytesctl/config.toml`
//! (falling back to `~/.config/bytesctl/config.toml`).

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The built-in restaurant, used unless another one is selected.
pub const DEFAULT_RESTAURANT: &str = "bytes";

pub const TEMPLATE: &str = r#"# bytesctl configuration
# Command-line flags and BYTES_* environment variables override these values.

[defaults]
# Restaurant to use: "bytes" or a name from a [restaurants.<name>] section below
# restaurant = "bytes"
# Date: "today", "tomorrow" or YYYY-MM-DD
date = "today"
# Time in Swiss local time, HH:MM
time = "11:45"
people = 2
# Language for server messages: "de" or "en"
lang = "de"
# comment = "Fensterplatz bitte"
newsletter = false

[contact]
first_name = ""
last_name = ""
email = ""
# International format, e.g. +41791234567
phone = ""
# One of: male, female, other, prefer-not-say
# salutation = "male"

[menu]
# Also show seasonals, bowls and toppings (override with --no-all)
all = false
# Only show today's lunch instead of the whole week (override with --week)
today = false

# More restaurants that take reservations through aleno. `url` is the page that embeds the
# booking widget. Use them with --restaurant <name>. `bytesctl restaurants add` writes these
# sections for you.
# [restaurants.muster]
# name = "Restaurant Muster"
# url = "https://restaurant.example/"
# phone = "+41 44 123 45 67"
"#;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub contact: Contact,
    #[serde(default)]
    pub menu: MenuDefaults,
    #[serde(default)]
    pub restaurants: BTreeMap<String, RestaurantConfig>,
}

/// A `[restaurants.<name>]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestaurantConfig {
    pub name: Option<String>,
    /// Page that embeds the aleno booking widget.
    pub url: String,
    pub phone: Option<String>,
    /// aleno restaurant id; only needed if the page embeds several widgets.
    pub restaurant_id: Option<String>,
}

/// A restaurant that can be booked.
#[derive(Debug, Clone, PartialEq)]
pub struct Restaurant {
    /// The name used on the command line and in the bookings file.
    pub slug: String,
    pub name: String,
    pub url: String,
    pub phone: Option<String>,
    /// aleno restaurant id to pick, if the page embeds several widgets.
    pub aleno_id: Option<String>,
}

impl Restaurant {
    fn bytes() -> Self {
        Self {
            slug: DEFAULT_RESTAURANT.into(),
            name: "Restaurant bytes, Chur".into(),
            url: "https://bytes-chur.ch/".into(),
            phone: Some("+41 81 253 01 01".into()),
            aleno_id: None,
        }
    }

    /// How to reach the restaurant, to finish a sentence like "please …".
    pub fn contact(&self) -> String {
        self.phone.as_ref().map_or_else(
            || "contact the restaurant".into(),
            |p| format!("call the restaurant at {p}"),
        )
    }
}

/// Restaurant slugs are typed on the command line and become part of a cache file name.
pub fn check_slug(slug: &str) -> Result<()> {
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if !valid {
        bail!("invalid restaurant name '{slug}': use letters, digits, - and _ only");
    }
    Ok(())
}

impl Config {
    /// Look up a restaurant by its slug. A config section may override the built-in one.
    pub fn restaurant(&self, slug: &str) -> Result<Restaurant> {
        if let Some(r) = self.restaurants.get(slug) {
            check_slug(slug)?;
            return Ok(Restaurant {
                slug: slug.to_string(),
                name: non_empty(r.name.as_deref()).unwrap_or_else(|| slug.to_string()),
                url: r.url.clone(),
                phone: non_empty(r.phone.as_deref()),
                aleno_id: non_empty(r.restaurant_id.as_deref()),
            });
        }
        if slug == DEFAULT_RESTAURANT {
            return Ok(Restaurant::bytes());
        }
        bail!(
            "unknown restaurant '{slug}' (available: {}); add it as [restaurants.{slug}] in the config file",
            self.restaurant_slugs().join(", ")
        )
    }

    /// All selectable restaurants, sorted.
    pub fn restaurant_slugs(&self) -> Vec<String> {
        let mut slugs: Vec<String> = self.restaurants.keys().cloned().collect();
        if !self.restaurants.contains_key(DEFAULT_RESTAURANT) {
            slugs.push(DEFAULT_RESTAURANT.into());
        }
        slugs.sort();
        slugs
    }

    /// The restaurant used when none is given on the command line.
    pub fn default_restaurant(&self) -> String {
        non_empty(self.defaults.restaurant.as_deref()).unwrap_or_else(|| DEFAULT_RESTAURANT.into())
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MenuDefaults {
    pub all: Option<bool>,
    pub today: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub restaurant: Option<String>,
    pub date: Option<String>,
    pub time: Option<String>,
    pub people: Option<u32>,
    pub lang: Option<String>,
    pub comment: Option<String>,
    pub newsletter: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contact {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub salutation: Option<String>,
}

pub fn default_path() -> Result<PathBuf> {
    let base = if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        PathBuf::from(dir)
    } else {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .ok_or_else(|| anyhow!("cannot determine home directory; use --config"))?;
        PathBuf::from(home).join(".config")
    };
    Ok(base.join("bytesctl").join("config.toml"))
}

/// Load the config. A missing file is only an error if the path was given explicitly.
pub fn load(path: &Path, explicit: bool) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str::<Config>(&text)
            .with_context(|| format!("invalid config file {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read config file {}", path.display())),
    }
}

/// Add or replace a `[restaurants.<slug>]` section in config text, keeping everything else
/// (including comments) as it is.
pub fn with_restaurant(text: &str, slug: &str, r: &RestaurantConfig) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse().context("invalid config file")?;
    let restaurants = doc
        .entry("restaurants")
        .or_insert_with(|| {
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            toml_edit::Item::Table(t)
        })
        .as_table_mut()
        .ok_or_else(|| anyhow!("`restaurants` in the config file is not a table"))?;
    let mut section = toml_edit::Table::new();
    if let Some(name) = &r.name {
        section["name"] = toml_edit::value(name);
    }
    section["url"] = toml_edit::value(&r.url);
    if let Some(phone) = &r.phone {
        section["phone"] = toml_edit::value(phone);
    }
    if let Some(id) = &r.restaurant_id {
        section["restaurant_id"] = toml_edit::value(id);
    }
    restaurants.insert(slug, toml_edit::Item::Table(section));
    checked(&doc)
}

/// Remove a `[restaurants.<slug>]` section from config text.
pub fn without_restaurant(text: &str, slug: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse().context("invalid config file")?;
    doc.get_mut("restaurants")
        .and_then(toml_edit::Item::as_table_mut)
        .and_then(|t| t.remove(slug))
        .ok_or_else(|| anyhow!("'{slug}' is not in the config file"))?;
    checked(&doc)
}

/// The new config text, after making sure bytesctl can still read it.
fn checked(doc: &toml_edit::DocumentMut) -> Result<String> {
    let text = doc.to_string();
    toml::from_str::<Config>(&text).context("the change would make the config invalid")?;
    Ok(text)
}

/// Treat empty strings in the config as unset.
pub fn non_empty(v: Option<&str>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_is_valid() {
        toml::from_str::<Config>(TEMPLATE).unwrap();
    }

    #[test]
    fn adding_and_removing_keeps_the_rest_of_the_file() {
        let text = "# my comment\n[contact]\nemail = \"max@example.com\" # work\n";
        let r = RestaurantConfig {
            name: Some("Restaurant Muster".into()),
            url: "https://restaurant.example/".into(),
            phone: None,
            restaurant_id: None,
        };
        let added = with_restaurant(text, "muster", &r).unwrap();
        assert!(added.starts_with(text), "{added}");
        assert!(added.contains("[restaurants.muster]"), "{added}");
        assert!(!added.contains("[restaurants]\n"), "{added}");
        assert!(!added.contains("phone"), "{added}");
        let cfg: Config = toml::from_str(&added).unwrap();
        assert_eq!(cfg.restaurant("muster").unwrap().name, "Restaurant Muster");

        // Adding again replaces the section instead of duplicating it.
        let again = with_restaurant(&added, "muster", &r).unwrap();
        assert_eq!(again.matches("[restaurants.muster]").count(), 1);

        let removed = without_restaurant(&added, "muster").unwrap();
        assert!(removed.starts_with(text), "{removed}");
        assert!(!removed.contains("muster"), "{removed}");
        assert!(without_restaurant(&removed, "muster").is_err());
    }

    #[test]
    fn slugs() {
        assert!(check_slug("my-place_2").is_ok());
        assert!(check_slug("").is_err());
        assert!(check_slug("a/b").is_err());
        assert!(check_slug("a b").is_err());
    }

    #[test]
    fn restaurant_lookup() {
        let cfg: Config = toml::from_str(
            r#"
            [defaults]
            restaurant = "muster"
            [restaurants.muster]
            url = "https://restaurant.example/"
            [restaurants."bad/name"]
            url = "https://example.com/"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.default_restaurant(), "muster");
        let r = cfg.restaurant("muster").unwrap();
        assert_eq!(r.name, "muster");
        assert_eq!(r.contact(), "contact the restaurant");
        assert_eq!(
            cfg.restaurant("bytes").unwrap().url,
            "https://bytes-chur.ch/"
        );
        assert!(cfg.restaurant("bad/name").is_err());
        assert!(cfg.restaurant("nope").is_err());
        assert_eq!(cfg.restaurant_slugs(), ["bad/name", "bytes", "muster"]);
    }
}
