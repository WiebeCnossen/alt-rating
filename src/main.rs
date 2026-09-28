use chrono::{Datelike, Months, Utc};
use regex::Regex;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::error::Error;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;
use tokio::time::sleep;
use zip::ZipArchive;

/// Full standard rating list (zipped fixed-width text).
const STANDARD_RATING_LIST_URL: &str = "https://ratings.fide.com/download/standard_rating_list.zip";

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
/// Minimum actual games required for a player to appear in the output lists.
const MIN_GAMES_FOR_LIST: usize = 12;
/// Include players rated at least this many points below the group leader.
const TOP_RATING_BAND: u32 = 250;

/// Fixed-width columns in `standard_rating_list.txt`.
const COL_ID: std::ops::Range<usize> = 0..15;
const COL_NAME: std::ops::Range<usize> = 15..76;
const COL_SEX: usize = 80;
const COL_RATING: std::ops::Range<usize> = 113..119;
const COL_FLAG_START: usize = 132;

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
    let (men_players, women_players) = fetch_top_players().await?;

    let men = process_players("open", &men_players, &periods).await?;
    write_summary_csv(summary_csv_path("open"), &men).await?;

    let women = process_players("women", &women_players, &periods).await?;
    write_summary_csv(summary_csv_path("women"), &women).await?;

    Ok(())
}

