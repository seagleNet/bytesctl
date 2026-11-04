//! TOML config file with defaults for bookings and contact details.
//!
//! Location: `--config <path>`, `$BYTES_CONFIG`, or `$XDG_CONFIG_HOME/bytesctl/config.toml`
//! (falling back to `~/.config/bytesctl/config.toml`).

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const TEMPLATE: &str = r#"# bytesctl configuration
# Command-line flags and BYTES_* environment variables override these values.

[defaults]
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

/// Treat empty strings in the config as unset.
pub fn non_empty(v: Option<&str>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
