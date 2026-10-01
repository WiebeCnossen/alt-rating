use crate::fide::{PeriodsFetch, periods_for_player};
use crate::model::{IncompletePlayer, Player, PlayerSummary};
use crate::tpr::{MIN_GAMES, summary_from_report, tpr_report};
use chrono::{Datelike, Months, Utc};
use std::error::Error;

/// Once the TPR list has this many players, cut off anyone more than
/// [`TPR_LIST_BAND`] below the player in this position (1-based).
const TPR_LIST_ANCHOR_RANK: usize = 30;

/// Include players whose ELO_YEAR is at least this many points below the anchor.
const TPR_LIST_BAND: i32 = 150;

/// Default tighter band once any player is incomplete, so missing players do not pull the floor down as far.
pub const TPR_LIST_BAND_INCOMPLETE: i32 = 0;

pub struct ProcessOutcome {
    pub summaries: Vec<PlayerSummary>,
    pub incomplete: Vec<IncompletePlayer>,
}

pub fn last_12_complete_months() -> Vec<String> {
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

pub async fn process_players(
    list: &str,
    players: &[Player],
    periods: &[String],
    max_attempts: u32,
    initial_wait_millis: u64,
    band_incomplete: i32,
) -> Result<ProcessOutcome, Box<dyn Error>> {
    let mut summaries = Vec::new();
    let mut incomplete = Vec::new();
    let mut anchor_elo_year: Option<i32> = None;
    let mut rating_floor: Option<i32> = None;

    println!("LIST\t{list}\t{}", players.len());
    for player in players {
        if let Some(floor) = rating_floor {
            let rating = player.rating.parse::<i32>().unwrap_or(0);
            if rating < floor {
                println!(
                    "STOP\t{}\trating {} below ELO_YEAR floor {}",
                    list, player.rating, floor
                );
                break;
            }
        }

        println!("{}\t{}\t{}", player.name, player.fide_id, player.rating);

        let results = match periods_for_player(
            &player.fide_id,
            periods,
            player.recent_games,
            max_attempts,
            initial_wait_millis,
        )
        .await?
        {
            PeriodsFetch::Complete {
                results,
                downloaded,
            } => {
                if downloaded {
                    println!("COMPLETE\t{}\t{}", player.name, player.fide_id);
                }
                results
            }
            PeriodsFetch::Incomplete => {
                println!("INCOMPLETE\t{}\t{}", player.name, player.fide_id);
                incomplete.push(IncompletePlayer {
                    name: player.name.clone(),
                    fide_id: player.fide_id.clone(),
                    recent_games: player.recent_games,
                });
                // Tighten the scan once the 30th exists; keep summaries so the
                // list can still grow toward the anchor for cutoff purposes.
                if let Some(tight_floor) = tpr_list_floor(&summaries, band_incomplete)
                    .or_else(|| anchor_elo_year.map(|a| a - band_incomplete))
                {
                    rating_floor = Some(rating_floor.map_or(tight_floor, |f| f.max(tight_floor)));
                    retain_within_tpr_band(&mut summaries, band_incomplete);
                }
                continue;
            }
        };

        let total_games: usize = results.iter().map(|r| r.games.len()).sum();
        if total_games < MIN_GAMES {
            println!(
                "SKIP\t{}\t{}\tfewer than {} rated games in last {} periods ({})",
                player.name,
                player.fide_id,
                MIN_GAMES,
                periods.len(),
                total_games
            );
            continue;
        }

        let player_rating: f64 = player.rating.parse()?;
        let report = tpr_report(&results, player_rating)?;
        let print_outcome = incomplete.is_empty();
        if print_outcome {
            for window in &report.windows {
                println!(
                    "TPR\t{}\t{}\t{}\t{}",
                    window.start_period, window.end_period, window.games, window.tpr
                );
            }
            println!("{}\tELO_YEAR\t{}", player.name, report.elo_year);
            println!(
                "{}\tTPR_ALL\t{}\t{}",
                player.name, report.all_games_tpr, report.total_games
            );
            println!("{}\tTPR_RAW\t{}", player.name, report.raw_games_tpr);
        }

        summaries.push(summary_from_report(
            player.name.clone(),
            player.rating.clone(),
            &report,
        ));
        sort_summaries_by_tpr(&mut summaries);
        let band = if incomplete.is_empty() {
            TPR_LIST_BAND
        } else {
            band_incomplete
        };
        retain_within_tpr_band(&mut summaries, band);
        rating_floor = tpr_list_floor(&summaries, band);
        maybe_print_new_anchor(list, &summaries, &mut anchor_elo_year, print_outcome);
    }

    if !incomplete.is_empty() {
        summaries.clear();
    }

    Ok(ProcessOutcome {
        summaries,
        incomplete,
    })
}

/// Retry period fetches for incomplete players until every one is complete.
pub async fn complete_incomplete_players(
    players: &[IncompletePlayer],
    periods: &[String],
    max_attempts: u32,
    initial_wait_millis: u64,
) -> Result<(), Box<dyn Error>> {
    let mut remaining: Vec<IncompletePlayer> = players.to_vec();
    while !remaining.is_empty() {
        println!("INCOMPLETE_RETRY\t{}", remaining.len());
        let mut still_incomplete = Vec::new();
        for player in &remaining {
            println!("{}\t{}\tretry", player.name, player.fide_id);
            match periods_for_player(
                &player.fide_id,
                periods,
                player.recent_games,
                max_attempts,
                initial_wait_millis,
            )
            .await?
            {
                PeriodsFetch::Complete { .. } => {
                    println!("COMPLETE\t{}\t{}", player.name, player.fide_id);
                }
                PeriodsFetch::Incomplete => {
                    println!("INCOMPLETE\t{}\t{}", player.name, player.fide_id);
                    still_incomplete.push(player.clone());
                }
            }
        }
        remaining = still_incomplete;
    }
    Ok(())
}

fn sort_summaries_by_tpr(summaries: &mut [PlayerSummary]) {
    summaries.sort_by(|a, b| {
        b.elo_year
            .cmp(&a.elo_year)
            .then_with(|| b.all_games_tpr.cmp(&a.all_games_tpr))
    });
}

/// ELO_YEAR at the anchor rank, if the list is long enough.
fn tpr_list_anchor(summaries: &[PlayerSummary]) -> Option<i32> {
    summaries.get(TPR_LIST_ANCHOR_RANK - 1).map(|s| s.elo_year)
}

/// Floor ELO_YEAR for list membership once the anchor rank is filled.
fn tpr_list_floor(summaries: &[PlayerSummary], band: i32) -> Option<i32> {
    Some(tpr_list_anchor(summaries)? - band)
}

fn maybe_print_new_anchor(
    list: &str,
    summaries: &[PlayerSummary],
    previous: &mut Option<i32>,
    print: bool,
) {
    let Some(elo_year) = tpr_list_anchor(summaries) else {
        return;
    };
    if *previous == Some(elo_year) {
        return;
    }
    *previous = Some(elo_year);
    if !print {
        return;
    }
    let holder = &summaries[TPR_LIST_ANCHOR_RANK - 1];
    println!(
        "ANCHOR\t{}\t{}\t{}\tELO_YEAR\t{}",
        list, TPR_LIST_ANCHOR_RANK, holder.name, elo_year
    );
}

/// Keep everyone until there are [`TPR_LIST_ANCHOR_RANK`] players; then keep
/// those within `band` of the anchor's ELO_YEAR.
fn retain_within_tpr_band(summaries: &mut Vec<PlayerSummary>, band: i32) {
    let Some(floor) = tpr_list_floor(summaries, band) else {
        return;
    };
    summaries.retain(|s| s.elo_year >= floor);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: &str, elo_year: i32) -> PlayerSummary {
        PlayerSummary {
            name: name.into(),
            rating: "2500".into(),
            elo_year,
            all_games_tpr: elo_year,
            raw_games_tpr: elo_year,
            total_games: 50,
        }
    }

    #[test]
    fn last_twelve_complete_months_are_ordered() {
        let periods = last_12_complete_months();
        assert_eq!(periods.len(), 12);
        assert!(periods[0].ends_with("-01"));
        assert_eq!(&periods[0][8..], "01");
        assert!(periods[0] < periods[11]);
    }

    #[test]
    fn keeps_everyone_before_anchor_rank_is_filled() {
        let mut summaries: Vec<_> = (0..29)
            .map(|i| summary(&format!("p{i}"), 2800 - i))
            .collect();
        retain_within_tpr_band(&mut summaries, TPR_LIST_BAND);
        assert_eq!(summaries.len(), 29);
    }

    #[test]
    fn cuts_off_below_anchor_tpr_minus_band() {
        // 30 players at 2700, plus one at 2550 (kept) and one at 2549 (below 2700-150).
        let mut summaries: Vec<_> = (0..30).map(|i| summary(&format!("top{i}"), 2700)).collect();
        summaries.push(summary("edge", 2550));
        summaries.push(summary("below", 2549));
        sort_summaries_by_tpr(&mut summaries);
        retain_within_tpr_band(&mut summaries, TPR_LIST_BAND);
        assert_eq!(summaries.len(), 31);
        assert!(summaries.iter().any(|s| s.name == "edge"));
        assert!(summaries.iter().all(|s| s.name != "below"));
        assert_eq!(tpr_list_floor(&summaries, TPR_LIST_BAND), Some(2550));
    }

    #[test]
    fn incomplete_band_keeps_a_higher_floor() {
        let mut summaries: Vec<_> = (0..30).map(|i| summary(&format!("top{i}"), 2700)).collect();
        sort_summaries_by_tpr(&mut summaries);
        assert_eq!(tpr_list_floor(&summaries, TPR_LIST_BAND), Some(2550));
        assert_eq!(
            tpr_list_floor(&summaries, TPR_LIST_BAND_INCOMPLETE),
            Some(2700 - TPR_LIST_BAND_INCOMPLETE)
        );
    }

    #[test]
    fn no_floor_before_anchor_rank() {
        let summaries: Vec<_> = (0..29)
            .map(|i| summary(&format!("p{i}"), 2800 - i))
            .collect();
        assert_eq!(tpr_list_floor(&summaries, TPR_LIST_BAND), None);
        assert_eq!(tpr_list_floor(&summaries, TPR_LIST_BAND_INCOMPLETE), None);
    }

    /// Incomplete before the 30th must not invent a floor from that player's rating.
    #[test]
    fn incomplete_floor_requires_anchor() {
        let summaries: Vec<_> = (0..10)
            .map(|i| summary(&format!("p{i}"), 2800 - i))
            .collect();
        let anchor_elo_year: Option<i32> = None;
        let floor = tpr_list_floor(&summaries, TPR_LIST_BAND_INCOMPLETE)
            .or_else(|| anchor_elo_year.map(|a| a - TPR_LIST_BAND_INCOMPLETE));
        assert_eq!(floor, None);
    }

    #[test]
    fn incomplete_floor_uses_saved_anchor() {
        let anchor_elo_year = Some(2700);
        let summaries: Vec<PlayerSummary> = Vec::new();
        let floor = tpr_list_floor(&summaries, TPR_LIST_BAND_INCOMPLETE)
            .or_else(|| anchor_elo_year.map(|a| a - TPR_LIST_BAND_INCOMPLETE));
        assert_eq!(floor, Some(2700 - TPR_LIST_BAND_INCOMPLETE));
    }

    #[test]
    fn incomplete_band_still_grows_list_toward_anchor() {
        // With incompletes present we use the tighter band, but summaries are kept
        // so the list can still reach the 30th for cutoff.
        let mut summaries: Vec<_> = (0..29)
            .map(|i| summary(&format!("p{i}"), 2800 - i as i32))
            .collect();
        assert_eq!(tpr_list_floor(&summaries, TPR_LIST_BAND_INCOMPLETE), None);
        summaries.push(summary("p29", 2771));
        sort_summaries_by_tpr(&mut summaries);
        retain_within_tpr_band(&mut summaries, TPR_LIST_BAND_INCOMPLETE);
        assert_eq!(summaries.len(), 30);
        assert_eq!(
            tpr_list_floor(&summaries, TPR_LIST_BAND_INCOMPLETE),
            Some(2771 - TPR_LIST_BAND_INCOMPLETE)
        );
    }
}
