use chrono::{Datelike, Months, Utc};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;
use tokio::time::sleep;

/// Top-lists page loads player rows via AJAX from this endpoint.
const TOP_LIST_URL: &str = "https://ratings.fide.com/a_top.php?list=open";

/// Profile calculations tab lists periods and game counts.
const PROFILE_CALC_URL: &str = "https://ratings.fide.com/profile/{id}/calculations";

/// Calculations page loads game rows via AJAX from this endpoint
/// (`calculations.phtml?id_number=…&period=…&rating=0`).
const CALC_URL: &str =
    "https://ratings.fide.com/a_indv_calculation.php?id_number={id}&rating_period={period}&t=0";

const WAIT_MILLIS: u64 = 200;
const CACHE_DIR: &str = "cache";

#[derive(Clone, Serialize, Deserialize)]
struct Player {
    fide_id: String,
    name: String,
    rating: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Game {
    color: String,
    opponent_rating: String,
    result: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct PeriodResult {
    fide_id: String,
    period: String,
    games: Vec<Game>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let players = fetch_players().await?;
    let periods = last_12_complete_months();

    for player in &players {
        println!("{}\t{}\t{}", player.name, player.fide_id, player.rating);

        let results = periods_for_player(&player.fide_id, &periods).await?;
        if results.iter().all(|r| r.games.is_empty()) {
            return Err(format!(
                "no rated games for {} ({}) in any of the last {} periods",
                player.name,
                player.fide_id,
                periods.len()
            )
            .into());
        }

        for result in &results {
            for game in &result.games {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    player.name,
                    player.fide_id,
                    result.period,
                    game.color,
                    game.opponent_rating,
                    game.result
                );
            }
        }
    }

    Ok(())
}

async fn periods_for_player(
    fide_id: &str,
    periods: &[String],
) -> Result<Vec<PeriodResult>, Box<dyn Error>> {
    let cached = load_all_period_results(fide_id, periods).await;
    let force_refresh = match &cached {
        Some(results) => results.iter().all(|r| r.games.is_empty()),
        None => false,
    };

    if force_refresh {
        return fetch_periods_using_profile(fide_id, periods, true).await;
    }

    if let Some(results) = cached {
        return Ok(results);
    }

    fetch_periods_using_profile(fide_id, periods, false).await
}

async fn fetch_periods_using_profile(
    fide_id: &str,
    periods: &[String],
    force_refresh: bool,
) -> Result<Vec<PeriodResult>, Box<dyn Error>> {
    let active = fetch_periods_with_standard_games(fide_id).await?;
    let mut results = Vec::with_capacity(periods.len());

    for period in periods {
        let path = period_cache_path(fide_id, period);
        if active.contains(period) {
            let result = if force_refresh {
                fetch_period(fide_id, period).await?
            } else if let Some(cached) = load_period_result(&path, fide_id, period).await {
                cached
            } else {
                fetch_period(fide_id, period).await?
            };

            if result.games.is_empty() {
                let _ = fs::remove_file(&path).await;
                return Err(format!(
                    "period {period} for player {fide_id} is listed with standard games on the profile, but the calculation page has no games"
                )
                .into());
            }

            save_period_result(&path, &result).await?;
            results.push(result);
        } else {
            let result = PeriodResult {
                fide_id: fide_id.to_string(),
                period: period.clone(),
                games: Vec::new(),
            };
            save_period_result(&path, &result).await?;
            results.push(result);
        }
    }

    Ok(results)
}

async fn load_all_period_results(fide_id: &str, periods: &[String]) -> Option<Vec<PeriodResult>> {
    let mut results = Vec::with_capacity(periods.len());
    for period in periods {
        let path = period_cache_path(fide_id, period);
        results.push(load_period_result(&path, fide_id, period).await?);
    }
    Some(results)
}

async fn fetch_period(fide_id: &str, period: &str) -> Result<PeriodResult, Box<dyn Error>> {
    let html = fetch_calculations(fide_id, period).await?;
    Ok(PeriodResult {
        fide_id: fide_id.to_string(),
        period: period.to_string(),
        games: parse_games(&html)?,
    })
}

fn period_cache_path(fide_id: &str, period: &str) -> PathBuf {
    PathBuf::from(CACHE_DIR).join(format!("{fide_id}_{period}.json"))
}

async fn load_period_result(
    path: impl AsRef<Path>,
    fide_id: &str,
    period: &str,
) -> Option<PeriodResult> {
    let text = fs::read_to_string(path).await.ok()?;
    let result: PeriodResult = serde_json::from_str(&text).ok()?;
    (result.fide_id == fide_id && result.period == period).then_some(result)
}

async fn save_period_result(
    path: impl AsRef<Path>,
    result: &PeriodResult,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(CACHE_DIR).await?;
    let text = serde_json::to_string_pretty(result)?;
    fs::write(path, text).await?;
    Ok(())
}

fn build_client() -> Result<Client, Box<dyn Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("alt-rating/0.1"));
    headers.insert(
        "X-Requested-With",
        HeaderValue::from_static("XMLHttpRequest"),
    );

    Ok(Client::builder()
        .default_headers(headers)
        .pool_max_idle_per_host(0)
        .build()?)
}

