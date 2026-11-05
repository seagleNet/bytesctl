mod api;
mod config;
mod edit;
mod menu;
mod store;
mod widget;

use anyhow::{Result, anyhow, bail};
use api::{BookingRequest, Client, RESTAURANT_PHONE, Timeslot};
use chrono::{Datelike, Days, Local, NaiveDate, NaiveTime};
use chrono_tz::Europe::Zurich;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use clap_complete::env::{CompleteEnv, Shells};
use config::{Config, non_empty};
use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::PathBuf;
use store::{Reservation, Status, Store};

/// Reserve a table at Restaurant bytes, Chur (bytes-chur.ch) from the command line.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Language for server messages [default: config or "de"]
    #[arg(long, global = true)]
    lang: Option<String>,

    /// Path to config file [default: ~/.config/bytesctl/config.toml]
    #[arg(long, global = true, env = "BYTES_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show which days in a month are open/closed
    Calendar {
        /// Month as YYYY-MM (default: current month)
        #[arg(long)]
        month: Option<String>,
    },
    /// List available time slots for a date
    Slots {
        /// Date as YYYY-MM-DD, "today" or "tomorrow" [default: config or today]
        date: Option<String>,
        /// Number of guests [default: config or 2]
        #[arg(short, long)]
        people: Option<u32>,
        /// Also show unavailable slots
        #[arg(long)]
        all: bool,
    },
    /// Place a reservation
    Book(BookArgs),
    /// List reservations made with bytesctl
    Bookings {
        /// Also show past and cancelled reservations
        #[arg(short, long)]
        all: bool,
        /// Show the cancellation secrets
        #[arg(long)]
        show_secrets: bool,
        /// Fetch the current status of each listed reservation from aleno
        #[arg(short, long)]
        refresh: bool,
    },
    /// Change a reservation: number of guests, date/time or comment
    Edit(edit::EditArgs),
    /// Cancel a reservation (by id/prefix from `bookings`, or a raw secret)
    Cancel {
        /// Reservation id, unique prefix, or secret. Optional if exactly one upcoming reservation
        #[arg(add = ArgValueCompleter::new(complete_reservation))]
        key: Option<String>,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
    /// Print this week's lunch menu (Mittagskarte)
    Menu(MenuArgs),
    /// Manage the config file
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Print the shell completion setup script (see the README for how to install it)
    Completions {
        /// Shell to generate the script for
        shell: clap_complete::Shell,
    },
}

// Each bool is an independent on/off command-line flag, not hidden state.
#[allow(clippy::struct_excessive_bools)]
#[derive(clap::Args)]
struct MenuArgs {
    /// Only show today's lunch [default: config menu.today]
    #[arg(short, long)]
    today: bool,
    /// Show the whole week, even if the config sets menu.today
    #[arg(short, long, conflicts_with = "today")]
    week: bool,
    /// Also show the regular dishes (bowls, seasonals, toppings) [default: config menu.all]
    #[arg(short, long)]
    all: bool,
    /// Hide the regular dishes, even if the config sets menu.all
    #[arg(long, conflicts_with = "all")]
    no_all: bool,
    /// Use this PDF URL instead of the one linked on the website
    #[arg(long, conflicts_with = "file")]
    url: Option<String>,
    /// Read a local PDF file instead of downloading
    #[arg(long)]
    file: Option<PathBuf>,
    /// Print the menu PDF URL and exit
    #[arg(long, conflicts_with_all = ["today", "week", "all", "no_all", "file"])]
    print_url: bool,
    /// Ignore the cached menu and fetch it from the website again
    #[arg(short, long, conflicts_with_all = ["url", "file"])]
    refresh: bool,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the config file path
    Path,
    /// Write a commented template config file
    Init {
        /// Overwrite an existing file
        #[arg(long)]
        force: bool,
    },
    /// Show the effective defaults after merging config and environment
    Show,
}

