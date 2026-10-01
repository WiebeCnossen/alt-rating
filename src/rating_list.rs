use crate::cache::save_standard_rating_list;
use crate::http::fetch_bytes_with_retry;
use crate::model::Player;
use chrono::{Datelike, Utc};
use regex::Regex;
use std::error::Error;
use std::io::{Cursor, Read};
use std::sync::LazyLock;
use zip::ZipArchive;

/// Full standard rating list (zipped fixed-width text).
const STANDARD_RATING_LIST_URL: &str = "https://ratings.fide.com/download/standard_rating_list.zip";

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

/// Download the standard rating list and return active men and women, highest rating first.
pub async fn fetch_top_players(
    max_attempts: u32,
) -> Result<(Vec<Player>, Vec<Player>), Box<dyn Error>> {
    let bytes = fetch_bytes_with_retry(STANDARD_RATING_LIST_URL, max_attempts, |bytes| {
        bytes.starts_with(b"PK")
    })
    .await?;
    let text = rating_list_text_from_zip(&bytes)?;
    let (year, month) = parse_rating_list_period(&text)?;
    save_standard_rating_list(year, month, &text).await?;
    verify_rating_list_is_current_month(year, month)?;
    parse_top_players_from_rating_list(&text)
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

fn verify_rating_list_is_current_month(year: i32, month: u32) -> Result<(), Box<dyn Error>> {
    let today = Utc::now().date_naive();
    if year == today.year() && month == today.month() {
        return Ok(());
    }
    Err(format!(
        "rating list period {year:04}-{month:02} is not the current month {:04}-{:02}",
        today.year(),
        today.month()
    )
    .into())
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

fn parse_top_players_from_rating_list(
    text: &str,
) -> Result<(Vec<Player>, Vec<Player>), Box<dyn Error>> {
    let mut men = Vec::new();
    let mut women = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if idx == 0 || line.len() < COL_FLAG_START {
            continue;
        }
        let flag = line[COL_FLAG_START..].trim();
        if flag.contains('i') {
            continue;
        }
        let sex = line.as_bytes().get(COL_SEX).copied().unwrap_or(b' ');
        let rating = line[COL_RATING].trim();
        if rating.is_empty() || rating.parse::<u32>().is_err() {
            continue;
        }
        let recent_games = match line.get(COL_GMS).map(str::trim).unwrap_or("") {
            "" => 0,
            gms => match gms.parse::<u32>() {
                Ok(n) => n,
                Err(_) => continue,
            },
        };
        let player = Player {
            fide_id: line[COL_ID].trim().to_string(),
            name: line[COL_NAME].trim().to_string(),
            rating: rating.to_string(),
            recent_games,
        };
        match sex {
            b'M' => men.push(player),
            b'F' => women.push(player),
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

        let (men, women) = parse_top_players_from_rating_list(&text).unwrap();
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
        assert_eq!(men[0].recent_games, 9);
        assert_eq!(women[0].recent_games, 3);
    }

    #[test]
    fn parses_rating_list_period_from_header() {
        let text = "ID Number      Name                                                         Fed Sex Tit  WTit OTit           FOA SEP26 Gms K  B-day Flag\n";
        assert_eq!(parse_rating_list_period(text).unwrap(), (2026, 9));
    }

    #[test]
    fn accepts_rating_list_for_current_month() {
        let today = Utc::now().date_naive();
        verify_rating_list_is_current_month(today.year(), today.month()).unwrap();
    }

    #[test]
    fn rejects_rating_list_for_other_month() {
        let today = Utc::now().date_naive();
        let other_month = if today.month() == 1 {
            2
        } else {
            today.month() - 1
        };
        let err = verify_rating_list_is_current_month(today.year(), other_month).unwrap_err();
        assert!(err.to_string().contains("is not the current month"));
    }
}
