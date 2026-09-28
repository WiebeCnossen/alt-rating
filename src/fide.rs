use crate::cache::{
    load_all_period_results, load_period_result, period_cache_path, remove_period_cache,
    save_period_result,
};
use crate::http::fetch_text_with_retry;
use crate::model::{Game, PeriodResult};
use regex::Regex;
use std::collections::HashSet;
use std::error::Error;

/// Profile calculations tab lists periods and game counts.
const PROFILE_CALC_URL: &str = "https://ratings.fide.com/profile/{id}/calculations";

/// Calculations page loads game rows via AJAX from this endpoint
/// (`calculations.phtml?id_number=…&period=…&rating=0`).
const CALC_URL: &str =
    "https://ratings.fide.com/a_indv_calculation.php?id_number={id}&rating_period={period}&t=0";

pub async fn periods_for_player(
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
                    remove_period_cache(&path).await;
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