// Each bool is an independent on/off command-line flag, not hidden state.
#[allow(clippy::struct_excessive_bools)]
#[derive(clap::Args)]
struct BookArgs {
    /// Date as YYYY-MM-DD, "today" or "tomorrow" [default: config or today]
    #[arg(short, long)]
    date: Option<String>,
    /// Time as HH:MM, Swiss local time [default: config]
    #[arg(short, long)]
    time: Option<String>,
    /// Number of guests [default: config or 2]
    #[arg(short, long)]
    people: Option<u32>,
    #[arg(long, env = "BYTES_FIRST_NAME")]
    first_name: Option<String>,
    #[arg(long, env = "BYTES_LAST_NAME")]
    last_name: Option<String>,
    #[arg(long, env = "BYTES_EMAIL")]
    email: Option<String>,
    /// Phone in international format, e.g. +41791234567
    #[arg(long, env = "BYTES_PHONE")]
    phone: Option<String>,
    #[arg(long, env = "BYTES_SALUTATION", value_enum)]
    salutation: Option<Salutation>,
    /// Comment for the restaurant [default: config]
    #[arg(long)]
    comment: Option<String>,
    /// Subscribe to the restaurant's newsletter [default: config]
    #[arg(long)]
    newsletter: bool,
    /// Do not subscribe to the newsletter, even if the config says so
    #[arg(long, conflicts_with = "newsletter")]
    no_newsletter: bool,
    /// Skip confirmation prompt
    #[arg(short, long)]
    yes: bool,
    /// Show what would be booked without sending the reservation
    #[arg(long)]
    dry_run: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Salutation {
    Male,
    Female,
    Other,
    PreferNotSay,
}

impl Salutation {
    fn api_value(self) -> &'static str {
        match self {
            Salutation::Male => "male",
            Salutation::Female => "female",
            Salutation::Other => "other",
            Salutation::PreferNotSay => "preferNotSay",
        }
    }
}

fn main() {
    CompleteEnv::with_factory(Cli::command).complete();
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if let Cmd::Completions { shell } = cli.cmd {
        return print_completions(shell);
    }
    let (cfg_path, explicit) = match cli.config {
        Some(p) => (p, true),
        None => (config::default_path()?, false),
    };
    if let Cmd::Config(cmd) = &cli.cmd {
        return config_cmd(cmd, &cfg_path, explicit);
    }
    let cfg = config::load(&cfg_path, explicit)?;
    let lang = cli
        .lang
        .or_else(|| non_empty(cfg.defaults.lang.as_deref()))
        .unwrap_or_else(|| "de".into());
    let client = Client::new(&lang)?;
    match cli.cmd {
        Cmd::Calendar { month } => calendar(&client, month),
        Cmd::Slots { date, people, all } => {
            let date = default_date(date, &cfg)?;
            slots(&client, date, default_people(people, &cfg), all)
        }
        Cmd::Book(args) => book(&client, args, &cfg),
        Cmd::Bookings {
            all,
            show_secrets,
            refresh,
        } => bookings_cmd(&client, all, show_secrets, refresh),
        Cmd::Edit(args) => edit::edit_cmd(&client, &args),
        Cmd::Cancel { key, yes } => cancel_cmd(&client, key.as_deref(), yes),
        Cmd::Menu(args) => menu_cmd(args, &cfg),
        Cmd::Config(_) | Cmd::Completions { .. } => unreachable!(),
    }
}

/// Print the script that makes `shell` ask this binary for completions on each Tab press.
fn print_completions(shell: clap_complete::Shell) -> Result<()> {
    let name = env!("CARGO_PKG_NAME");
    let shells = Shells::builtins();
    let completer = shells
        .completer(&shell.to_string())
        .ok_or_else(|| anyhow!("no completions for {shell}"))?;
    completer.write_registration("COMPLETE", name, name, name, &mut io::stdout())?;
    Ok(())
}

/// Tab completion for reservation arguments, from the local bookings file.
fn complete_reservation(current: &OsStr) -> Vec<CompletionCandidate> {
    let store = store::default_path().and_then(|path| Store::load(&path));
    store.map_or_else(
        |_| Vec::new(),
        |s| reservation_candidates(&s, chrono::Utc::now(), &current.to_string_lossy()),
    )
}

/// Upcoming reservations whose short id starts with `prefix`. Never offers secrets.
fn reservation_candidates(
    s: &Store,
    now: chrono::DateTime<chrono::Utc>,
    prefix: &str,
) -> Vec<CompletionCandidate> {
    s.list(false, now)
        .into_iter()
        .filter(|r| r.id.is_some() && r.short_id().starts_with(prefix))
        .map(|r| {
            let when = r.at.with_timezone(&Zurich).format("%a %d.%m. %H:%M");
            let help = format!("{when}, {} guest(s)", r.people);
            CompletionCandidate::new(r.short_id()).help(Some(help.into()))
        })
        .collect()
}

