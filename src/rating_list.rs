use crate::cache::{has_rating_list, load_rating_list_players, save_rating_list_players};
use crate::http::fetch_bytes_with_retry;
use crate::model::{CompactPlayer, Player};
use chrono::{Datelike, Months, NaiveDate, Utc};
use regex::Regex;
use std::error::Error;
use std::io::{Cursor, Read};
use std::sync::LazyLock;
use zip::ZipArchive;

/// Full standard rating list (zipped fixed-width text) for the current month.
const STANDARD_RATING_LIST_URL: &str = "https://ratings.fide.com/download/standard_rating_list.zip";

/// Minimum rating loaded for top-30 / activity lookups.
pub const COMPACT_RATING_FLOOR: u32 = 2200;

/// Fixed-width columns in `standard_rating_list.txt`.
const COL_ID: std::ops::Range<usize> = 0..15;
const COL_NAME: std::ops::Range<usize> = 15..76;
const COL_SEX: usize = 80;
const COL_RATING: std::ops::Range<usize> = 113..119;
const COL_GMS: std::ops::Range<usize> = 119..122;
const COL_FLAG_START: usize = 132;

static RATING_LIST_PERIOD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)(\d{2})\b").unwrap()
});

/// Load the standard rating list for peildatum `year`/`month` from cache, or download it.
pub async fn fetch_top_players(
    year: i32,
    month: u32,
    max_attempts: u32,
    initial_wait_millis: u64,
) -> Result<(Vec<Player>, Vec<Player>), Box<dyn Error>> {
    if has_rating_list(year, month)
        && let Some(players) = load_rating_list_players(year, month, None)
    {
        return split_top_players(players);
    }

    let (list_year, list_month, players) =
        download_and_store_rating_list(year, month, max_attempts, initial_wait_millis).await?;
    if list_year != year || list_month != month {
        return Err(format!(
            "rating list period {list_year:04}-{list_month:02} does not match requested {year:04}-{month:02}"
        )
        .into());
    }
    split_top_players(players)
}

/// Load compact players (rating ≥ [`COMPACT_RATING_FLOOR`]) for a rating-list month.
pub async fn load_compact_players(
    year: i32,
    month: u32,
    max_attempts: u32,
    initial_wait_millis: u64,
) -> Result<Vec<CompactPlayer>, Box<dyn Error>> {
    if let Some(players) = load_rating_list_players(year, month, Some(COMPACT_RATING_FLOOR)) {
        return Ok(players);
    }

    let (list_year, list_month, _) =
        download_and_store_rating_list(year, month, max_attempts, initial_wait_millis).await?;
    if list_year != year || list_month != month {
        return Err(format!(
            "rating list period {list_year:04}-{list_month:02} does not match requested {year:04}-{month:02}"
        )
        .into());
    }
    load_rating_list_players(year, month, Some(COMPACT_RATING_FLOOR))
        .ok_or_else(|| "rating list missing after store".into())
}

/// Load compact lists for every month in `months`, returning them keyed by `YYYY-MM-01`.
pub async fn load_compact_lists_by_period(
    months: &[(i32, u32)],
    max_attempts: u32,
    initial_wait_millis: u64,
) -> Result<std::collections::HashMap<String, Vec<CompactPlayer>>, Box<dyn Error>> {
    let mut map = std::collections::HashMap::new();
    for &(year, month) in months {
        let period = format!("{year:04}-{month:02}-01");
        if map.contains_key(&period) {
            continue;
        }
        println!("LIST_MONTH\t{period}");
        let players = load_compact_players(year, month, max_attempts, initial_wait_millis).await?;
        map.insert(period, players);
    }
    Ok(map)
}

/// Download a month's rating list, parse players into tables, and return them.
async fn download_and_store_rating_list(
    year: i32,
    month: u32,
    max_attempts: u32,
    initial_wait_millis: u64,
) -> Result<(i32, u32, Vec<CompactPlayer>), Box<dyn Error>> {
    let today = Utc::now().date_naive();
    let url = if year == today.year() && month == today.month() {
        STANDARD_RATING_LIST_URL.to_string()
    } else {
        historical_standard_list_url(year, month)?
    };

    let bytes = fetch_bytes_with_retry(&url, max_attempts, initial_wait_millis, |bytes| {
        bytes.starts_with(b"PK")
    })
    .await?;
    let text = rating_list_text_from_zip(&bytes)?;
    let (list_year, list_month) = parse_rating_list_period(&text)?;
    let players: Vec<CompactPlayer> = iter_rating_list_players(&text).collect();
    save_rating_list_players(list_year, list_month, &players)?;
    Ok((list_year, list_month, players))
}

