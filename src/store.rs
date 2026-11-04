//! Local record of reservations made with bytesctl, so they can be listed and cancelled
//! without having to keep the cancellation secret around.
//!
//! Stored as JSON in `$XDG_DATA_HOME/bytesctl/bookings.json` (default
//! `~/.local/share/bytesctl/`). This is data, not cache: the secret is the only way to cancel a
//! reservation through the API, so the file is written with owner-only permissions.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Booked,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reservation {
    /// aleno reservation id; may be missing if the server did not return one.
    pub id: Option<String>,
    /// Secret needed to cancel the reservation.
    pub secret: String,
    /// Start of the reservation.
    pub at: DateTime<Utc>,
    pub people: u32,
    pub shift: String,
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub provisional: bool,
    pub booked_at: DateTime<Utc>,
    pub status: Status,
    #[serde(default)]
    pub cancelled_at: Option<DateTime<Utc>>,
    /// Last state code reported by aleno (`popup/info`), if ever checked.
    #[serde(default)]
    pub state: Option<i64>,
    /// Whether aleno still allows cancelling online, as of the last check.
    #[serde(default)]
    pub cancelable: Option<bool>,
    /// aleno did not know this reservation at the last check.
    #[serde(default)]
    pub not_found: bool,
    #[serde(default)]
    pub checked_at: Option<DateTime<Utc>>,
}

/// Human label for an aleno reservation state code (codes from aleno's GraphQL
/// `ReservationState` docs, which state the numeric value used by the other APIs).
pub fn state_label(code: i64) -> String {
    match code {
        // aleno uses 1 for both RESERVATION_OPEN and RESERVATION_PROVISIONAL.
        1 => "open",
        2 => "accepted",
        3 | 7 => "cancelled",
        4 => "arrived",
        5 => "departed",
        6 => "no-show",
        8 => "walk-in",
        9 => "waiting list",
        10 => "seated",
        11 => "partly arrived",
        12 => "editing",
        13 => "paid",
        14 => "moved",
        15 => "card requested",
        16 => "card dismissed",
        17 => "reconfirmed",
        n => return format!("state {n}"),
    }
    .to_string()
}

/// CANCELED (3) or STORNO (7).
pub fn state_is_cancelled(code: i64) -> bool {
    matches!(code, 3 | 7)
}

impl Reservation {
    pub fn is_upcoming(&self, now: DateTime<Utc>) -> bool {
        self.status == Status::Booked && self.at >= now
    }

    /// Status shown in listings: aleno's state if it was checked, otherwise what bytesctl knows.
    pub fn status_label(&self, now: DateTime<Utc>) -> String {
        if self.status == Status::Cancelled {
            "cancelled".into()
        } else if self.not_found {
            "not found".into()
        } else if let Some(code) = self.state {
            state_label(code)
        } else if self.at < now {
            "past".into()
        } else if self.provisional {
            "provisional".into()
        } else {
            "booked".into()
        }
    }

    /// Record the result of a `popup/info` lookup (`None` = unknown to aleno).
    /// Returns human-readable descriptions of what changed.
    pub fn apply_remote(
        &mut self,
        info: Option<&crate::api::ReservationInfo>,
        now: DateTime<Utc>,
    ) -> Vec<String> {
        let mut changes = Vec::new();
        let before = self.status_label(now);
        self.checked_at = Some(now);
        match info {
            None => {
                self.not_found = true;
                self.cancelable = None;
            }
            Some(info) => {
                self.not_found = false;
                self.cancelable = info.cancelable;
                if let Some(code) = info.state {
                    self.state = Some(code);
                    if state_is_cancelled(code) && self.status == Status::Booked {
                        self.status = Status::Cancelled;
                        self.cancelled_at.get_or_insert(now);
                    }
                }
                if let Some(at) = info.selected_date.filter(|at| *at != self.at) {
                    changes.push(format!(
                        "time changed to {}",
                        at.with_timezone(&chrono_tz::Europe::Zurich)
                            .format("%a %d.%m.%Y %H:%M")
                    ));
                    self.at = at;
                }
                if let Some(p) = info.people_count.filter(|p| *p != self.people) {
                    changes.push(format!("guests changed from {} to {p}", self.people));
                    self.people = p;
                }
            }
        }
        let after = self.status_label(now);
        if after != before {
            changes.insert(0, format!("{before} → {after}"));
        }
        changes
    }

    /// Short id shown in listings and accepted by `cancel`.
    pub fn short_id(&self) -> String {
        let src = self.id.as_deref().unwrap_or(&self.secret);
        src.chars().take(6).collect()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub reservations: Vec<Reservation>,
}

pub fn default_path() -> Result<PathBuf> {
    let base = if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        PathBuf::from(dir)
    } else {
        let home =
            std::env::var_os("HOME").ok_or_else(|| anyhow!("cannot determine home directory"))?;
        PathBuf::from(home).join(".local").join("share")
    };
    Ok(base.join("bytesctl").join("bookings.json"))
}

