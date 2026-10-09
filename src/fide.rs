use crate::cache::{load_period_result, player_gms, remove_period_result, save_period_result};
use crate::http::{RetryLimitReached, TextFetchKind, fetch_text_with_retry};
use crate::model::{Game, PeriodResult};
use crate::rating_list::parse_period;
use regex::Regex;
use std::error::Error;

fn is_retry_limit(err: &(dyn Error + 'static)) -> bool {
    err.is::<RetryLimitReached>()
}

/// Used only as the AJAX referer for the period calculation endpoint.
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

/// Load period games for `fide_id`, using rating-list `Gms` as the expected
/// game count per period (no profile `/calculations` round-trip).
pub async fn periods_for_player(
    fide_id: &str,
    periods: &[String],
    max_attempts: u32,
    initial_wait_millis: u64,
) -> Result<PeriodsFetch, Box<dyn Error>> {
    let expected: Vec<u32> = periods
        .iter()
        .map(|period| expected_gms_from_rating_list(fide_id, period))
        .collect::<Result<_, _>>()?;

    let mut results = Vec::with_capacity(periods.len());
    let mut downloaded = false;

    for (period, &expected_gms) in periods.iter().zip(expected.iter()) {
        if let Some(cached) = load_period_result(fide_id, period) {
            if cached.games.len() as u32 == expected_gms {
                results.push(cached);
                continue;
            }
            let _ = remove_period_result(fide_id, period);
        }

        let result = if expected_gms == 0 {
            PeriodResult {
                fide_id: fide_id.to_string(),
                period: period.clone(),
                games: Vec::new(),
            }
        } else {
            match fetch_period_with_game_count(
                fide_id,
                period,
                expected_gms,
                max_attempts,
                initial_wait_millis,
            )
            .await
            {
                Ok(result) => {
                    downloaded = true;
                    result
                }
                Err(err) if is_retry_limit(err.as_ref()) => return Ok(PeriodsFetch::Incomplete),
                Err(err) => return Err(err),
            }
        };

        save_period_result(&result)?;
        results.push(result);
    }

    Ok(PeriodsFetch::Complete {
        results,
        downloaded,
    })
}

fn expected_gms_from_rating_list(fide_id: &str, period: &str) -> Result<u32, Box<dyn Error>> {
    let (year, month) = parse_period(period)?;
    player_gms(fide_id, year, month).ok_or_else(|| {
        format!("rating list {year:04}-{month:02} not in cache (needed for {fide_id} Gms)").into()
    })
}

async fn fetch_period_with_game_count(
    fide_id: &str,
    period: &str,
    expected_games: u32,
    max_attempts: u32,
    initial_wait_millis: u64,
) -> Result<PeriodResult, Box<dyn Error>> {
    let url = CALC_URL
        .replacen("{id}", fide_id, 1)
        .replacen("{period}", period, 1);
    let referer = PROFILE_CALC_URL.replacen("{id}", fide_id, 1);
    let html = fetch_text_with_retry(
        &url,
        max_attempts,
        initial_wait_millis,
        TextFetchKind::Ajax { referer: &referer },
        |text| {
            parse_games(text)
                .map(|games| games.len() as u32 == expected_games)
                .unwrap_or(false)
        },
    )
    .await?;
    let games = parse_games(&html)?;
    if games.len() as u32 != expected_games {
        return Err(format!(
            "period {period} for {fide_id}: expected {expected_games} games, got {}",
            games.len()
        )
        .into());
    }
    Ok(PeriodResult {
        fide_id: fide_id.to_string(),
        period: period.to_string(),
        games,
    })
}

pub fn parse_games(html: &str) -> Result<Vec<Game>, Box<dyn Error>> {
    // Game rows: color note + name, then rating cell (may include a "*" marker), fed, result.
    let re = Regex::new(
        r#"(?s)<span class="(white|black)_note">.*?</span>\s*([^<]+?)\s*</td>.*?<td\s+class="list4">(\d+)\b.*?</td>\s*<td\s+class="list4 table_scale">[A-Z]{3}</td>\s*<td\s+class=list4>(\d+\.\d+)</td>"#,
    )?;

    Ok(re
        .captures_iter(html)
        .map(|caps| Game {
            color: caps[1].to_string(),
            opponent_name: caps[2].trim().to_string(),
            opponent_rating: caps[3].to_string(),
            result: caps[4].to_string(),
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
        assert_eq!(games[0].opponent_name, "Firouzja, Alireza");
        assert_eq!(games[0].opponent_rating, "2759");
        assert_eq!(games[0].result, "0.00");
        assert_eq!(games[1].color, "white");
        assert_eq!(games[1].opponent_name, "Keymer, Vincent");
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
        assert_eq!(games[0].opponent_name, "Costanza, Mitchell");
        assert_eq!(games[0].opponent_rating, "2407");
        assert_eq!(games[0].result, "1.00");
        assert_eq!(games[1].color, "white");
        assert_eq!(games[1].opponent_name, "Villamil, Nahum Jose");
        assert_eq!(games[1].opponent_rating, "2407");
    }
}
