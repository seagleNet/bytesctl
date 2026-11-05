//! Fetch and parse the weekly lunch menu PDF ("Mittagskarte") from bytes-chur.ch.
//!
//! The PDF's text stream order does not follow the visual layout (dishes and prices end up on
//! the wrong lines), so text is extracted with glyph positions and re-assembled into rows and
//! cells geometrically before parsing.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Datelike, Days, NaiveDate, Utc, Weekday};
use pdf_extract::{Document, MediaBox, OutputDev, OutputError, Transform};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::PathBuf;

pub const HOMEPAGE: &str = "https://bytes-chur.ch/";

// ---------------------------------------------------------------------------------------------
// Locating and downloading the PDF

/// Find the current lunch menu URL by scanning the homepage for a link to the Mittagskarte PDF.
pub fn find_menu_url(http: &reqwest::blocking::Client) -> Result<String> {
    let html = http
        .get(HOMEPAGE)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .context("cannot load bytes-chur.ch")?;
    let mut candidates: Vec<String> = html
        .split(['"', '\''])
        .filter(|s| s.starts_with("http") && s.to_lowercase().ends_with(".pdf"))
        .filter(|s| s.to_lowercase().contains("mittag"))
        .map(|s| s.replace("&#038;", "&").replace("&amp;", "&"))
        .collect();
    candidates.dedup();
    // Prefer the highest calendar week if several are linked (file names look like 26_45KW_...).
    candidates.sort_by_key(|u| std::cmp::Reverse(week_from_url(u)));
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no lunch menu (Mittagskarte) PDF linked on {HOMEPAGE}"))
}

fn week_from_url(url: &str) -> (u32, u32) {
    let name = url.rsplit('/').next().unwrap_or("");
    let mut parts = name.split('_');
    let year = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let week = parts
        .next()
        .and_then(|p| p.trim_end_matches(|c: char| c.is_alphabetic()).parse().ok())
        .unwrap_or(0);
    (year, week)
}

pub fn download(http: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>> {
    let resp = http
        .get(url)
        .send()
        .with_context(|| format!("cannot download {url}"))?;
    if !resp.status().is_success() {
        bail!("downloading {url} failed: {}", resp.status());
    }
    Ok(resp.bytes()?.to_vec())
}

// ---------------------------------------------------------------------------------------------
// Cache: the downloaded PDF plus where/when it came from, in $XDG_CACHE_HOME/bytesctl/.

/// While the cached menu's week is over, re-check the website at most this often.
const STALE_RECHECK_SECS: i64 = 3600;

#[derive(Serialize, Deserialize)]
pub struct CacheMeta {
    pub url: String,
    pub fetched_at: DateTime<Utc>,
}

pub fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("bytesctl"))
}

pub fn load_cache() -> Option<(CacheMeta, Vec<u8>)> {
    let dir = cache_dir()?;
    let meta: CacheMeta =
        serde_json::from_slice(&std::fs::read(dir.join("menu.json")).ok()?).ok()?;
    let bytes = std::fs::read(dir.join("menu.pdf")).ok()?;
    Some((meta, bytes))
}

/// Best effort: failing to write the cache never fails the command.
pub fn store_cache(url: &str, bytes: &[u8]) {
    let Some(dir) = cache_dir() else { return };
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let meta = serde_json::to_vec(&CacheMeta {
            url: url.to_string(),
            fetched_at: Utc::now(),
        })?;
        // PDF first, then metadata, each via rename so readers never see a half-written file.
        for (name, data) in [("menu.pdf", bytes), ("menu.json", meta.as_slice())] {
            let tmp = dir.join(format!("{name}.tmp"));
            std::fs::write(&tmp, data)?;
            std::fs::rename(&tmp, dir.join(name))?;
        }
        Ok(())
    };
    if let Err(e) = write() {
        eprintln!("warning: cannot write menu cache in {}: {e}", dir.display());
    }
}