fn config_cmd(cmd: &ConfigCmd, path: &std::path::Path, explicit: bool) -> Result<()> {
    match cmd {
        ConfigCmd::Path => {
            println!("{}", path.display());
            Ok(())
        }
        ConfigCmd::Init { force } => {
            if path.exists() && !force {
                bail!(
                    "{} already exists (use --force to overwrite)",
                    path.display()
                );
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, config::TEMPLATE)?;
            println!("Wrote {}", path.display());
            Ok(())
        }
        ConfigCmd::Show => {
            let cfg = config::load(path, explicit)?;
            let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
            let show = |v: Option<String>| v.unwrap_or_else(|| "(not set)".into());
            let d = &cfg.defaults;
            let c = &cfg.contact;
            println!(
                "config file: {}{}",
                path.display(),
                if path.exists() { "" } else { " (missing)" }
            );
            println!(
                "date:        {}",
                show(non_empty(d.date.as_deref()).or(Some("today".into())))
            );
            println!("time:        {}", show(non_empty(d.time.as_deref())));
            println!("people:      {}", d.people.unwrap_or(2));
            println!(
                "lang:        {}",
                show(non_empty(d.lang.as_deref()).or(Some("de".into())))
            );
            println!("comment:     {}", show(non_empty(d.comment.as_deref())));
            println!("newsletter:  {}", d.newsletter.unwrap_or(false));
            println!(
                "first_name:  {}",
                show(env("BYTES_FIRST_NAME").or(non_empty(c.first_name.as_deref())))
            );
            println!(
                "last_name:   {}",
                show(env("BYTES_LAST_NAME").or(non_empty(c.last_name.as_deref())))
            );
            println!(
                "email:       {}",
                show(env("BYTES_EMAIL").or(non_empty(c.email.as_deref())))
            );
            println!(
                "phone:       {}",
                show(env("BYTES_PHONE").or(non_empty(c.phone.as_deref())))
            );
            println!(
                "salutation:  {}",
                show(env("BYTES_SALUTATION").or(non_empty(c.salutation.as_deref())))
            );
            println!("menu.all:    {}", cfg.menu.all.unwrap_or(false));
            println!("menu.today:  {}", cfg.menu.today.unwrap_or(false));
            Ok(())
        }
    }
}

fn menu_cmd(args: MenuArgs, cfg: &Config) -> Result<()> {
    let MenuArgs {
        today: today_flag,
        week,
        all,
        no_all,
        url,
        file,
        print_url,
        refresh,
    } = args;
    let today_only = !week && (today_flag || cfg.menu.today.unwrap_or(false));
    let all = !no_all && (all || cfg.menu.all.unwrap_or(false));
    let today = today();
    let (bytes, source_url) = match (file, url) {
        (Some(path), _) => {
            let bytes =
                std::fs::read(&path).map_err(|e| anyhow!("cannot read {}: {e}", path.display()))?;
            (bytes, None)
        }
        (None, Some(url)) => (menu::download(&http_client()?, &url)?, Some(url)),
        (None, None) => {
            let (bytes, url) = current_menu_pdf(today, refresh)?;
            (bytes, Some(url))
        }
    };
    if print_url {
        println!("{}", source_url.unwrap_or_default());
        return Ok(());
    }
    let parsed = menu::parse(&bytes, today)?;
    menu::print(&parsed, today, &menu::PrintOptions { today_only, all });
    Ok(())
}

fn http_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent("bytesctl/0.1")
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