async fn process_players(
    list: &str,
    players: &[Player],
    periods: &[String],
) -> Result<Vec<PlayerSummary>, Box<dyn Error>> {
    let mut summaries = Vec::with_capacity(players.len());

    println!("LIST\t{list}\t{}", players.len());
    for player in players {
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
        println!("{}\tTPR_RAW\t{}", player.name, report.raw_games_tpr);

        if report.total_games < MIN_GAMES_FOR_LIST {
            continue;
        }

        summaries.push(PlayerSummary {
            name: player.name.clone(),
            rating: player.rating.clone(),
            max_tpr: report.max_tpr,
            all_games_tpr: report.all_games_tpr,
            raw_games_tpr: report.raw_games_tpr,
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
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            s.name,
            s.max_tpr,
            s.all_games_tpr,
            s.raw_games_tpr,
            s.rating,
            s.diff(),
            s.total_games
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
    let mut out = String::from("name,tpr_max,tpr_all,tpr_raw,rating,diff,games\n");
    for s in summaries {
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            csv_escape(&s.name),
            s.max_tpr,
            s.all_games_tpr,
            s.raw_games_tpr,
            csv_escape(&s.rating),
            s.diff(),
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
    rating: String,
    max_tpr: i32,
    all_games_tpr: i32,
    raw_games_tpr: i32,
    total_games: usize,
}

impl PlayerSummary {
    fn diff(&self) -> i32 {
        self.max_tpr - self.rating.parse::<i32>().unwrap_or(0)
    }
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
    raw_games_tpr: i32,
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
    let actual_games: Vec<RatedGame> = period_games
        .iter()
        .flat_map(|(_, g)| g.iter().copied())
        .collect();
    let raw_games_tpr = tournament_performance_rating(&actual_games);

    let mut all_games = actual_games;
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
        raw_games_tpr,
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

async fn try_fetch_bytes(url: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let client = build_client()?;
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec())
}

/// Download the standard rating list and return the top active men and women.
async fn fetch_top_players() -> Result<(Vec<Player>, Vec<Player>), Box<dyn Error>> {
    let mut wait_millis = WAIT_MILLIS;
    loop {
        sleep(Duration::from_millis(wait_millis)).await;
        match try_fetch_bytes(STANDARD_RATING_LIST_URL).await {
            Ok(bytes) => match parse_top_players_from_zip(&bytes) {
                Ok(players) => return Ok(players),
                Err(_) => {
                    if wait_millis <= WAIT_DOUBLE_LIMIT_MILLIS {
                        wait_millis = wait_millis.saturating_mul(2);
                    }
                }
            },
            Err(_) => {
                if wait_millis <= WAIT_DOUBLE_LIMIT_MILLIS {
                    wait_millis = wait_millis.saturating_mul(2);
                }
            }
        }
    }
}

fn parse_top_players_from_zip(bytes: &[u8]) -> Result<(Vec<Player>, Vec<Player>), Box<dyn Error>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut file = archive.by_index(0)?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw);
    parse_top_players_from_rating_list(&text)
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
        let player = Player {
            fide_id: line[COL_ID].trim().to_string(),
            name: line[COL_NAME].trim().to_string(),
            rating: rating.to_string(),
        };
        match sex {
            b'M' => men.push(player),
            b'F' => women.push(player),
            _ => {}
        }
    }

    men.sort_by(|a, b| {
        b.rating
            .parse::<u32>()
            .unwrap_or(0)
            .cmp(&a.rating.parse::<u32>().unwrap_or(0))
    });
    women.sort_by(|a, b| {
        b.rating
            .parse::<u32>()
            .unwrap_or(0)
            .cmp(&a.rating.parse::<u32>().unwrap_or(0))
    });
    retain_within_rating_band(&mut men);
    retain_within_rating_band(&mut women);

    if men.is_empty() || women.is_empty() {
        return Err("rating list did not yield players".into());
    }
    Ok((men, women))
}

fn retain_within_rating_band(players: &mut Vec<Player>) {
    let Some(top) = players.first().and_then(|p| p.rating.parse::<u32>().ok()) else {
        return;
    };
    let floor = top.saturating_sub(TOP_RATING_BAND);
    players.retain(|p| p.rating.parse::<u32>().unwrap_or(0) >= floor);
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

    fn rating_list_line(id: &str, name: &str, sex: char, rating: u32, flag: &str) -> String {
        let mut line = vec![b' '; 136];
        let put = |line: &mut [u8], start: usize, value: &str| {
            let bytes = value.as_bytes();
            line[start..start + bytes.len()].copy_from_slice(bytes);
        };
        put(&mut line, 0, id);
        put(&mut line, 15, name);
        line[80] = sex as u8;
        put(&mut line, 113, &format!("{rating}"));
        put(&mut line, 132, flag);
        String::from_utf8(line).unwrap()
    }

    #[test]
    fn parses_rating_list_skipping_inactive_and_taking_top_by_sex() {
        let text = [
            "ID Number      Name                                                         Fed Sex Tit  WTit OTit           FOA SEP26 Gms K  B-day Flag".to_string(),
            rating_list_line("1", "Low, Man", 'M', 2000, ""),
            rating_list_line("2", "Top, Man", 'M', 2800, ""),
            rating_list_line("3", "Inactive, Man", 'M', 2900, "i"),
            rating_list_line("4", "Near, Man", 'M', 2550, ""),
            rating_list_line("5", "Top, Woman", 'F', 2500, "w"),
            rating_list_line("6", "Inactive, Woman", 'F', 2600, "wi"),
            rating_list_line("7", "Second, Woman", 'F', 2400, ""),
            rating_list_line("8", "Far, Woman", 'F', 2200, ""),
        ]
        .join("\n");

        let (men, women) = parse_top_players_from_rating_list(&text).unwrap();
        // Men: top 2800 => floor 2550; include 2800 and 2550, not 2000.
        assert_eq!(men.len(), 2);
        assert_eq!(men[0].fide_id, "2");
        assert_eq!(men[1].fide_id, "4");
        assert!(men.iter().all(|p| p.fide_id != "3"));
        // Women: top 2500 => floor 2250; include 2500 and 2400, not 2200.
        assert_eq!(women.len(), 2);
        assert_eq!(women[0].fide_id, "5");
        assert_eq!(women[1].fide_id, "7");
        assert!(women.iter().all(|p| p.fide_id != "6"));
    }
}