async fn fetch_players() -> Result<Vec<Player>, Box<dyn Error>> {
    let client = build_client()?;
    let html = client
        .get(TOP_LIST_URL)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let re = Regex::new(
        r#"(?s)<a href=/profile/(\d+)>([^<]+)</a>.*?<td class=rating_column>(\d+)</td>"#,
    )?;

    Ok(re
        .captures_iter(&html)
        .map(|caps| Player {
            fide_id: caps[1].to_string(),
            name: caps[2].to_string(),
            rating: caps[3].to_string(),
        })
        .collect())
}

/// Periods on the profile page where standard games (STD GMS) > 0.
async fn fetch_periods_with_standard_games(
    fide_id: &str,
) -> Result<HashSet<String>, Box<dyn Error>> {
    sleep(Duration::from_millis(WAIT_MILLIS)).await;
    let url = PROFILE_CALC_URL.replacen("{id}", fide_id, 1);
    let client = build_client()?;
    let html = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(parse_periods_with_standard_games(&html)?)
}

fn parse_periods_with_standard_games(html: &str) -> Result<HashSet<String>, Box<dyn Error>> {
    // Period | STD rating | STD GMS | …
    let re = Regex::new(
        r#"(?s)<td width=75 align=right>&nbsp;(\d{4})-([A-Za-z]{3})&nbsp;</td>\s*<td[^>]*>&nbsp;\d+&nbsp;</td>\s*<td[^>]*>&nbsp;(\d+)&nbsp;</td>"#,
    )?;

    let mut periods = HashSet::new();
    for caps in re.captures_iter(html) {
        let std_games: u32 = caps[3].parse()?;
        if std_games == 0 {
            continue;
        }
        let month = month_abbrev_to_number(&caps[2]).ok_or_else(|| {
            format!("unknown month abbreviation on profile page: {}", &caps[2])
        })?;
        periods.insert(format!("{:04}-{:02}-01", &caps[1].parse::<i32>()?, month));
    }
    Ok(periods)
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

fn last_12_complete_months() -> Vec<String> {
    let today = Utc::now().date_naive();
    let first_of_this_month =
        chrono::NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();

    (0..12)
        .rev()
        .map(|i| {
            let d = first_of_this_month - Months::new(i + 1);
            format!("{:04}-{:02}-01", d.year(), d.month())
        })
        .collect()
}

async fn fetch_calculations(fide_id: &str, period: &str) -> Result<String, Box<dyn Error>> {
    let url = CALC_URL
        .replacen("{id}", fide_id, 1)
        .replacen("{period}", period, 1);
    sleep(Duration::from_millis(WAIT_MILLIS)).await;
    let client = build_client()?;
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?)
}

