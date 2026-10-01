use crate::fide::{periods_for_player, PeriodsFetch};
use crate::model::{IncompletePlayer, Player, PlayerSummary};
use crate::tpr::{summary_from_report, tpr_report};
use chrono::{Datelike, Months, Utc};
use std::error::Error;

/// Minimum actual games required for a player to appear in the output lists.
const MIN_GAMES_FOR_LIST: usize = 12;

/// Once the TPR list has this many players, cut off anyone more than
/// [`TPR_LIST_BAND`] below the player in this position (1-based).
const TPR_LIST_ANCHOR_RANK: usize = 30;

/// Include players whose ELO_YEAR is at least this many points below the anchor.
const TPR_LIST_BAND: i32 = 150;

/// Tighter band once any player is incomplete, so missing players do not pull the floor down as far.
const TPR_LIST_BAND_INCOMPLETE: i32 = 50;

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

        let results =
            match periods_for_player(&player.fide_id, periods, player.recent_games, max_attempts)
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
                    });
                    let tight_floor = tpr_list_floor(&summaries, TPR_LIST_BAND_INCOMPLETE)
                        .or_else(|| anchor_elo_year.map(|a| a - TPR_LIST_BAND_INCOMPLETE))
                        .unwrap_or_else(|| {
                            player.rating.parse::<i32>().unwrap_or(0) - TPR_LIST_BAND_INCOMPLETE
                        });
                    rating_floor = Some(rating_floor.map_or(tight_floor, |f| f.max(tight_floor)));
                    summaries.clear();
                    continue;
                }
            };

        // Once anyone is incomplete, keep scanning for more incompletes only.
        if !incomplete.is_empty() {
            continue;
        }

        if results.iter().all(|r| r.games.is_empty()) {
            println!(
                "SKIP\t{}\t{}\tno rated games in last {} periods",
                player.name,
                player.fide_id,
                periods.len()
            );
            continue;
        }

        let player_rating: f64 = player.rating.parse()?;
        let report = tpr_report(&results, player_rating)?;
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

        if report.total_games < MIN_GAMES_FOR_LIST {
            continue;
        }

        summaries.push(summary_from_report(
            player.name.clone(),
            player.rating.clone(),
            &report,
        ));
        sort_summaries_by_tpr(&mut summaries);
        retain_within_tpr_band(&mut summaries, TPR_LIST_BAND);
        rating_floor = tpr_list_floor(&summaries, TPR_LIST_BAND);
        maybe_print_new_anchor(list, &summaries, &mut anchor_elo_year);
    }

    if !incomplete.is_empty() {
        summaries.clear();
    }

    Ok(ProcessOutcome {
        summaries,
        incomplete,
    })
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

fn maybe_print_new_anchor(list: &str, summaries: &[PlayerSummary], previous: &mut Option<i32>) {
    let Some(elo_year) = tpr_list_anchor(summaries) else {
        return;
    };
    if *previous == Some(elo_year) {
        return;
    }
    *previous = Some(elo_year);
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
            Some(2650)
        );
    }
}