impl Store {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("invalid bookings file {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => {
                Err(e).with_context(|| format!("cannot read bookings file {}", path.display()))
            }
        }
    }

    /// Write atomically (temp file + rename) with mode 0600, since the file holds secrets.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path
            .parent()
            .ok_or_else(|| anyhow!("invalid bookings path {}", path.display()))?;
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let tmp = path.with_extension("json.tmp");
        let data = serde_json::to_vec_pretty(self)?;
        write_private(&tmp, &data).with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(())
    }

    /// Reservations sorted by date, optionally only upcoming ones.
    pub fn list(&self, all: bool, now: DateTime<Utc>) -> Vec<&Reservation> {
        let mut v: Vec<_> = self
            .reservations
            .iter()
            .filter(|r| all || r.is_upcoming(now))
            .collect();
        v.sort_by_key(|r| r.at);
        v
    }

    /// Find a reservation by reservation id, secret, or a unique prefix of either.
    pub fn find(&self, key: &str) -> Result<usize> {
        if let Some(i) = self
            .reservations
            .iter()
            .position(|r| r.id.as_deref() == Some(key) || r.secret == key)
        {
            return Ok(i);
        }
        let matches: Vec<usize> = self
            .reservations
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                r.id.as_deref().is_some_and(|id| id.starts_with(key)) || r.secret.starts_with(key)
            })
            .map(|(i, _)| i)
            .collect();
        match matches.as_slice() {
            [i] => Ok(*i),
            [] => bail!("no saved reservation matches '{key}' (see `bytesctl bookings --all`)"),
            _ => bail!("'{key}' matches several reservations; use more characters"),
        }
    }
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(id: &str, secret: &str, at: &str, status: Status) -> Reservation {
        Reservation {
            id: Some(id.into()),
            secret: secret.into(),
            at: at.parse().unwrap(),
            people: 2,
            shift: "Mittag".into(),
            name: "Max Muster".into(),
            email: "max@example.com".into(),
            comment: String::new(),
            provisional: false,
            booked_at: "2026-11-01T10:00:00Z".parse().unwrap(),
            status,
            cancelled_at: None,
            state: None,
            cancelable: None,
            not_found: false,
            checked_at: None,
        }
    }

    fn store() -> Store {
        Store {
            reservations: vec![
                res("abc123", "s3cretA", "2026-11-10T10:45:00Z", Status::Booked),
                res("abd999", "s3cretB", "2026-11-02T10:45:00Z", Status::Booked),
                res(
                    "xyz000",
                    "s3cretC",
                    "2026-11-12T18:00:00Z",
                    Status::Cancelled,
                ),
            ],
        }
    }

    #[test]
    fn find_by_id_prefix_and_secret() {
        let s = store();
        assert_eq!(s.find("abc123").unwrap(), 0);
        assert_eq!(s.find("abc").unwrap(), 0);
        assert_eq!(s.find("s3cretB").unwrap(), 1);
        assert!(s.find("ab").is_err(), "ambiguous prefix");
        assert!(s.find("nope").is_err());
    }

    #[test]
    fn list_upcoming_only() {
        let s = store();
        let now: DateTime<Utc> = "2026-11-04T10:00:00Z".parse().unwrap();
        let ids: Vec<_> = s.list(false, now).iter().map(|r| r.short_id()).collect();
        assert_eq!(ids, ["abc123"]);
        assert_eq!(s.list(true, now).len(), 3);
    }

    #[test]
    fn apply_remote_updates_state_time_and_people() {
        use crate::api::ReservationInfo;
        let now: DateTime<Utc> = "2026-11-04T10:00:00Z".parse().unwrap();
        let mut r = res("abc123", "s", "2026-11-10T10:45:00Z", Status::Booked);
        assert_eq!(r.status_label(now), "booked");

        let info = ReservationInfo {
            state: Some(2),
            selected_date: Some("2026-11-10T11:00:00Z".parse().unwrap()),
            people_count: Some(3),
            cancelable: Some(true),
            ..ReservationInfo::default()
        };
        let changes = r.apply_remote(Some(&info), now);
        assert_eq!(r.status_label(now), "accepted");
        assert_eq!(changes.len(), 3, "{changes:?}");
        assert_eq!(r.people, 3);
        assert_eq!(r.cancelable, Some(true));

        // Unchanged on a second identical check.
        assert_eq!(r.apply_remote(Some(&info), now), Vec::<String>::new());

        // Cancelled by the restaurant (STORNO).
        let storno = ReservationInfo {
            state: Some(7),
            ..info
        };
        r.apply_remote(Some(&storno), now);
        assert_eq!(r.status, Status::Cancelled);
        assert!(!r.is_upcoming(now));
    }

    #[test]
    fn apply_remote_not_found() {
        let now: DateTime<Utc> = "2026-11-04T10:00:00Z".parse().unwrap();
        let mut r = res("abc123", "s", "2026-11-10T10:45:00Z", Status::Booked);
        assert_eq!(r.apply_remote(None, now), ["booked → not found"]);
        assert_eq!(r.status_label(now), "not found");
    }

    #[test]
    fn labels() {
        assert_eq!(state_label(1), "open");
        assert_eq!(state_label(17), "reconfirmed");
        assert_eq!(state_label(99), "state 99");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("bytesctl-test-{}", std::process::id()));
        let path = dir.join("bookings.json");
        store().save(&path).unwrap();
        let loaded = Store::load(&path).unwrap();
        assert_eq!(loaded.reservations.len(), 3);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
