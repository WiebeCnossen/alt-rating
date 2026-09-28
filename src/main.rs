use chrono::{Datelike, Months, Utc};
use regex::Regex;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;
use tokio::time::sleep;

/// Top-lists page loads player rows via AJAX from this endpoint.
const TOP_LIST_URL: &str = "https://ratings.fide.com/a_top.php?list={list}";

/// Profile calculations tab lists periods and game counts.
const PROFILE_CALC_URL: &str = "https://ratings.fide.com/profile/{id}/calculations";

/// Calculations page loads game rows via AJAX from this endpoint
/// (`calculations.phtml?id_number=…&period=…&rating=0`).
const CALC_URL: &str =
    "https://ratings.fide.com/a_indv_calculation.php?id_number={id}&rating_period={period}&t=0";

const WAIT_MILLIS: u64 = 500;
/// Stop doubling the backoff once the wait exceeds this many milliseconds.
const WAIT_DOUBLE_LIMIT_MILLIS: u64 = 5_000;
const CACHE_DIR: &str = "cache";
const OUTPUT_DIR: &str = "output";
/// Development coefficient used when checking that |ΔR| < 0.5 at the TPR.
const RATING_K: f64 = 10.0;
const TPR_GAME_THRESHOLD: usize = 50;

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
    let periods = last_12_complete_months();

    let men = process_top_list("open", &periods).await?;
    write_summary_csv(summary_csv_path("open"), &men).await?;

    let women = process_top_list("women", &periods).await?;
    write_summary_csv(summary_csv_path("women"), &women).await?;

    Ok(())
}

async fn process_top_list(
    list: &str,
    periods: &[String],
) -> Result<Vec<PlayerSummary>, Box<dyn Error>> {
    let players = fetch_players(list).await?;
    let mut summaries = Vec::with_capacity(players.len());

    println!("LIST\t{list}");
    for player in &players {
        println!("{}\t{}\t{}", player.name, player.fide_id, player.rating);

        let results = periods_for_player(&player.fide_id, periods).await?;
        if results.iter().all(|r| r.games.is_empty()) {
            return Err(format!(
                "no rated games for {} ({}) in any of the last {} periods",
                player.name,
                player.fide_id,
                periods.len()
            )
            .into());
        }

        let player_rating: f64 = player.rating.parse()?;
        let report = tpr_report(&results, player_rating)?;
        for window in &report.windows {
            println!(
                "TPR\t{}\t{}\t{}\t{}",
                window.start_period, window.end_period, window.games, window.tpr
            );
        }
        println!("{}\tTPR_MAX\t{}", player.name, report.max_tpr);
        println!(
            "{}\tTPR_ALL\t{}\t{}",
            player.name, report.all_games_tpr, report.total_games
        );

        summaries.push(PlayerSummary {
            name: player.name.clone(),
            fide_id: player.fide_id.clone(),
            rating: player.rating.clone(),
            max_tpr: report.max_tpr,
            all_games_tpr: report.all_games_tpr,
            total_games: report.total_games,
        });
    }

    summaries.sort_by(|a, b| {
        b.max_tpr
            .cmp(&a.max_tpr)
            .then_with(|| b.all_games_tpr.cmp(&a.all_games_tpr))
    });

    println!();
    for s in &summaries {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            s.name, s.fide_id, s.rating, s.max_tpr, s.all_games_tpr, s.total_games
        );
    }
    println!();

    Ok(summaries)
}

fn summary_csv_path(prefix: &str) -> PathBuf {
    let today = Utc::now().date_naive();
    PathBuf::from(OUTPUT_DIR).join(format!(
        "{prefix}-{:04}-{:02}.csv",
        today.year(),
        today.month()
    ))
}

async fn write_summary_csv(
    path: impl AsRef<Path>,
    summaries: &[PlayerSummary],
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(OUTPUT_DIR).await?;
    let mut out = String::from("name,fide_id,tpr_max,tpr_all,rating,games\n");
    for s in summaries {
        out.push_str(&format!(
            "{},{},{},{},{},{}\n",
            csv_escape(&s.name),
            csv_escape(&s.fide_id),
            s.max_tpr,
            s.all_games_tpr,
            csv_escape(&s.rating),
            s.total_games
        ));
    }
    fs::write(path, out).await?;
    Ok(())
}

fn csv_escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

struct PlayerSummary {
    name: String,
    fide_id: String,
    rating: String,
    max_tpr: i32,
    all_games_tpr: i32,
    total_games: usize,
}

struct TprWindow {
    start_period: String,
    end_period: String,
    games: usize,
    tpr: i32,
}

struct TprReport {
    windows: Vec<TprWindow>,
    max_tpr: i32,
    all_games_tpr: i32,
    total_games: usize,
}

/// Opponent rating and game score (0, 0.5, or 1).
type RatedGame = (f64, f64);
type PeriodGameList = (String, Vec<RatedGame>);

