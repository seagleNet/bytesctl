//! Minimal client for the aleno reservation widget API.
//!
//! The API key, restaurant id and domain come from the widget embedded on the restaurant's
//! homepage (see `widget`). Every call is `POST {domain}/api/<endpoint>` with a JSON body that
//! includes `restaurantId`, and `Authorization: <api key>`.

use crate::config::Restaurant;
use crate::widget::{self, Widget};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;

/// A non-2xx response from the aleno API, keeping its machine-readable error code.
#[derive(Debug)]
pub struct ApiError {
    pub endpoint: String,
    pub status: reqwest::StatusCode,
    pub code: Option<String>,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} returned {}: {}",
            self.endpoint, self.status, self.message
        )
    }
}

impl std::error::Error for ApiError {}

/// The parts of `popup/info` that bytesctl relies on. The endpoint is undocumented (its shape
/// was reconstructed from the booking widget), so every field is optional and parsed leniently.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ReservationInfo {
    /// aleno reservation state code (see `store::state_label`).
    pub state: Option<i64>,
    pub selected_date: Option<DateTime<Utc>>,
    pub people_count: Option<u32>,
    /// Whether the guest may still cancel online.
    pub cancelable: Option<bool>,
    pub shift_id: Option<String>,
    pub comment: Option<String>,
    /// Answers to shift questions; sent back unchanged when editing.
    pub questions: Option<Value>,
    /// Error code from `selfEditChecked` if aleno says self-editing is not possible.
    pub self_edit_error: Option<String>,
}

impl ReservationInfo {
    pub fn from_value(v: &Value) -> Self {
        let r = v.get("reservation");
        let field = |name: &str| r.and_then(|r| r.get(name));
        Self {
            state: field("state").and_then(|s| s.as_i64().or_else(|| s.as_str()?.parse().ok())),
            selected_date: field("selectedDate")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok()),
            people_count: field("peopleCount")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
            cancelable: v.get("cancelable").and_then(Value::as_bool),
            shift_id: v
                .pointer("/shift/_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            comment: field("comment").and_then(Value::as_str).map(str::to_string),
            questions: field("questions").filter(|q| q.is_array()).cloned(),
            self_edit_error: v
                .get("selfEditChecked")
                .filter(|c| c.get("type").and_then(Value::as_str) == Some("error"))
                .map(|c| {
                    c.get("code")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string()
                }),
        }
    }
}

/// Body for `popup/selfEdit/check` and `popup/selfEdit/submit` (same shape for both).
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SelfEditRequest {
    pub secret: String,
    pub people_count: u32,
    pub selected_date: String,
    pub shift_id: String,
    pub comment: String,
    pub questions: Value,
}

/// Result of a self-edit check or submit, as interpreted by the booking widget.
#[derive(Debug, Clone, PartialEq)]
pub enum SelfEditOutcome {
    /// The change is possible (check) or was applied (submit).
    Success,
    /// The new shift needs answers to extra questions.
    Questions,
    /// No capacity for the requested people/time.
    NoAvailability,
    /// The change would need a credit-card guarantee or payment.
    PaymentRequired,
    /// The new party size crosses the restaurant's group-reservation threshold.
    GroupReservation,
    /// Editing not possible; aleno's error code and message, if any.
    Rejected(Option<String>, Option<String>),
}