/// A cached menu is fresh while its week is not over, or if it was checked very recently.
pub fn cache_is_fresh(menu: &Menu, meta: &CacheMeta, today: NaiveDate) -> bool {
    let week_current = menu.range.is_some_and(|(_, end)| end >= today);
    let recently_checked = (Utc::now() - meta.fetched_at).num_seconds() < STALE_RECHECK_SECS;
    week_current || recently_checked
}

// ---------------------------------------------------------------------------------------------
// Positional text extraction

#[derive(Debug, Clone)]
struct Glyph {
    x: f64,
    y: f64,
    adv: f64,
    text: String,
}

#[derive(Default)]
struct Collector {
    glyphs: Vec<Glyph>,
    page_width: f64,
    page: u32,
}

impl OutputDev for Collector {
    fn begin_page(
        &mut self,
        n: u32,
        mb: &MediaBox,
        _: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), OutputError> {
        self.page = n;
        if n == 1 {
            self.page_width = mb.urx - mb.llx;
        }
        Ok(())
    }
    fn end_page(&mut self) -> Result<(), OutputError> {
        Ok(())
    }
    fn output_character(
        &mut self,
        t: &Transform,
        width: f64,
        spacing: f64,
        font_size: f64,
        ch: &str,
    ) -> Result<(), OutputError> {
        // Only the first page carries the menu.
        if self.page == 1 {
            self.glyphs.push(Glyph {
                x: t.m31,
                y: t.m32,
                adv: (width * font_size + spacing) * t.m11.abs().max(1e-6),
                text: ch.to_string(),
            });
        }
        Ok(())
    }
    fn begin_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }
    fn end_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }
    fn end_line(&mut self) -> Result<(), OutputError> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct Cell {
    x: f64,
    text: String,
}

#[derive(Debug, Clone)]
struct Row {
    cells: Vec<Cell>,
}