/// The current lunch menu PDF and its URL, from the cache when still fresh, else the website.
fn current_menu_pdf(today: NaiveDate, refresh: bool) -> Result<(Vec<u8>, String)> {
    let cached = menu::load_cache();
    if !refresh && let Some((meta, bytes)) = &cached {
        let fresh = menu::parse(bytes, today).is_ok_and(|m| menu::cache_is_fresh(&m, meta, today));
        if fresh {
            return Ok((bytes.clone(), meta.url.clone()));
        }
    }

    let fetched = (|| -> Result<(Vec<u8>, String)> {
        let http = http_client()?;
        let url = menu::find_menu_url(&http)?;
        // Same PDF as last time (e.g. the new week isn't published yet): skip the download.
        let bytes = match &cached {
            Some((meta, bytes)) if meta.url == url && !refresh => bytes.clone(),
            _ => menu::download(&http, &url)?,
        };
        Ok((bytes, url))
    })();

    match (fetched, cached) {
        (Ok((bytes, url)), _) => {
            menu::store_cache(&url, &bytes);
            Ok((bytes, url))
        }
        (Err(e), Some((meta, bytes))) => {
            eprintln!(
                "warning: {e:#}; showing cached menu from {}",
                meta.fetched_at
                    .with_timezone(&Zurich)
                    .format("%d.%m.%Y %H:%M")
            );
            Ok((bytes, meta.url))
        }
        (Err(e), None) => Err(e),
    }
}

fn default_date(arg: Option<String>, cfg: &Config) -> Result<NaiveDate> {
    parse_date(
        &arg.or_else(|| non_empty(cfg.defaults.date.as_deref()))
            .unwrap_or_else(|| "today".into()),
    )
}

fn default_people(arg: Option<u32>, cfg: &Config) -> u32 {
    arg.or(cfg.defaults.people).unwrap_or(2)
}

/// Pick the CLI/env value, else the config value, else fail with a hint.
fn required(arg: Option<String>, cfg: Option<&str>, flag: &str, key: &str) -> Result<String> {
    arg.filter(|s| !s.trim().is_empty())
        .or_else(|| non_empty(cfg))
        .ok_or_else(|| anyhow!("missing {flag} (or set `{key}` in the config file)"))
}

fn today() -> NaiveDate {
    Local::now().with_timezone(&Zurich).date_naive()
}

fn parse_date(s: &str) -> Result<NaiveDate> {
    match s {
        "today" => Ok(today()),
        "tomorrow" => Ok(today() + Days::new(1)),
        _ => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .or_else(|_| NaiveDate::parse_from_str(s, "%d.%m.%Y"))
            .map_err(|_| anyhow!("invalid date '{s}', expected YYYY-MM-DD")),
    }
}

fn parse_time(s: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H.%M"))
        .map_err(|_| anyhow!("invalid time '{s}', expected HH:MM"))
}

fn local_time(slot: &Timeslot) -> NaiveTime {
    slot.selected_date.with_timezone(&Zurich).time()
}

fn slots_for_day(client: &Client, date: NaiveDate, people: u32) -> Result<Vec<Timeslot>> {
    let mut slots: Vec<_> = client
        .timeslots(date, people)?
        .into_iter()
        .filter(|s| s.selected_date.with_timezone(&Zurich).date_naive() == date)
        .collect();
    slots.sort_by_key(|s| s.selected_date);
    Ok(slots)
}

fn calendar(client: &Client, month: Option<String>) -> Result<()> {
    let start = match month {
        Some(m) => NaiveDate::parse_from_str(&format!("{m}-01"), "%Y-%m-%d")
            .map_err(|_| anyhow!("invalid month '{m}', expected YYYY-MM"))?,
        None => today().with_day(1).unwrap(),
    };
    let days = client.calendar(start)?;
    println!("{}", start.format("%B %Y"));
    let mut d = start;
    while d.month() == start.month() {
        let status = match days.get(&d) {
            Some(c) if c.is_past => "past",
            Some(c) if c.is_closed => "closed",
            Some(c) if c.is_holiday => "holiday",
            _ if d < today() => "past",
            _ => "open",
        };
        println!("  {}  {}", d.format("%a %d.%m.%Y"), status);
        d = d + Days::new(1);
    }
    Ok(())
}

fn slots(client: &Client, date: NaiveDate, people: u32, all: bool) -> Result<()> {
    let slots = slots_for_day(client, date, people)?;
    let shown: Vec<_> = slots.iter().filter(|s| all || s.is_available()).collect();
    if shown.is_empty() {
        println!(
            "No available slots on {} for {people} guest(s).",
            date.format("%a %d.%m.%Y")
        );
        return Ok(());
    }
    println!("{} — {people} guest(s):", date.format("%a %d.%m.%Y"));
    let mut current_shift = "";
    for s in shown {
        if s.shift_name != current_shift {
            current_shift = &s.shift_name;
            println!("  {current_shift}");
        }
        let note = match s.unavailable_reason() {
            Some(r) => format!("  ({r})"),
            None if s.is_provisional => "  (provisional — restaurant must confirm)".into(),
            None => String::new(),
        };
        println!("    {}{note}", local_time(s).format("%H:%M"));
    }
    Ok(())
}