struct ConsecutiveWindow {
    start: usize,
    end: usize,
    games: Vec<RatedGame>,
}

fn tpr_report(results: &[PeriodResult], player_rating: f64) -> Result<TprReport, Box<dyn Error>> {
    let period_games: Vec<PeriodGameList> = results
        .iter()
        .map(|r| {
            let games = r
                .games
                .iter()
                .map(|g| {
                    Ok((
                        g.opponent_rating.trim().parse::<f64>()?,
                        g.result.trim().parse::<f64>()?,
                    ))
                })
                .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
            Ok((r.period.clone(), games))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;

    let total_games: usize = period_games.iter().map(|(_, g)| g.len()).sum();
    let mut all_games: Vec<RatedGame> = period_games
        .iter()
        .flat_map(|(_, g)| g.iter().copied())
        .collect();
    if all_games.len() < TPR_GAME_THRESHOLD {
        let virtual_opp = player_rating - 100.0;
        while all_games.len() < TPR_GAME_THRESHOLD {
            all_games.push((virtual_opp, 0.5));
        }
    }
    let all_games_tpr = tournament_performance_rating(&all_games);

    let windows = if total_games < TPR_GAME_THRESHOLD {
        let start = period_games
            .first()
            .map(|(p, _)| p.clone())
            .unwrap_or_default();
        let end = period_games
            .last()
            .map(|(p, _)| p.clone())
            .unwrap_or_default();
        vec![TprWindow {
            start_period: start,
            end_period: end,
            games: total_games,
            tpr: all_games_tpr,
        }]
    } else {
        consecutive_windows_with_at_least(&period_games, TPR_GAME_THRESHOLD)
            .into_iter()
            .map(|window| TprWindow {
                start_period: period_games[window.start].0.clone(),
                end_period: period_games[window.end].0.clone(),
                games: window.games.len(),
                tpr: tournament_performance_rating(&window.games),
            })
            .collect()
    };

    let max_tpr = windows
        .iter()
        .map(|w| w.tpr)
        .max()
        .ok_or("no TPR windows")?;
    Ok(TprReport {
        windows,
        max_tpr,
        all_games_tpr,
        total_games,
    })
}

/// All consecutive period ranges [start, end] with at least `min_games` games,
/// excluding ranges that start or end on an empty period.
fn consecutive_windows_with_at_least(
    period_games: &[PeriodGameList],
    min_games: usize,
) -> Vec<ConsecutiveWindow> {
    let n = period_games.len();
    let counts: Vec<usize> = period_games.iter().map(|(_, g)| g.len()).collect();
    let mut windows = Vec::new();

    for start in 0..n {
        if counts[start] == 0 {
            continue;
        }
        let mut total = 0usize;
        for end in start..n {
            total += counts[end];
            if counts[end] == 0 {
                continue;
            }
            if total >= min_games {
                let games: Vec<RatedGame> = period_games[start..=end]
                    .iter()
                    .flat_map(|(_, g)| g.iter().copied())
                    .collect();
                windows.push(ConsecutiveWindow { start, end, games });
            }
        }
    }

    windows
}

/// Expected score for a player rated `player` vs opponent `opponent` (FIDE 400-cap).
fn expected_score(player: f64, opponent: f64) -> f64 {
    let d = (player - opponent).clamp(-400.0, 400.0);
    1.0 / (1.0 + 10f64.powf(-d / 400.0))
}

fn score_delta(player: f64, games: &[RatedGame]) -> f64 {
    games
        .iter()
        .map(|&(opp, score)| score - expected_score(player, opp))
        .sum()
}

fn rating_change(player: f64, games: &[RatedGame], k: f64) -> f64 {
    k * score_delta(player, games)
}

/// Rating R such that |K·(S−E(R))| is minimized and < 0.5.
fn tournament_performance_rating(games: &[RatedGame]) -> i32 {
    // score_delta increases with player rating; find crossing near 0.
    let mut lo = 0i32;
    let mut hi = 4500i32;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if score_delta(mid as f64, games) > 0.0 {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }

    let candidates = [
        lo.saturating_sub(2),
        lo.saturating_sub(1),
        lo,
        lo + 1,
        lo + 2,
    ];
    let mut best_r = lo;
    let mut best_abs = f64::INFINITY;
    for r in candidates {
        let change = rating_change(r as f64, games, RATING_K).abs();
        if change < best_abs {
            best_abs = change;
            best_r = r;
        }
    }
    best_r
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
            let cached = if force_refresh {
                None
            } else {
                load_period_result(&path, fide_id, period).await
            };

            let result = match cached {
                Some(cached) if !cached.games.is_empty() => cached,
                _ => {
                    let _ = fs::remove_file(&path).await;
                    fetch_period_until_nonempty(fide_id, period).await?
                }
            };

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

async fn fetch_period_until_nonempty(
    fide_id: &str,
    period: &str,
) -> Result<PeriodResult, Box<dyn Error>> {
    let url = CALC_URL
        .replacen("{id}", fide_id, 1)
        .replacen("{period}", period, 1);
    let html = fetch_text_with_retry(&url, |text| {
        parse_games(text)
            .map(|games| !games.is_empty())
            .unwrap_or(false)
    })
    .await?;
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

/// Wait, then GET `url`. On transport/HTTP failure or rejected body, double the wait
/// (until over 5s) and retry until `accept` returns true.
async fn fetch_text_with_retry(
    url: &str,
    mut accept: impl FnMut(&str) -> bool,
) -> Result<String, Box<dyn Error>> {
    let mut wait_millis = WAIT_MILLIS;
    loop {
        sleep(Duration::from_millis(wait_millis)).await;
        match try_fetch_text(url).await {
            Ok(text) if accept(&text) => return Ok(text),
            _ => {
                if wait_millis <= WAIT_DOUBLE_LIMIT_MILLIS {
                    wait_millis = wait_millis.saturating_mul(2);
                }
            }
        }
    }
}

async fn try_fetch_text(url: &str) -> Result<String, Box<dyn Error>> {
    let client = build_client()?;
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?)
}

async fn fetch_players(list: &str) -> Result<Vec<Player>, Box<dyn Error>> {
    let url = TOP_LIST_URL.replacen("{list}", list, 1);
    let html = fetch_text_with_retry(&url, |text| !parse_players(text).is_empty()).await?;
    Ok(parse_players(&html))
}

fn parse_players(html: &str) -> Vec<Player> {
    let Ok(re) = Regex::new(
        r#"(?s)<a href=/profile/(\d+)>([^<]+)</a>.*?<td class=rating_column>(\d+)</td>"#,
    ) else {
        return Vec::new();
    };

    re.captures_iter(html)
        .map(|caps| Player {
            fide_id: caps[1].to_string(),
            name: caps[2].to_string(),
            rating: caps[3].to_string(),
        })
        .collect()
}

/// Periods on the profile page where standard games (STD GMS) > 0.
async fn fetch_periods_with_standard_games(
    fide_id: &str,
) -> Result<HashSet<String>, Box<dyn Error>> {
    let url = PROFILE_CALC_URL.replacen("{id}", fide_id, 1);
    let html = fetch_text_with_retry(&url, |text| !text.trim().is_empty()).await?;
    parse_periods_with_standard_games(&html)
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
        let month = month_abbrev_to_number(&caps[2])
            .ok_or_else(|| format!("unknown month abbreviation on profile page: {}", &caps[2]))?;
        periods.insert(format!("{:04}-{:02}-01", caps[1].parse::<i32>()?, month));
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

    #[test]
    fn finds_all_consecutive_period_windows_with_enough_games() {
        let periods = vec![
            ("a".into(), vec![(2000.0, 1.0); 30]),
            ("b".into(), vec![(2000.0, 1.0); 30]),
            ("c".into(), vec![(2000.0, 1.0); 30]),
        ];
        let windows = consecutive_windows_with_at_least(&periods, 50);
        assert_eq!(windows.len(), 3);
        assert_eq!(
            (windows[0].start, windows[0].end, windows[0].games.len()),
            (0, 1, 60)
        );
        assert_eq!(
            (windows[1].start, windows[1].end, windows[1].games.len()),
            (0, 2, 90)
        );
        assert_eq!(
            (windows[2].start, windows[2].end, windows[2].games.len()),
            (1, 2, 60)
        );
    }

    #[test]
    fn omits_windows_that_start_or_end_on_empty_period() {
        let periods = vec![
            ("empty".into(), vec![]),
            ("a".into(), vec![(2000.0, 1.0); 30]),
            ("b".into(), vec![(2000.0, 1.0); 30]),
            ("also_empty".into(), vec![]),
        ];
        let windows = consecutive_windows_with_at_least(&periods, 50);
        assert_eq!(windows.len(), 1);
        assert_eq!(
            (windows[0].start, windows[0].end, windows[0].games.len()),
            (1, 2, 60)
        );
    }

    #[test]
    fn tpr_is_near_opponent_rating_for_even_score() {
        let games: Vec<RatedGame> = (0..50).map(|_| (2500.0, 0.5)).collect();
        let tpr = tournament_performance_rating(&games);
        assert!((tpr - 2500).abs() <= 1);
        assert!(rating_change(tpr as f64, &games, RATING_K).abs() < 0.5);
    }

    #[test]
    fn tpr_pads_virtual_draws_when_under_threshold() {
        let results = vec![PeriodResult {
            fide_id: "1".into(),
            period: "2025-09-01".into(),
            games: vec![Game {
                color: "white".into(),
                opponent_rating: "2400".into(),
                result: "1.00".into(),
            }],
        }];
        let report = tpr_report(&results, 2500.0).unwrap();
        assert_eq!(report.windows.len(), 1);
        assert_eq!(report.windows[0].games, 1);
    }
}