impl Row {
    fn text(&self) -> String {
        self.cells
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Glyphs within this vertical distance belong to the same visual row.
const ROW_TOLERANCE: f64 = 2.5;
/// A horizontal gap larger than this starts a new cell (table column).
const CELL_GAP: f64 = 8.0;

fn layout(bytes: &[u8]) -> Result<(Vec<Row>, f64)> {
    let doc = Document::load_mem(bytes).context("not a valid PDF")?;
    let mut col = Collector::default();
    pdf_extract::output_doc(&doc, &mut col).map_err(|e| anyhow!("cannot read PDF: {e}"))?;

    let mut glyphs = col.glyphs;
    glyphs.sort_by(|a, b| b.y.total_cmp(&a.y));

    // Cluster glyphs into rows by y (top to bottom).
    let mut rows: Vec<(f64, Vec<Glyph>)> = Vec::new();
    for g in glyphs {
        match rows.last_mut() {
            Some((y, gs)) if (*y - g.y).abs() <= ROW_TOLERANCE => gs.push(g),
            _ => rows.push((g.y, vec![g])),
        }
    }

    let rows = rows
        .into_iter()
        .filter_map(|(_, mut gs)| {
            gs.sort_by(|a, b| a.x.total_cmp(&b.x));
            let mut cells: Vec<Cell> = Vec::new();
            let mut end = f64::NEG_INFINITY;
            for g in gs {
                if cells.is_empty() || g.x - end > CELL_GAP {
                    cells.push(Cell {
                        x: g.x,
                        text: String::new(),
                    });
                }
                cells.last_mut().unwrap().text.push_str(&g.text);
                end = end.max(g.x + g.adv);
            }
            let cells: Vec<Cell> = cells
                .into_iter()
                .map(|c| Cell {
                    x: c.x,
                    text: normalize(&c.text),
                })
                .filter(|c| !c.text.is_empty())
                .collect();
            (!cells.is_empty()).then_some(Row { cells })
        })
        .collect();
    Ok((rows, col.page_width))
}

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------------------------
// Menu model and parsing

#[derive(Debug, Clone)]
pub struct Item {
    pub label: Option<String>,
    pub name: String,
    pub price: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Day {
    pub name: String,
    pub weekday: Option<Weekday>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Default)]
pub struct Menu {
    pub title: Option<String>,
    pub range_text: Option<String>,
    pub range: Option<(NaiveDate, NaiveDate)>,
    pub starters: Vec<Item>,
    pub days: Vec<Day>,
    pub dessert: Vec<Item>,
    pub regulars: Vec<Section>,
}

fn weekday_from_name(s: &str) -> Option<Weekday> {
    let w = s.trim_end_matches(':').to_lowercase();
    Some(match w.as_str() {
        "mentig" | "määntig" | "mäntig" | "montag" => Weekday::Mon,
        "zistig" | "ziischtig" | "dienstag" => Weekday::Tue,
        "mittwuch" | "mittwoch" => Weekday::Wed,
        "donnstig" | "dunnstig" | "donnerstag" => Weekday::Thu,
        "fritig" | "friitig" | "freitag" => Weekday::Fri,
        "samstig" | "samstag" => Weekday::Sat,
        _ => return None,
    })
}

fn is_price(s: &str) -> bool {
    let t = s.trim_end_matches(".-").trim_end_matches(".–").trim();
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '.')
}

fn clean_price(s: &str) -> String {
    s.trim_end_matches(".-")
        .trim_end_matches(".–")
        .trim()
        .to_string()
}

fn is_menu_label(s: &str) -> bool {
    let l = s.to_lowercase();
    (l.starts_with("menü") || l.starts_with("menu")) && l.ends_with(':') && l.len() <= 10
}

/// Split trailing price from text: "Gemischter Saisonsalat: ... 8" -> ("...", Some("8")).
fn split_trailing_price(s: &str) -> (String, Option<String>) {
    let mut words: Vec<&str> = s.split_whitespace().collect();
    let mut price = None;
    if let Some(last) = words.last() {
        if is_price(last) {
            price = Some(clean_price(last));
            words.pop();
            // "12 .-" style
        } else if (*last == ".-" || *last == ".–")
            && words.len() >= 2
            && is_price(words[words.len() - 2])
        {
            price = Some(clean_price(words[words.len() - 2]));
            words.truncate(words.len() - 2);
        }
    }
    (words.join(" "), price)
}

const MONTHS: [&str; 12] = [
    "januar",
    "februar",
    "märz",
    "april",
    "mai",
    "juni",
    "juli",
    "august",
    "september",
    "oktober",
    "november",
    "dezember",
];

/// Parse "02. November bis 06. November" (year inferred relative to `today`).
fn parse_range(text: &str, today: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    let lower = text.to_lowercase();
    let (a, b) = lower.split_once(" bis ")?;
    let parse = |s: &str| -> Option<(u32, Option<u32>, Option<i32>)> {
        let s = s.replace('.', " ");
        let parts: Vec<&str> = s.split_whitespace().collect();
        let day: u32 = parts.first()?.parse().ok()?;
        let month = parts.get(1).and_then(|m| {
            MONTHS
                .iter()
                .position(|n| m.starts_with(&n[..3.min(n.len())]))
                .and_then(|i| u32::try_from(i + 1).ok())
                .or_else(|| m.parse().ok())
        });
        let year = parts.get(2).and_then(|y| y.parse().ok());
        Some((day, month, year))
    };
    let (d1, m1, y1) = parse(a)?;
    let (d2, m2, y2) = parse(b)?;
    let m2 = m2?;
    let m1 = m1.unwrap_or(m2);
    // Pick the year that puts the start closest to today.
    let year = y1.or(y2).unwrap_or_else(|| {
        (today.year() - 1..=today.year() + 1)
            .min_by_key(|y| {
                NaiveDate::from_ymd_opt(*y, m1, d1)
                    .map_or(i64::MAX, |d| (d - today).num_days().abs())
            })
            .unwrap()
    });
    let start = NaiveDate::from_ymd_opt(year, m1, d1)?;
    let end_year = if m2 < m1 { year + 1 } else { year };
    let end = NaiveDate::from_ymd_opt(y2.unwrap_or(end_year), m2, d2)?;
    Some((start, end))
}

/// The part of the menu PDF a row belongs to.
#[derive(PartialEq, Clone, Copy)]
enum Sec {
    Head,
    Starters,
    Days,
    Dessert,
    Regulars,
    Footer,
}

pub fn parse(bytes: &[u8], today: NaiveDate) -> Result<Menu> {
    let (rows, page_width) = layout(bytes)?;
    let mut menu = Menu::default();

    let mut sec = Sec::Head;
    let mut regular_rows: Vec<&Row> = Vec::new();

    for row in &rows {
        let text = row.text();
        let lower = text.to_lowercase();

        // Section switches
        if lower.starts_with("vorspeise") {
            sec = Sec::Starters;
            continue;
        }
        if lower.starts_with("tagesmenü") || lower.starts_with("tagesmenu") {
            sec = Sec::Days;
            continue;
        }
        if lower.starts_with("dessert der woche") {
            sec = Sec::Dessert;
            continue;
        }
        if lower.starts_with("keine lust") {
            sec = Sec::Regulars;
            continue;
        }
        if lower.starts_with("morgen wieder") || lower.contains("tischreservation") {
            sec = Sec::Footer;
            continue;
        }

        match sec {
            Sec::Head => {
                if lower.contains(" bis ")
                    && lower.chars().next().is_some_and(|c| c.is_ascii_digit())
                {
                    menu.range_text = Some(text.clone());
                    menu.range = parse_range(&text, today);
                } else if menu.title.is_none() && lower.contains("mittag") {
                    menu.title = Some(text.clone());
                }
            }
            Sec::Starters | Sec::Dessert => {
                let (name, price) = split_trailing_price(&text);
                let target = if sec == Sec::Starters {
                    &mut menu.starters
                } else {
                    &mut menu.dessert
                };
                if !name.is_empty() {
                    target.push(Item {
                        label: None,
                        name,
                        price,
                    });
                }
            }
            Sec::Days => {
                if !lower.starts_with("inkl.") {
                    parse_day_row(&mut menu, row);
                }
            }
            Sec::Regulars => regular_rows.push(row),
            Sec::Footer => {}
        }
    }

    menu.regulars = parse_regulars(&regular_rows, page_width);
    if menu.days.is_empty() {
        bail!("could not find the daily menus in the PDF (layout changed?)");
    }
    Ok(menu)
}

/// One row of the "Tagesmenü" table: a weekday name, "Menü N:" label, dish text and/or price.
fn parse_day_row(menu: &mut Menu, row: &Row) {
    let mut label = None;
    let mut dish = Vec::new();
    let mut price = None;
    for c in &row.cells {
        if let Some(wd) = weekday_from_name(&c.text) {
            menu.days.push(Day {
                name: c.text.clone(),
                weekday: Some(wd),
                items: vec![],
            });
        } else if is_menu_label(&c.text) {
            label = Some(c.text.trim_end_matches(':').to_string());
        } else if is_price(&c.text) {
            price = Some(clean_price(&c.text));
        } else {
            dish.push(c.text.clone());
        }
    }
    let Some(day) = menu.days.last_mut() else {
        return;
    };
    let dish = dish.join(" ");
    if label.is_none() && price.is_none() && !dish.is_empty() {
        // Wrapped continuation of the previous dish.
        if let Some(last) = day.items.last_mut() {
            last.name = format!("{} {dish}", last.name);
            return;
        }
    }
    if label.is_some() || !dish.is_empty() {
        day.items.push(Item {
            label,
            name: dish,
            price,
        });
    } else if let (Some(p), Some(last)) = (price, day.items.last_mut()) {
        last.price.get_or_insert(p);
    }
}

/// The "Keine Lust aufs Menü?" part is laid out in two columns, each a list of
/// "Name:" / description / price, with section headings like "Seasonals" or "Toppings".
fn parse_regulars(rows: &[&Row], page_width: f64) -> Vec<Section> {
    let mid = if page_width > 0.0 {
        page_width / 2.0
    } else {
        300.0
    };
    let mut columns: [Vec<Section>; 2] = [vec![], vec![]];

    for row in rows {
        for (ci, col) in columns.iter_mut().enumerate() {
            let cells: Vec<&Cell> = row
                .cells
                .iter()
                .filter(|c| (c.x >= mid) == (ci == 1))
                .collect();
            if cells.is_empty() {
                continue;
            }
            let mut text_parts = Vec::new();
            let mut price = None;
            for c in &cells {
                if is_price(&c.text) {
                    price = Some(clean_price(&c.text));
                } else {
                    text_parts.push(c.text.as_str());
                }
            }
            let text = text_parts.join(" ");
            let (text, inline_price) = split_trailing_price(&text);
            let price = price.or(inline_price);

            if col.is_empty() {
                col.push(Section {
                    title: String::new(),
                    items: vec![],
                });
            }
            let sec = col.last_mut().unwrap();

            if text.is_empty() {
                // Price on its own row: belongs to the last item without a price.
                if let (Some(p), Some(item)) = (
                    price,
                    sec.items.iter_mut().rev().find(|i| i.price.is_none()),
                ) {
                    item.price = Some(p);
                }
            } else if text.ends_with(':') {
                sec.items.push(Item {
                    label: None,
                    name: text.trim_end_matches(':').trim().to_string(),
                    price,
                });
            } else if price.is_some()
                && !sec
                    .items
                    .last()
                    .is_some_and(|i| i.price.is_none() && i.label.is_none())
            {
                // Single-line "Name price" item (e.g. toppings).
                sec.items.push(Item {
                    label: None,
                    name: text,
                    price,
                });
            } else if let Some(item) = sec.items.last_mut().filter(|i| i.price.is_none()) {
                // Description line of the current item.
                match &mut item.label {
                    Some(d) => *d = format!("{d} {text}"),
                    None => item.label = Some(text),
                }
                if price.is_some() {
                    item.price = price;
                }
            } else {
                // A heading.
                if sec.items.is_empty() && sec.title.is_empty() {
                    sec.title = text;
                } else {
                    col.push(Section {
                        title: text,
                        items: vec![],
                    });
                }
            }
        }
    }

    columns
        .into_iter()
        .flatten()
        .filter(|s| !s.items.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Output

pub struct PrintOptions {
    pub today_only: bool,
    pub all: bool,
}

pub fn print(menu: &Menu, today: NaiveDate, opts: &PrintOptions) {
    let width = 72;
    let title = menu.title.as_deref().unwrap_or("Mittagskarte");
    match (&menu.range_text, menu.range) {
        (Some(t), Some((s, _))) => println!("{title} — {t} (KW {})", s.iso_week().week()),
        (Some(t), None) => println!("{title} — {t}"),
        _ => println!("{title}"),
    }

    if let Some((start, end)) = menu.range {
        if end < today {
            let monday = today - Days::new(u64::from(today.weekday().num_days_from_monday()));
            if end < monday {
                println!("⚠ This menu is for a past week (no newer one published yet?).");
            }
        } else if start > today + Days::new(7) {
            println!("⚠ This menu is for a future week.");
        }
    }

    let today_wd = menu
        .range
        .filter(|(s, e)| *s <= today && today <= *e)
        .map(|_| today.weekday());

    if opts.today_only {
        let Some(wd) = today_wd else {
            println!(
                "\nNo lunch menu for today ({}).",
                today.format("%a %d.%m.%Y")
            );
            return;
        };
        match menu.days.iter().find(|d| d.weekday == Some(wd)) {
            Some(day) => {
                print_items("Vorspeise", &menu.starters, width);
                print_day(menu, day, None, width);
            }
            None => println!(
                "\nNo lunch menu for today ({}).",
                today.format("%a %d.%m.%Y")
            ),
        }
        print_items("Dessert der Woche", &menu.dessert, width);
        if opts.all {
            print_regulars(menu, width);
        }
        return;
    }

    print_items("Vorspeise", &menu.starters, width);
    for day in &menu.days {
        print_day(menu, day, today_wd, width);
    }
    print_items("Dessert der Woche", &menu.dessert, width);

    if opts.all {
        print_regulars(menu, width);
    }
}

fn print_regulars(menu: &Menu, width: usize) {
    println!("\nKeine Lust aufs Menü?");
    for sec in &menu.regulars {
        let items: Vec<Item> = sec
            .items
            .iter()
            .map(|i| Item {
                label: None,
                name: match &i.label {
                    Some(desc) => format!("{}: {desc}", i.name),
                    None => i.name.clone(),
                },
                price: i.price.clone(),
            })
            .collect();
        print_items(&sec.title, &items, width);
    }
}

fn print_day(menu: &Menu, day: &Day, today: Option<Weekday>, width: usize) {
    let date = match (menu.range, day.weekday) {
        (Some((start, _)), Some(wd)) => {
            let offset = wd
                .num_days_from_monday()
                .saturating_sub(start.weekday().num_days_from_monday());
            Some(start + Days::new(u64::from(offset)))
        }
        _ => None,
    };
    let mut header = day.name.clone();
    if let Some(d) = date {
        let _ = write!(header, " {}", d.format("%d.%m."));
    }
    if today.is_some() && day.weekday == today {
        header.push_str("  ◀ heute");
    }
    println!("\n{header}");
    for item in &day.items {
        let label = item
            .label
            .as_deref()
            .map(|l| format!("{l}  "))
            .unwrap_or_default();
        print_line(
            &format!("{label}{}", item.name),
            item.price.as_deref(),
            width,
        );
    }
}

fn print_items(title: &str, items: &[Item], width: usize) {
    if items.is_empty() {
        return;
    }
    if !title.is_empty() {
        println!("\n{title}");
    }
    for item in items {
        print_line(&item.name, item.price.as_deref(), width);
    }
}

fn print_line(text: &str, price: Option<&str>, width: usize) {
    let text = format!("  {text}");
    match price {
        Some(p) => {
            let used = text.chars().count() + p.chars().count();
            let pad = width.saturating_sub(used).max(2);
            println!("{text}{}{p}", " ".repeat(pad));
        }
        None => println!("{text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        assert_eq!(
            parse_range("02. November bis 06. November", today),
            Some((
                NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
                NaiveDate::from_ymd_opt(2026, 11, 6).unwrap()
            ))
        );
        let dec = NaiveDate::from_ymd_opt(2026, 12, 30).unwrap();
        assert_eq!(
            parse_range("29. Dezember bis 02. Januar", dec),
            Some((
                NaiveDate::from_ymd_opt(2026, 12, 29).unwrap(),
                NaiveDate::from_ymd_opt(2027, 1, 2).unwrap()
            ))
        );
    }

    #[test]
    fn prices() {
        assert_eq!(
            split_trailing_price("Crispy Chicken 12 .-"),
            ("Crispy Chicken".into(), Some("12".into()))
        );
        assert_eq!(
            split_trailing_price("Falafel 8.-"),
            ("Falafel".into(), Some("8".into()))
        );
        assert_eq!(
            split_trailing_price("Tagessuppe: Frag gerne"),
            ("Tagessuppe: Frag gerne".into(), None)
        );
    }

    #[test]
    fn url_week() {
        assert_eq!(
            week_from_url(
                "https://bytes-chur.ch/wp-content/uploads/2026/10/26_45KW_Bytes_Mittagskarte.pdf"
            ),
            (26, 45)
        );
    }
}