struct Booking {
    date: NaiveDate,
    time: NaiveTime,
    people: u32,
    first_name: String,
    last_name: String,
    email: String,
    phone: String,
    salutation: Option<Salutation>,
    comment: String,
    newsletter: bool,
    yes: bool,
    dry_run: bool,
}

fn resolve_booking(a: BookArgs, cfg: &Config) -> Result<Booking> {
    let c = &cfg.contact;
    let salutation = match a.salutation {
        Some(s) => Some(s),
        None => non_empty(c.salutation.as_deref())
            .map(|s| {
                Salutation::from_str(&s, true).map_err(|_| {
                    anyhow!(
                        "invalid salutation '{s}' in config (male, female, other, prefer-not-say)"
                    )
                })
            })
            .transpose()?,
    };
    let time = a
        .time
        .or_else(|| non_empty(cfg.defaults.time.as_deref()))
        .ok_or_else(|| anyhow!("missing --time (or set `defaults.time` in the config file)"))?;
    Ok(Booking {
        date: default_date(a.date, cfg)?,
        time: parse_time(&time)?,
        people: default_people(a.people, cfg),
        first_name: required(
            a.first_name,
            c.first_name.as_deref(),
            "--first-name",
            "contact.first_name",
        )?,
        last_name: required(
            a.last_name,
            c.last_name.as_deref(),
            "--last-name",
            "contact.last_name",
        )?,
        email: required(a.email, c.email.as_deref(), "--email", "contact.email")?,
        phone: required(a.phone, c.phone.as_deref(), "--phone", "contact.phone")?.replace(' ', ""),
        salutation,
        comment: a
            .comment
            .or_else(|| non_empty(cfg.defaults.comment.as_deref()))
            .unwrap_or_default(),
        newsletter: !a.no_newsletter && (a.newsletter || cfg.defaults.newsletter.unwrap_or(false)),
        yes: a.yes,
        dry_run: a.dry_run,
    })
}

fn book(client: &Client, args: BookArgs, cfg: &Config) -> Result<()> {
    let a = resolve_booking(args, cfg)?;
    let (date, time) = (a.date, a.time);
    if a.people == 0 {
        bail!("--people must be at least 1");
    }
    if !a.phone.starts_with('+') {
        bail!("phone must be in international format, e.g. +41791234567");
    }
    if !a.email.contains('@') {
        bail!("invalid email '{}'", a.email);
    }

    let slots = slots_for_day(client, date, a.people)?;
    let Some(slot) = slots.iter().find(|s| local_time(s) == time) else {
        let avail: Vec<_> = slots
            .iter()
            .filter(|s| s.is_available())
            .map(|s| local_time(s).format("%H:%M").to_string())
            .collect();
        if avail.is_empty() {
            bail!("no slots available on {date} for {} guest(s)", a.people);
        }
        bail!(
            "no slot at {} on {date}. Available: {}",
            time.format("%H:%M"),
            avail.join(", ")
        );
    };
    if let Some(reason) = slot.unavailable_reason() {
        bail!(
            "slot {} on {date} is not bookable: {reason}",
            time.format("%H:%M")
        );
    }

    let questions = client.shift_questions(slot, a.people)?;
    if !questions.is_empty() {
        bail!(
            "the '{}' shift requires answering additional questions, which this tool does not support; \
             please book via https://bytes-chur.ch/kontakt/#reservation",
            slot.shift_name
        );
    }

    println!("Reservation at Restaurant bytes, Chur");
    println!(
        "  When:   {} {} ({})",
        date.format("%a %d.%m.%Y"),
        time.format("%H:%M"),
        slot.shift_name
    );
    println!("  Guests: {}", a.people);
    println!("  Name:   {} {}", a.first_name, a.last_name);
    println!("  Email:  {}", a.email);
    println!("  Phone:  {}", a.phone);
    if !a.comment.is_empty() {
        println!("  Note:   {}", a.comment);
    }
    if slot.is_provisional {
        println!("  (provisional: the restaurant has to confirm this booking)");
    }

    if a.dry_run {
        println!("Dry run — nothing was booked.");
        return Ok(());
    }
    if !a.yes && !confirm("Place this reservation?")? {
        bail!("aborted");
    }

    let req = BookingRequest {
        shift_id: slot.shift_id.clone(),
        room_id: String::new(),
        selected_date: slot
            .selected_date
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        people_count: a.people,
        is_walk_in: false,
        customer_first_name: a.first_name,
        customer_last_name: a.last_name,
        customer_email: a.email,
        customer_phone: a.phone,
        customer_sex: a.salutation.map(|s| s.api_value().to_string()),
        customer_newsletter: a.newsletter,
        comment: a.comment,
        questions: vec![],
        customer_additional_consents: vec![],
    };
    let resp = client.create_booking(&req)?;
    report_booking(client, &resp, &req, slot);
    Ok(())
}