impl SelfEditOutcome {
    pub fn from_value(v: &Value) -> Self {
        let get = |k: &str| v.get(k).and_then(Value::as_str);
        match get("type") {
            Some("success") => Self::Success,
            Some("questions") => Self::Questions,
            Some("availabilities") => Self::NoAvailability,
            Some("payments-no-show") => Self::PaymentRequired,
            _ => match get("code") {
                Some("reservation-group-after" | "reservation-group-before") => {
                    Self::GroupReservation
                }
                Some("payments-required") => Self::PaymentRequired,
                code => Self::Rejected(
                    code.map(str::to_string),
                    get("text").or_else(|| get("message")).map(str::to_string),
                ),
            },
        }
    }
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::blocking::Client,
    lang: String,
    restaurant: Restaurant,
    /// Loaded on the first API call (from cache, else the restaurant's website).
    widget: RefCell<Option<Widget>>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Timeslot {
    pub selected_date: DateTime<Utc>,
    pub shift_id: String,
    pub shift_name: String,
    /// The restaurant's name according to aleno.
    #[serde(default)]
    pub restaurant_name: Option<String>,
    #[serde(default, rename = "timeslotFull")]
    pub full: bool,
    #[serde(default)]
    pub is_after_deadline: bool,
    #[serde(default)]
    pub is_provisional: bool,
    #[serde(default)]
    pub is_unavailable_reason: Option<Value>,
}

impl Timeslot {
    pub fn is_available(&self) -> bool {
        !self.full && !self.is_after_deadline && self.is_unavailable_reason.is_none()
    }

    pub fn unavailable_reason(&self) -> Option<String> {
        if let Some(r) = &self.is_unavailable_reason {
            let issue = r
                .get("issue")
                .and_then(Value::as_str)
                .unwrap_or("unavailable");
            return Some(issue.to_string());
        }
        if self.full {
            return Some("full".into());
        }
        if self.is_after_deadline {
            return Some("after booking deadline".into());
        }
        None
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CalendarDay {
    #[serde(default)]
    pub is_past: bool,
    #[serde(default)]
    pub is_closed: bool,
    #[serde(default)]
    pub is_holiday: bool,
}

#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BookingRequest {
    pub shift_id: String,
    pub room_id: String,
    pub selected_date: String,
    pub people_count: u32,
    pub is_walk_in: bool,
    pub customer_first_name: String,
    pub customer_last_name: String,
    pub customer_email: String,
    pub customer_phone: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub customer_sex: Option<String>,
    pub customer_newsletter: bool,
    pub comment: String,
    pub questions: Vec<Value>,
    pub customer_additional_consents: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookingResponse {
    pub reservation_id: Option<String>,
    pub secret: Option<String>,
    #[serde(default)]
    pub is_provisional: bool,
}

impl Client {
    pub fn new(lang: &str, restaurant: Restaurant) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .user_agent("bytesctl/0.1")
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            lang: lang.to_string(),
            restaurant,
            widget: RefCell::new(None),
        })
    }

    /// The same client, talking to another restaurant.
    pub fn for_restaurant(&self, restaurant: Restaurant) -> Self {
        Self {
            http: self.http.clone(),
            lang: self.lang.clone(),
            restaurant,
            widget: RefCell::new(None),
        }
    }

    pub fn restaurant(&self) -> &Restaurant {
        &self.restaurant
    }

    fn widget(&self) -> Result<Widget> {
        if let Some(w) = self.widget.borrow().as_ref() {
            return Ok(w.clone());
        }
        let w = widget::load(&self.http, &self.restaurant)?;
        *self.widget.borrow_mut() = Some(w.clone());
        Ok(w)
    }

    pub fn restaurant_id(&self) -> Result<String> {
        Ok(self.widget()?.restaurant_id)
    }

    /// POST to `endpoint`, adding `restaurantId`. If aleno rejects the cached API key, the widget
    /// settings are re-read from the website and the call is retried once.
    fn call(&self, endpoint: &str, body: &Value) -> Result<Value> {
        let w = self.widget()?;
        match self.send(&w, endpoint, body) {
            Err(e)
                if e.downcast_ref::<ApiError>()
                    .is_some_and(|a| a.code.as_deref() == Some("api-forbidden")) =>
            {
                let fresh = widget::refresh(&self.http, &self.restaurant)?;
                *self.widget.borrow_mut() = Some(fresh.clone());
                if fresh.key == w.key && fresh.restaurant_id == w.restaurant_id {
                    return Err(e);
                }
                self.send(&fresh, endpoint, body)
            }
            other => other,
        }
    }

