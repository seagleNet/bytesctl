//! `bytesctl edit`: change the party size, date/time or comment of a reservation through
//! aleno's guest self-edit (`popup/selfEdit/check` then `popup/selfEdit/submit`).

use crate::api::{Client, RESTAURANT_PHONE, ReservationInfo, SelfEditOutcome, SelfEditRequest};
use crate::store::{self, Status, Store};
use crate::{confirm, local_time, parse_date, parse_time, resolve_key, save_store, slots_for_day};
use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use chrono_tz::Europe::Zurich;
use serde_json::json;

#[derive(clap::Args)]
pub struct EditArgs {
    /// Reservation id, unique prefix, or secret. Optional if exactly one upcoming reservation
    key: Option<String>,
    /// New number of guests
    #[arg(short, long)]
    people: Option<u32>,
    /// New date as YYYY-MM-DD, "today" or "tomorrow"
    #[arg(short, long)]
    date: Option<String>,
    /// New time as HH:MM (Swiss local time)
    #[arg(short, long)]
    time: Option<String>,
    /// New comment for the restaurant ("" removes it)
    #[arg(long)]
    comment: Option<String>,
    /// Skip confirmation prompt
    #[arg(short, long)]
    yes: bool,
    /// Only ask aleno whether the change is possible; change nothing
    #[arg(long)]
    dry_run: bool,
}

/// The reservation as it is now and as it should become.
struct Change {
    old_at: DateTime<Utc>,
    old_people: u32,
    old_comment: String,
    new_at: DateTime<Utc>,
    new_people: u32,
    new_comment: String,
    shift_id: String,
    /// Name of the new shift, if the time moved to a looked-up slot.
    shift_name: Option<String>,
}

pub fn edit_cmd(client: &Client, a: &EditArgs) -> Result<()> {
    if a.people.is_none() && a.date.is_none() && a.time.is_none() && a.comment.is_none() {
        bail!("nothing to change: pass --people, --date, --time and/or --comment");
    }
    if a.people == Some(0) {
        bail!("--people must be at least 1");
    }

    let path = store::default_path()?;
    let mut s = Store::load(&path)?;
    let now = Utc::now();
    let index = resolve_key(&s, a.key.as_deref(), now)?;
    let secret = match index {
        Some(i) => {
            let r = &s.reservations[i];
            if r.status == Status::Cancelled {
                bail!("reservation {} is cancelled", r.short_id());
            }
            if r.at < now {
                bail!("reservation {} is in the past", r.short_id());
            }
            r.secret.clone()
        }
        None => a.key.clone().unwrap_or_default(),
    };

    // Always start from aleno's current view of the reservation.
    let info = client.reservation_info(&secret)?;
    if let Some(i) = index {
        s.reservations[i].apply_remote(info.as_ref(), now);
        save_store(&s, &path);
    }
    let info =
        info.ok_or_else(|| anyhow!("aleno doesn't know this reservation (cancelled or removed?)"))?;
    if info.state.is_some_and(store::state_is_cancelled) {
        bail!("this reservation is cancelled (according to aleno)");
    }
    if let Some(code) = &info.self_edit_error {
        bail!(
            "aleno doesn't allow changing this reservation online ({}); please call the restaurant at {RESTAURANT_PHONE}",
            explain_self_edit_error(code)
        );
    }

    let change = plan_change(client, a, &info, index.map(|i| &s.reservations[i]))?;
    print_change(&change);

    let req = SelfEditRequest {
        secret: secret.clone(),
        people_count: change.new_people,
        selected_date: change
            .new_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        shift_id: change.shift_id.clone(),
        comment: change.new_comment.clone(),
        questions: info.questions.clone().unwrap_or_else(|| json!([])),
    };

    let outcome = client.self_edit(&req, false)?;
    if outcome != SelfEditOutcome::Success {
        bail!(explain_outcome(client, &outcome, &change));
    }
    if a.dry_run {
        println!("aleno says this change is possible. Dry run — nothing was changed.");
        return Ok(());
    }
    if !a.yes && !confirm("Apply this change?")? {
        bail!("aborted");
    }

    let outcome = client.self_edit(&req, true)?;
    if outcome != SelfEditOutcome::Success {
        bail!(
            "aleno rejected the change after checking it: {}",
            explain_outcome(client, &outcome, &change)
        );
    }
    println!("Reservation changed. aleno will send an updated confirmation email.");

    if let Some(i) = index {
        let r = &mut s.reservations[i];
        r.at = change.new_at;
        r.people = change.new_people;
        r.comment = change.new_comment;
        if let Some(name) = change.shift_name {
            r.shift = name;
        }
        save_store(&s, &path);
    }
    Ok(())
}