/// Print the outcome of a booking and save it locally so it can be listed and cancelled.
fn report_booking(
    client: &Client,
    resp: &api::BookingResponse,
    req: &BookingRequest,
    slot: &Timeslot,
) {
    if resp.is_provisional {
        println!(
            "Reservation request sent (provisional — awaiting confirmation by the restaurant)."
        );
    } else {
        println!("Reservation booked! A confirmation email is on its way.");
    }
    if let Some(id) = &resp.reservation_id {
        println!("  Reservation ID: {id}");
    }
    let Some(secret) = &resp.secret else {
        eprintln!(
            "warning: the server returned no cancellation secret; cancel via the confirmation email"
        );
        return;
    };

    let now = chrono::Utc::now();
    let mut r = Reservation {
        id: resp.reservation_id.clone(),
        secret: secret.clone(),
        at: slot.selected_date,
        people: req.people_count,
        shift: slot.shift_name.clone(),
        name: format!("{} {}", req.customer_first_name, req.customer_last_name),
        email: req.customer_email.clone(),
        comment: req.comment.clone(),
        provisional: resp.is_provisional,
        booked_at: now,
        status: Status::Booked,
        cancelled_at: None,
        state: None,
        cancelable: None,
        not_found: false,
        checked_at: None,
    };
    // The booking already exists, so neither of these steps may fail the command.
    if let Ok(Some(info)) = client.reservation_info(secret) {
        r.apply_remote(Some(&info), now);
        println!("  Status:         {}", r.status_label(now));
    }
    let short = r.short_id();
    let cancelable = r.cancelable;
    let key = match save_reservation(r) {
        Ok(()) => short,
        Err(e) => {
            eprintln!("warning: could not save reservation locally: {e:#}");
            println!("  Secret:         {secret}");
            secret.clone()
        }
    };
    println!("{}", cancel_hint(cancelable, &key));
}

/// How to cancel a new booking. Only a definite "no" from aleno replaces the usual hint.
fn cancel_hint(cancelable: Option<bool>, key: &str) -> String {
    if cancelable == Some(false) {
        format!(
            "\n⚠ This reservation can't be cancelled online.\n  To cancel, call the restaurant: {RESTAURANT_PHONE}"
        )
    } else {
        format!("  Cancel with:    bytesctl cancel {key}")
    }
}

fn save_reservation(r: Reservation) -> Result<()> {
    let path = store::default_path()?;
    let mut s = Store::load(&path)?;
    s.reservations.push(r);
    s.save(&path)
}

fn describe(r: &Reservation) -> String {
    let local = r.at.with_timezone(&Zurich);
    format!(
        "{}  {} ({}), {} guest(s), {}",
        local.format("%a %d.%m.%Y %H:%M"),
        r.short_id(),
        r.shift,
        r.people,
        r.name
    )
}

