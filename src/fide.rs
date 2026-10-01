use crate::cache::{
    load_cached_period_results, load_period_result, period_cache_path, remove_period_cache,
    save_period_result,
};
use crate::http::{RetryLimitReached, fetch_text_with_retry};
use crate::model::{Game, PeriodResult};
use regex::Regex;
use std::collections::HashSet;
use std::error::Error;

fn is_retry_limit(err: &Box<dyn Error>) -> bool {
    err.is::<RetryLimitReached>()
}

/// Profile calculations tab lists periods and game counts.
const PROFILE_CALC_URL: &str = "https://ratings.fide.com/profile/{id}/calculations";

/// Calculations page loads game rows via AJAX from this endpoint
/// (`calculations.phtml?id_number=…&period=…&rating=0`).
const CALC_URL: &str =
    "https://ratings.fide.com/a_indv_calculation.php?id_number={id}&rating_period={period}&t=0";

/// Outcome of loading a player's period games.
pub enum PeriodsFetch {
    /// All required periods were loaded (cached and/or fetched).
    Complete {
        results: Vec<PeriodResult>,
        /// At least one period's rated games were downloaded from FIDE.
        downloaded: bool,
    },
    /// A required period fetch exhausted retries; nothing was cached for that miss.
    Incomplete,
}

pub async fn periods_for_player(
    fide_id: &str,
    periods: &[String],
    recent_month_games: u32,
    max_attempts: u32,
) -> Result<PeriodsFetch, Box<dyn Error>> {
    let cached = load_cached_period_results(fide_id, periods).await;
    let missing: Vec<usize> = cached
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.is_none().then_some(i))
        .collect();

    if missing.is_empty() {
        let results: Vec<PeriodResult> = cached.into_iter().map(Option::unwrap).collect();
        if results.iter().all(|r| r.games.is_empty()) {
            return fetch_periods_using_profile(fide_id, periods, true, max_attempts).await;
        }
        return Ok(PeriodsFetch::Complete {
            results,
            downloaded: false,
        });
    }

    // Rating-list `Gms` covers the latest period: fill or fetch only that month
    // when every older period is already cached.
    if missing.as_slice() == [periods.len().saturating_sub(1)] && !periods.is_empty() {
        let period = &periods[periods.len() - 1];
        let path = period_cache_path(fide_id, period);
        let (latest, downloaded) = if recent_month_games == 0 {
            (
                PeriodResult {
                    fide_id: fide_id.to_string(),
                    period: period.clone(),
                    games: Vec::new(),
                },
                false,
            )
        } else {
            match fetch_period_until_nonempty(fide_id, period, max_attempts).await {
                Ok(result) => (result, true),
                Err(err) if is_retry_limit(&err) => return Ok(PeriodsFetch::Incomplete),
                Err(err) => return Err(err),
            }
        };
        save_period_result(&path, &latest).await?;

        let mut results: Vec<PeriodResult> = cached
            .into_iter()
            .take(periods.len() - 1)
            .map(|r| r.expect("older periods were cached"))
            .collect();
        results.push(latest);
        return Ok(PeriodsFetch::Complete {
            results,
            downloaded,
        });
    }

    fetch_periods_using_profile(fide_id, periods, false, max_attempts).await
}

async fn fetch_periods_using_profile(
    fide_id: &str,
    periods: &[String],
    force_refresh: bool,
    max_attempts: u32,
) -> Result<PeriodsFetch, Box<dyn Error>> {
    let active = match fetch_periods_with_standard_games(fide_id, max_attempts).await {
        Ok(active) => active,
        Err(err) if is_retry_limit(&err) => return Ok(PeriodsFetch::Incomplete),
        Err(err) => return Err(err),
    };
    let mut results = Vec::with_capacity(periods.len());
    let mut downloaded = false;

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
                    remove_period_cache(&path).await;
                    match fetch_period_until_nonempty(fide_id, period, max_attempts).await {
                        Ok(result) => {
                            downloaded = true;
                            result
                        }
                        Err(err) if is_retry_limit(&err) => {
                            return Ok(PeriodsFetch::Incomplete);
                        }
                        Err(err) => return Err(err),
                    }
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

    Ok(PeriodsFetch::Complete {
        results,
        downloaded,
    })
}

async fn fetch_period_until_nonempty(
    fide_id: &str,
    period: &str,
    max_attempts: u32,
) -> Result<PeriodResult, Box<dyn Error>> {
    let url = CALC_URL
        .replacen("{id}", fide_id, 1)
        .replacen("{period}", period, 1);
    let html = fetch_text_with_retry(&url, max_attempts, |text| {
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

/// Periods on the profile page where standard games (STD GMS) > 0.
async fn fetch_periods_with_standard_games(
    fide_id: &str,
    max_attempts: u32,
) -> Result<HashSet<String>, Box<dyn Error>> {
    let url = PROFILE_CALC_URL.replacen("{id}", fide_id, 1);
    let html = fetch_text_with_retry(&url, max_attempts, |text| !text.trim().is_empty()).await?;
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

pub fn parse_games(html: &str) -> Result<Vec<Game>, Box<dyn Error>> {
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
}