/// Work out the new values, keeping whatever wasn't passed on the command line.
fn plan_change(
    client: &Client,
    a: &EditArgs,
    info: &ReservationInfo,
    local: Option<&store::Reservation>,
) -> Result<Change> {
    let old_at = info
        .selected_date
        .or(local.map(|r| r.at))
        .ok_or_else(|| anyhow!("cannot determine the current reservation time"))?;
    let old_people = info
        .people_count
        .or(local.map(|r| r.people))
        .ok_or_else(|| anyhow!("cannot determine the current number of guests"))?;
    let old_comment = info
        .comment
        .clone()
        .or_else(|| local.map(|r| r.comment.clone()))
        .unwrap_or_default();

    let old_local = old_at.with_timezone(&Zurich);
    let date = a
        .date
        .as_deref()
        .map(parse_date)
        .transpose()?
        .unwrap_or_else(|| old_local.date_naive());
    let time = a
        .time
        .as_deref()
        .map(parse_time)
        .transpose()?
        .unwrap_or_else(|| old_local.time());
    let new_people = a.people.unwrap_or(old_people);
    let new_comment = a.comment.clone().unwrap_or_else(|| old_comment.clone());
    let moved = date != old_local.date_naive() || time != old_local.time();

    if !moved && new_people == old_people && new_comment == old_comment {
        bail!("nothing changes: the reservation already has these values");
    }

    let (new_at, shift_id, shift_name) = if let (Some(id), false) = (&info.shift_id, moved) {
        (old_at, id.clone(), None)
    } else {
        // The server decides availability (it accounts for this reservation's own seats);
        // here we only need the slot's exact start time and shift.
        let slots = slots_for_day(client, date, new_people)?;
        let slot = slots
            .iter()
            .find(|s| local_time(s) == time)
            .ok_or_else(|| {
                anyhow!(
                    "no slot at {} on {date}. {}",
                    time.format("%H:%M"),
                    free_times(client, date, new_people)
                )
            })?;
        (
            slot.selected_date,
            slot.shift_id.clone(),
            Some(slot.shift_name.clone()),
        )
    };

    Ok(Change {
        old_at,
        old_people,
        old_comment,
        new_at,
        new_people,
        new_comment,
        shift_id,
        shift_name,
    })
}

fn print_change(c: &Change) {
    let fmt = |at: DateTime<Utc>| {
        at.with_timezone(&Zurich)
            .format("%a %d.%m.%Y %H:%M")
            .to_string()
    };
    let line = |label: &str, old: String, new: String| {
        if old == new {
            println!("  {label:<7} {old}");
        } else {
            println!("  {label:<7} {old}  →  {new}");
        }
    };
    println!("Change reservation:");
    line("When:", fmt(c.old_at), fmt(c.new_at));
    line(
        "Guests:",
        c.old_people.to_string(),
        c.new_people.to_string(),
    );
    if !c.old_comment.is_empty() || !c.new_comment.is_empty() {
        let show = |s: &str| {
            if s.is_empty() {
                "(none)".to_string()
            } else {
                s.to_string()
            }
        };
        line("Note:", show(&c.old_comment), show(&c.new_comment));
    }
}

/// "Free times that day: …" for error messages (best effort).
fn free_times(client: &Client, date: NaiveDate, people: u32) -> String {
    match slots_for_day(client, date, people) {
        Ok(slots) => {
            let free: Vec<String> = slots
                .iter()
                .filter(|s| s.is_available())
                .map(|s| local_time(s).format("%H:%M").to_string())
                .collect();
            if free.is_empty() {
                format!("No free times that day for {people} guest(s).")
            } else {
                format!("Free times that day: {}", free.join(", "))
            }
        }
        Err(_) => String::new(),
    }
}

fn explain_outcome(client: &Client, outcome: &SelfEditOutcome, c: &Change) -> String {
    let local = c.new_at.with_timezone(&Zurich);
    match outcome {
        SelfEditOutcome::Success => "ok".into(),
        SelfEditOutcome::NoAvailability => {
            let time: NaiveTime = local.time();
            format!(
                "no table for {} guest(s) on {} at {}. {}",
                c.new_people,
                local.format("%a %d.%m.%Y"),
                time.format("%H:%M"),
                free_times(client, local.date_naive(), c.new_people)
            )
        }
        SelfEditOutcome::Questions => "the new time's shift asks additional questions, which bytesctl doesn't support; please use the change link in your confirmation email".into(),
        SelfEditOutcome::PaymentRequired => "this change would need a credit-card guarantee or payment; please use the change link in your confirmation email".into(),
        SelfEditOutcome::GroupReservation => format!(
            "with {} guests this becomes a group reservation, which the restaurant arranges directly; please call {RESTAURANT_PHONE}",
            c.new_people
        ),
        SelfEditOutcome::Rejected(code, msg) => format!(
            "aleno rejected the change: {}",
            msg.as_deref()
                .or(code.as_deref())
                .unwrap_or("editing not possible")
        ),
    }
}

fn explain_self_edit_error(code: &str) -> &str {
    match code {
        "payments-enabled" | "payments-required" => "payments are required for changes",
        "self-edit-disabled" => "online changes are switched off",
        other => other,
    }
}