fn historical_standard_list_url(year: i32, month: u32) -> Result<String, Box<dyn Error>> {
    let abbrev =
        month_number_to_abbrev(month).ok_or_else(|| format!("invalid month number: {month}"))?;
    let yy = year % 100;
    Ok(format!(
        "https://ratings.fide.com/download/standard_{abbrev}{yy:02}frl.zip"
    ))
}

fn rating_list_text_from_zip(bytes: &[u8]) -> Result<String, Box<dyn Error>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut file = archive.by_index(0)?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)?;
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

/// Period encoded in the header rating column, e.g. `SEP26` → `(2026, 9)`.
fn parse_rating_list_period(text: &str) -> Result<(i32, u32), Box<dyn Error>> {
    let header = text.lines().next().ok_or("rating list is empty")?;
    let caps = RATING_LIST_PERIOD_RE
        .captures(header)
        .ok_or("rating list header has no period like SEP26")?;
    let month = month_abbrev_to_number(&caps[1])
        .ok_or_else(|| format!("unknown month abbreviation in rating list: {}", &caps[1]))?;
    let year = 2000 + caps[2].parse::<i32>()?;
    Ok((year, month))
}

fn month_abbrev_to_number(abbrev: &str) -> Option<u32> {
    Some(match &abbrev.to_ascii_lowercase()[..] {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    })
}

fn month_number_to_abbrev(month: u32) -> Option<&'static str> {
    Some(match month {
        1 => "jan",
        2 => "feb",
        3 => "mar",
        4 => "apr",
        5 => "may",
        6 => "jun",
        7 => "jul",
        8 => "aug",
        9 => "sep",
        10 => "oct",
        11 => "nov",
        12 => "dec",
        _ => return None,
    })
}

/// Subtract `months` from a `YYYY-MM-01` period string.
pub fn period_minus_months(period: &str, months: u32) -> Result<String, Box<dyn Error>> {
    let date = NaiveDate::parse_from_str(period, "%Y-%m-%d")?;
    let d = date
        .checked_sub_months(Months::new(months))
        .ok_or("period underflow")?;
    Ok(format!("{:04}-{:02}-01", d.year(), d.month()))
}

/// Parse `YYYY-MM-01` into `(year, month)`.
pub fn parse_period(period: &str) -> Result<(i32, u32), Box<dyn Error>> {
    let date = NaiveDate::parse_from_str(period, "%Y-%m-%d")?;
    Ok((date.year(), date.month()))
}

fn split_top_players(
    players: Vec<CompactPlayer>,
) -> Result<(Vec<Player>, Vec<Player>), Box<dyn Error>> {
    let mut men = Vec::new();
    let mut women = Vec::new();
    for player in players {
        let out = Player {
            fide_id: player.fide_id,
            name: player.name,
            rating: player.rating.to_string(),
        };
        match player.sex {
            'M' => men.push(out),
            'F' => women.push(out),
            _ => {}
        }
    }

    let by_rating_desc = |a: &Player, b: &Player| {
        b.rating
            .parse::<u32>()
            .unwrap_or(0)
            .cmp(&a.rating.parse::<u32>().unwrap_or(0))
    };
    men.sort_by(by_rating_desc);
    women.sort_by(by_rating_desc);

    if men.is_empty() || women.is_empty() {
        return Err("rating list did not yield players".into());
    }
    Ok((men, women))
}