fn bookings_cmd(client: &Client, all: bool, show_secrets: bool, refresh: bool) -> Result<()> {
    let path = store::default_path()?;
    let mut s = Store::load(&path)?;
    let now = chrono::Utc::now();

    if refresh {
        refresh_reservations(client, &mut s, all, now);
        save_store(&s, &path);
    }

    let list = s.list(all, now);
    if list.is_empty() {
        if all {
            println!("No saved reservations.");
        } else {
            println!("No upcoming reservations. (Use --all to include past and cancelled ones.)");
        }
        return Ok(());
    }
    println!(
        "{:<6}  {:<16}  {:>6}  {:<10}  {:<14}  NAME",
        "ID", "WHEN", "GUESTS", "SHIFT", "STATUS"
    );
    for r in &list {
        println!(
            "{:<6}  {:<16}  {:>6}  {:<10}  {:<14}  {}",
            r.short_id(),
            r.at.with_timezone(&Zurich).format("%a %d.%m. %H:%M"),
            r.people,
            r.shift,
            r.status_label(now),
            r.name
        );
        if !r.comment.is_empty() {
            println!("        note: {}", r.comment);
        }
        if r.cancelable == Some(false) && r.is_upcoming(now) {
            println!("        can no longer be cancelled online (call {RESTAURANT_PHONE})");
        }
        if show_secrets {
            println!("        secret: {}", r.secret);
        }
    }

    if list
        .iter()
        .any(|r| r.state == Some(1) && r.status == Status::Booked)
    {
        println!(
            "\nopen = registered by the restaurant but not explicitly accepted yet \
             (aleno uses the same state for provisional requests)."
        );
    }
    match list.iter().filter_map(|r| r.checked_at).min() {
        Some(t) if !refresh => println!(
            "\nStatus as of {} — use --refresh to update.",
            t.with_timezone(&Zurich).format("%d.%m. %H:%M")
        ),
        None => println!("\nStatus as recorded when booking — use --refresh to ask aleno."),
        Some(_) => {}
    }
    Ok(())
}

/// Ask aleno for the current state of every reservation that would be listed and is not
/// already cancelled locally. Failures are reported per reservation and don't abort.
fn refresh_reservations(
    client: &Client,
    s: &mut Store,
    all: bool,
    now: chrono::DateTime<chrono::Utc>,
) {
    for r in &mut s.reservations {
        if r.status == Status::Cancelled || !(all || r.is_upcoming(now)) {
            continue;
        }
        match client.reservation_info(&r.secret) {
            Ok(info) => {
                for change in r.apply_remote(info.as_ref(), now) {
                    println!("{}: {change}", r.short_id());
                }
            }
            Err(e) => eprintln!("warning: {}: could not fetch status: {e:#}", r.short_id()),
        }
    }
}

/// Save the bookings file, warning instead of failing (the server side already happened).
fn save_store(s: &Store, path: &std::path::Path) {
    if let Err(e) = s.save(path) {
        eprintln!("warning: could not update {}: {e:#}", path.display());
    }
}