    fn send(&self, w: &Widget, endpoint: &str, body: &Value) -> Result<Value> {
        let mut body = body.clone();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("restaurantId".into(), json!(w.restaurant_id));
        }
        let url = format!("{}/{endpoint}", w.api_base);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", &w.key)
            .header("Accept-Language", &self.lang)
            .json(&body)
            .send()
            .with_context(|| format!("request to {endpoint} failed"))?;
        let status = resp.status();
        let text = resp.text()?;
        let value: Value = serde_json::from_str(&text)
            .with_context(|| format!("invalid JSON from {endpoint} ({status}): {text}"))?;
        if !status.is_success() {
            let msg = value
                .get("text")
                .or_else(|| value.get("message"))
                .or_else(|| value.get("reason"))
                .or_else(|| value.get("error"))
                .map_or_else(
                    || value.to_string(),
                    |v| v.as_str().map_or_else(|| v.to_string(), str::to_string),
                );
            return Err(ApiError {
                endpoint: endpoint.to_string(),
                status,
                code: value
                    .get("code")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                message: msg,
            }
            .into());
        }
        Ok(value)
    }

    /// Opening calendar for the month containing `month_start` (only exceptional days are listed).
    pub fn calendar(&self, month_start: NaiveDate) -> Result<BTreeMap<NaiveDate, CalendarDay>> {
        let ms = month_start
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let v = self.call("aleno/v1/popup/calendar", &json!({ "selectedDate": ms }))?;
        let cal = v.get("calendar").cloned().unwrap_or(json!({}));
        Ok(serde_json::from_value(cal)?)
    }

    /// Timeslots starting on `date` (the server may also return a following day).
    pub fn timeslots(&self, date: NaiveDate, people: u32) -> Result<Vec<Timeslot>> {
        let v = self.call(
            "aleno/v1/popup/timeslots",
            &json!({
                "isWalkIn": false,
                "peopleCount": people,
                "roomId": "",
                "selectedDate": format!("{date}T00:00:00Z"),
                "selectedDateRangeMinimum": 1,
            }),
        )?;
        let list = v
            .get("bookings")
            .and_then(|b| b.get(self.restaurant_id().ok()?))
            .cloned()
            .unwrap_or(json!([]));
        Ok(serde_json::from_value(list)?)
    }

    /// Shift details for the chosen slot; returns the questions the shift asks (usually none).
    pub fn shift_questions(&self, slot: &Timeslot, people: u32) -> Result<Vec<Value>> {
        let v = self.call(
            "aleno/v1/shift/search",
            &json!({
                "__payments": true,
                "reservation": {
                    "peopleCount": people,
                    "selectedDate": slot.selected_date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "roomId": "",
                },
                "shiftIds": [slot.shift_id],
            }),
        )?;
        let shift = v
            .get("shifts")
            .and_then(|s| s.get(0))
            .ok_or_else(|| anyhow!("shift {} not found", slot.shift_id))?;
        if shift
            .pointer("/payments/paymentsNoShowEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(anyhow!(
                "this shift requires a credit card guarantee; please book via the website"
            ));
        }
        Ok(shift
            .get("questions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub fn create_booking(&self, req: &BookingRequest) -> Result<BookingResponse> {
        let mut body = serde_json::to_value(req)?;
        if let Some(obj) = body.as_object_mut() {
            obj.insert("fromRestaurantId".into(), json!(self.restaurant_id()?));
        }
        let v = self.call("aleno/v1/booking/create", &body)?;
        Ok(serde_json::from_value(v)?)
    }

    /// Current server-side state of a reservation; `None` if aleno doesn't know the secret.
    pub fn reservation_info(&self, secret: &str) -> Result<Option<ReservationInfo>> {
        match self.call("aleno/v1/popup/info", &json!({ "secret": secret })) {
            Ok(v) => Ok(Some(ReservationInfo::from_value(&v))),
            Err(e)
                if e.downcast_ref::<ApiError>()
                    .is_some_and(|a| a.code.as_deref() == Some("reservation-not-found")) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Ask aleno whether a change is possible (`submit = false`) or apply it (`submit = true`).
    pub fn self_edit(&self, req: &SelfEditRequest, submit: bool) -> Result<SelfEditOutcome> {
        let endpoint = if submit {
            "aleno/v1/popup/selfEdit/submit"
        } else {
            "aleno/v1/popup/selfEdit/check"
        };
        match self.call(endpoint, &serde_json::to_value(req)?) {
            Ok(v) => Ok(SelfEditOutcome::from_value(&v)),
            // Refusals may also come as HTTP errors carrying the same codes.
            Err(e) => match e.downcast_ref::<ApiError>() {
                Some(a) if a.status.is_client_error() => Ok(SelfEditOutcome::from_value(&json!({
                    "code": a.code,
                    "text": a.message,
                }))),
                _ => Err(e),
            },
        }
    }

    pub fn cancel(&self, secret: &str) -> Result<()> {
        self.call("aleno/v1/popup/cancel", &json!({ "secret": secret }))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_info_parsing() {
        let v = json!({
            "cancelable": false,
            "shift": { "_id": "x" },
            "reservation": {
                "state": 2,
                "selectedDate": "2026-11-10T10:45:00.000Z",
                "peopleCount": 4,
                "customerFirstName": "Max"
            }
        });
        let i = ReservationInfo::from_value(&v);
        assert_eq!(i.state, Some(2));
        assert_eq!(i.people_count, Some(4));
        assert_eq!(i.cancelable, Some(false));
        assert_eq!(
            i.selected_date,
            Some("2026-11-10T10:45:00Z".parse().unwrap())
        );
        // Unexpected shapes degrade to None instead of failing.
        let odd = ReservationInfo::from_value(
            &json!({ "reservation": { "state": "1", "selectedDate": 5 } }),
        );
        assert_eq!(odd.state, Some(1));
        assert_eq!(odd.selected_date, None);
        assert_eq!(
            ReservationInfo::from_value(&json!({})),
            ReservationInfo::default()
        );
    }

    #[test]
    fn reservation_info_edit_fields() {
        let v = json!({
            "shift": { "_id": "EZaAJaLRaMm3QY2Lo" },
            "selfEditChecked": { "type": "error", "code": "payments-required" },
            "reservation": { "comment": "Fenster", "questions": [] }
        });
        let i = ReservationInfo::from_value(&v);
        assert_eq!(i.shift_id.as_deref(), Some("EZaAJaLRaMm3QY2Lo"));
        assert_eq!(i.comment.as_deref(), Some("Fenster"));
        assert_eq!(i.questions, Some(json!([])));
        assert_eq!(i.self_edit_error.as_deref(), Some("payments-required"));
        let ok = json!({ "selfEditChecked": { "type": "success" } });
        assert_eq!(ReservationInfo::from_value(&ok).self_edit_error, None);
    }

    #[test]
    fn self_edit_outcomes() {
        use SelfEditOutcome as O;
        let p = |v: Value| O::from_value(&v);
        assert_eq!(p(json!({ "type": "success" })), O::Success);
        assert_eq!(
            p(json!({ "type": "questions", "questions": [] })),
            O::Questions
        );
        assert_eq!(p(json!({ "type": "availabilities" })), O::NoAvailability);
        assert_eq!(
            p(json!({ "type": "payments-no-show", "amount": 20 })),
            O::PaymentRequired
        );
        assert_eq!(
            p(json!({ "code": "payments-required" })),
            O::PaymentRequired
        );
        assert_eq!(
            p(json!({ "code": "reservation-group-after" })),
            O::GroupReservation
        );
        assert_eq!(
            p(json!({ "code": "too-late", "text": "Too late" })),
            O::Rejected(Some("too-late".into()), Some("Too late".into()))
        );
        assert_eq!(p(json!({})), O::Rejected(None, None));
    }
}