fn iter_rating_list_players(text: &str) -> impl Iterator<Item = CompactPlayer> + '_ {
    text.lines().enumerate().filter_map(|(idx, line)| {
        if idx == 0 || line.len() < COL_FLAG_START {
            return None;
        }
        let flag = line[COL_FLAG_START..].trim();
        if flag.contains('i') {
            return None;
        }
        let sex = line.as_bytes().get(COL_SEX).copied().unwrap_or(b' ') as char;
        if sex != 'M' && sex != 'F' {
            return None;
        }
        let rating = line[COL_RATING].trim().parse::<u32>().ok()?;
        let gms = match line.get(COL_GMS).map(str::trim).unwrap_or("") {
            "" => 0,
            gms => gms.parse::<u32>().ok()?,
        };
        Some(CompactPlayer {
            fide_id: line[COL_ID].trim().to_string(),
            name: line[COL_NAME].trim().to_string(),
            sex,
            rating,
            gms,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rating_list_line(
        id: &str,
        name: &str,
        sex: char,
        rating: u32,
        gms: u32,
        flag: &str,
    ) -> String {
        let mut line = vec![b' '; 136];
        let put = |line: &mut [u8], start: usize, value: &str| {
            let bytes = value.as_bytes();
            line[start..start + bytes.len()].copy_from_slice(bytes);
        };
        put(&mut line, 0, id);
        put(&mut line, 15, name);
        line[80] = sex as u8;
        put(&mut line, 113, &format!("{rating}"));
        put(&mut line, 119, &format!("{gms}"));
        put(&mut line, 132, flag);
        String::from_utf8(line).unwrap()
    }

    #[test]
    fn parses_rating_list_skipping_inactive_and_sorting_by_rating() {
        let text = [
            "ID Number      Name                                                         Fed Sex Tit  WTit OTit           FOA SEP26 Gms K  B-day Flag".to_string(),
            rating_list_line("1", "Low, Man", 'M', 2000, 0, ""),
            rating_list_line("2", "Top, Man", 'M', 2800, 9, ""),
            rating_list_line("3", "Inactive, Man", 'M', 2900, 0, "i"),
            rating_list_line("4", "Near, Man", 'M', 2550, 0, ""),
            rating_list_line("5", "Top, Woman", 'F', 2500, 3, "w"),
            rating_list_line("6", "Inactive, Woman", 'F', 2600, 0, "wi"),
            rating_list_line("7", "Second, Woman", 'F', 2400, 0, ""),
            rating_list_line("8", "Far, Woman", 'F', 2200, 0, ""),
        ]
        .join("\n");

        let parsed: Vec<_> = iter_rating_list_players(&text).collect();
        assert_eq!(parsed.iter().find(|p| p.fide_id == "2").unwrap().gms, 9);
        assert_eq!(parsed.iter().find(|p| p.fide_id == "5").unwrap().gms, 3);

        let (men, women) = split_top_players(parsed).unwrap();
        assert_eq!(
            men.iter().map(|p| p.fide_id.as_str()).collect::<Vec<_>>(),
            vec!["2", "4", "1"]
        );
        assert_eq!(
            women.iter().map(|p| p.fide_id.as_str()).collect::<Vec<_>>(),
            vec!["5", "7", "8"]
        );
        assert!(men.iter().all(|p| p.fide_id != "3"));
        assert!(women.iter().all(|p| p.fide_id != "6"));
    }

    #[test]
    fn compact_parse_keeps_rating_floor() {
        let text = [
            "ID Number      Name                                                         Fed Sex Tit  WTit OTit           FOA SEP26 Gms K  B-day Flag".to_string(),
            rating_list_line("1", "Low, Man", 'M', 2199, 5, ""),
            rating_list_line("2", "Edge, Man", 'M', 2200, 5, ""),
            rating_list_line("3", "Top, Woman", 'F', 2500, 3, "w"),
        ]
        .join("\n");
        let players: Vec<_> = iter_rating_list_players(&text)
            .filter(|p| p.rating >= COMPACT_RATING_FLOOR)
            .collect();
        assert_eq!(
            players
                .iter()
                .map(|p| p.fide_id.as_str())
                .collect::<Vec<_>>(),
            vec!["2", "3"]
        );
    }

    #[test]
    fn historical_url_uses_mon_yy() {
        assert_eq!(
            historical_standard_list_url(2025, 9).unwrap(),
            "https://ratings.fide.com/download/standard_sep25frl.zip"
        );
    }

    #[test]
    fn period_minus_months_rolls_year() {
        assert_eq!(period_minus_months("2026-01-01", 1).unwrap(), "2025-12-01");
        assert_eq!(period_minus_months("2026-07-01", 1).unwrap(), "2026-06-01");
    }

    #[test]
    fn parses_rating_list_period_from_header() {
        let text = "ID Number      Name                                                         Fed Sex Tit  WTit OTit           FOA SEP26 Gms K  B-day Flag\n";
        assert_eq!(parse_rating_list_period(text).unwrap(), (2026, 9));
    }
}