fn parse_games(html: &str) -> Result<Vec<Game>, Box<dyn Error>> {
    // Game rows: color note, then rating cell (may include a "*" marker), fed, result.
    let re = Regex::new(
        r#"(?s)<span class="(white|black)_note">.*?</span>.*?<td\s+class="list4">(\d+)\b.*?</td>\s*<td\s+class="list4 table_scale">[A-Z]{3}</td>\s*<td\s+class=list4>(\d+\.\d+)</td>"#,
    )?;

    Ok(re
        .captures_iter(html)
        .map(|caps| Game {
            color: caps[1].to_string(),
            opponent_rating: caps[2].to_string(),
            result: caps[3].to_string(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_color_rating_result() {
        let html = r#"
			<tr bgcolor=#efefef>
				<td class=list4 style="display: flex;"><span class="black_note">&nbsp;</span> Firouzja, Alireza</td>
				<td  class="list4">g</td>
				<td  class="list4 table_scale"></td>
				<td  class="list4">2759 </td>
				<td  class="list4 table_scale">FRA</td>
				<td  class=list4>0.00</td>
				<td  class="list4">1</td>
			</tr>
			<tr bgcolor=#efefef>
				<td class=list4><span class="white_note">&nbsp;</span> Keymer, Vincent</td>
				<td  class="list4">g</td>
				<td  class="list4 table_scale"></td>
				<td  class="list4">2759 </td>
				<td  class="list4 table_scale">GER</td>
				<td  class=list4>0.50</td>
				<td  class="list4">1</td>
			</tr>"#;
        let games = parse_games(html).unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].color, "black");
        assert_eq!(games[0].opponent_rating, "2759");
        assert_eq!(games[0].result, "0.00");
        assert_eq!(games[1].color, "white");
        assert_eq!(games[1].result, "0.50");
    }

    #[test]
    fn parses_games_with_provisional_rating_marker() {
        let html = r#"
				<td class=list4 style="display: flex;align-content: middle; padding-bottom:5px; padding-top: 5px;"><span class="black_note">&nbsp;</span> Costanza, Mitchell</td>
				<td  class="list4"></td>
				<td  class="list4 table_scale"></td>
				<td  class="list4">2407 <font color=blue>&nbsp;*&nbsp;</font></td>
				<td  class="list4 table_scale">USA</td>
				<td  class=list4>1.00</td>
				<td  class="list4">1</td>
				<td class=list4 style="display: flex;"><span class="white_note">&nbsp;</span> Villamil, Nahum Jose</td>
				<td  class="list4"></td>
				<td  class="list4 table_scale"></td>
				<td  class="list4">2407 <font color=blue>&nbsp;*&nbsp;</font></td>
				<td  class="list4 table_scale">COL</td>
				<td  class=list4>1.00</td>
				<td  class="list4">1</td>"#;
        let games = parse_games(html).unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].color, "black");
        assert_eq!(games[0].opponent_rating, "2407");
        assert_eq!(games[0].result, "1.00");
        assert_eq!(games[1].color, "white");
        assert_eq!(games[1].opponent_rating, "2407");
    }

    #[test]
    fn parses_profile_periods_with_standard_games() {
        let html = r#"
			<td width=75 align=right>&nbsp;2026-Sep&nbsp;</td>
			<td valign=top width=40 align=right>&nbsp;2823&nbsp;</td>
			<td valign=top width=30 align=right>&nbsp;0&nbsp;</td>
			<td width=75 align=right>&nbsp;2026-Jul&nbsp;</td>
			<td valign=top width=40 align=right>&nbsp;2823&nbsp;</td>
			<td valign=top width=30 align=right>&nbsp;10&nbsp;</td>
			<td width=75 align=right>&nbsp;2025-Dec&nbsp;</td>
			<td valign=top width=40 align=right>&nbsp;2840&nbsp;</td>
			<td valign=top width=30 align=right>&nbsp;1&nbsp;</td>"#;
        let periods = parse_periods_with_standard_games(html).unwrap();
        assert!(periods.contains("2026-07-01"));
        assert!(periods.contains("2025-12-01"));
        assert!(!periods.contains("2026-09-01"));
    }

    #[test]
    fn last_twelve_complete_months_are_ordered() {
        let periods = last_12_complete_months();
        assert_eq!(periods.len(), 12);
        assert!(periods[0].ends_with("-01"));
        assert_eq!(&periods[0][8..], "01");
        // contiguous months ending with previous calendar month
        assert!(periods[0] < periods[11]);
    }

    #[test]
    fn period_cache_path_uses_fide_id_and_period() {
        assert_eq!(
            period_cache_path("1503014", "2025-09-01"),
            PathBuf::from("cache").join("1503014_2025-09-01.json")
        );
    }
}