/// The saved reservation selected by `key` (id, unique prefix or secret), or the only
/// upcoming one if no key is given. `Ok(None)` means `key` is a raw secret not saved locally.
fn resolve_key(
    s: &Store,
    key: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<usize>> {
    if let Some(k) = key {
        return match s.find(k) {
            Ok(i) => Ok(Some(i)),
            // Not one of ours: treat a long key as a raw secret (e.g. from an older booking).
            Err(_) if k.len() >= 12 => Ok(None),
            Err(e) => Err(e),
        };
    }
    let upcoming: Vec<usize> = (0..s.reservations.len())
        .filter(|&i| s.reservations[i].is_upcoming(now))
        .collect();
    match upcoming.as_slice() {
        [i] => Ok(Some(*i)),
        [] => bail!("no upcoming reservations"),
        _ => bail!("several upcoming reservations; pass the ID shown by `bytesctl bookings`"),
    }
}

fn cancel_cmd(client: &Client, key: Option<&str>, yes: bool) -> Result<()> {
    let path = store::default_path()?;
    let mut s = Store::load(&path)?;
    let now = chrono::Utc::now();

    let index = resolve_key(&s, key, now)?;

    let secret = match index {
        Some(i) => {
            let r = &s.reservations[i];
            if r.status == Status::Cancelled {
                bail!("reservation {} is already cancelled", r.short_id());
            }
            println!("{}", describe(r));
            if r.at < now {
                bail!("reservation {} is in the past", r.short_id());
            }
            r.secret.clone()
        }
        None => key.unwrap_or_default().to_string(),
    };

    check_cancelable(client, &mut s, index, &secret, now, &path)?;

    if !yes && !confirm("Cancel this reservation?")? {
        bail!("aborted");
    }
    client.cancel(&secret)?;
    println!("Reservation cancelled.");

    if let Some(i) = index {
        s.reservations[i].status = Status::Cancelled;
        s.reservations[i].cancelled_at = Some(now);
        save_store(&s, &path);
    }
    Ok(())
}

/// Ask aleno whether the reservation exists and may still be cancelled online, before
/// prompting. If the check itself fails, warn and let the cancel request decide.
fn check_cancelable(
    client: &Client,
    s: &mut Store,
    index: Option<usize>,
    secret: &str,
    now: chrono::DateTime<chrono::Utc>,
    path: &std::path::Path,
) -> Result<()> {
    let info = match client.reservation_info(secret) {
        Ok(info) => info,
        Err(e) => {
            eprintln!("warning: could not check the reservation with aleno: {e:#}");
            return Ok(());
        }
    };
    if let Some(i) = index {
        s.reservations[i].apply_remote(info.as_ref(), now);
        save_store(s, path);
    }
    let Some(info) = info else {
        bail!("aleno doesn't know this reservation (already cancelled or removed?)");
    };
    if index.is_none() {
        // Raw secret: show what is about to be cancelled.
        if let (Some(at), Some(p)) = (info.selected_date, info.people_count) {
            println!(
                "{}, {p} guest(s)",
                at.with_timezone(&Zurich).format("%a %d.%m.%Y %H:%M")
            );
        }
    }
    if info.state.is_some_and(store::state_is_cancelled) {
        bail!("this reservation is already cancelled (according to aleno)");
    }
    if info.cancelable == Some(false) {
        bail!(
            "this reservation can no longer be cancelled online; please call the restaurant at {RESTAURANT_PHONE}"
        );
    }
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_lowercase().as_str(),
        "y" | "yes" | "j" | "ja"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shell_has_completions() {
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Elvish,
            clap_complete::Shell::Fish,
            clap_complete::Shell::PowerShell,
            clap_complete::Shell::Zsh,
        ] {
            assert!(
                Shells::builtins().completer(&shell.to_string()).is_some(),
                "{shell}"
            );
        }
    }

    #[test]
    fn reservation_candidates_offer_upcoming_ids_only() {
        let reservation = |id: Option<&str>, at: &str, status| Reservation {
            id: id.map(str::to_string),
            secret: "topsecret".into(),
            at: at.parse().unwrap(),
            people: 2,
            shift: "Mittag".into(),
            name: "Max Muster".into(),
            email: String::new(),
            comment: String::new(),
            provisional: false,
            booked_at: at.parse().unwrap(),
            status,
            cancelled_at: None,
            state: None,
            cancelable: None,
            not_found: false,
            checked_at: None,
        };
        let s = Store {
            reservations: vec![
                reservation(Some("abc123xyz"), "2026-11-10T10:45:00Z", Status::Booked),
                reservation(Some("abd456xyz"), "2026-11-11T10:45:00Z", Status::Booked),
                reservation(Some("old000xyz"), "2026-11-01T10:45:00Z", Status::Booked),
                reservation(Some("can000xyz"), "2026-11-12T10:45:00Z", Status::Cancelled),
                reservation(None, "2026-11-13T10:45:00Z", Status::Booked),
            ],
        };
        let now = "2026-11-05T10:00:00Z".parse().unwrap();
        let ids = |prefix| -> Vec<String> {
            reservation_candidates(&s, now, prefix)
                .iter()
                .map(|c| c.get_value().to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(ids(""), ["abc123", "abd456"]);
        assert_eq!(ids("abd"), ["abd456"]);
        assert_eq!(ids("top"), Vec::<String>::new());
    }

    #[test]
    fn cancel_hint_warns_only_on_definite_no() {
        assert!(cancel_hint(Some(false), "abc").contains("can't be cancelled online"));
        assert!(!cancel_hint(Some(false), "abc").contains("bytesctl cancel"));
        for known in [Some(true), None] {
            assert_eq!(
                cancel_hint(known, "abc"),
                "  Cancel with:    bytesctl cancel abc"
            );
        }
    }
}
