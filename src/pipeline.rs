use crate::fide::periods_for_player;
use crate::model::{Player, PlayerSummary};
use crate::output::print_summary_table;
use crate::tpr::{summary_from_report, tpr_report};
use chrono::{Datelike, Months, Utc};
use std::error::Error;

/// Minimum actual games required for a player to appear in the output lists.
const MIN_GAMES_FOR_LIST: usize = 12;

/// Once the TPR list has this many players, cut off anyone more than
/// [`TPR_LIST_BAND`] below the player in this position (1-based).
const TPR_LIST_ANCHOR_RANK: usize = 30;

/// Include players whose TPR_MAX is at least this many points below the anchor.
const TPR_LIST_BAND: i32 = 150;

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
) -> Result<Vec<PlayerSummary>, Box<dyn Error>> {
    let mut summaries = Vec::new();

    println!("LIST\t{list}\t{}", players.len());
    for player in players {
        if let Some(floor) = tpr_list_floor(&summaries) {
            let rating = player.rating.parse::<i32>().unwrap_or(0);
            if rating < floor {
                println!(
                    "STOP\t{}\trating {} below TPR floor {}",
                    list, player.rating, floor
                );
                break;
            }
        }

        println!("{}\t{}\t{}", player.name, player.fide_id, player.rating);

        let results = periods_for_player(&player.fide_id, periods).await?;
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
        println!("{}\tTPR_MAX\t{}", player.name, report.max_tpr);
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
        retain_within_tpr_band(&mut summaries);
    }

    print_summary_table(&summaries);

    Ok(summaries)
}

fn sort_summaries_by_tpr(summaries: &mut [PlayerSummary]) {
    summaries.sort_by(|a, b| {
        b.max_tpr
            .cmp(&a.max_tpr)
            .then_with(|| b.all_games_tpr.cmp(&a.all_games_tpr))
    });
}

/// Floor TPR_MAX for list membership once the anchor rank is filled.
fn tpr_list_floor(summaries: &[PlayerSummary]) -> Option<i32> {
    if summaries.len() < TPR_LIST_ANCHOR_RANK {
        return None;
    }
    Some(summaries[TPR_LIST_ANCHOR_RANK - 1].max_tpr - TPR_LIST_BAND)
}

/// Keep everyone until there are [`TPR_LIST_ANCHOR_RANK`] players; then keep
/// those within [`TPR_LIST_BAND`] of the anchor's TPR_MAX.
fn retain_within_tpr_band(summaries: &mut Vec<PlayerSummary>) {
    let Some(floor) = tpr_list_floor(summaries) else {
        return;
    };
    summaries.retain(|s| s.max_tpr >= floor);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: &str, max_tpr: i32) -> PlayerSummary {
        PlayerSummary {
            name: name.into(),
            rating: "2500".into(),
            max_tpr,
            all_games_tpr: max_tpr,
            raw_games_tpr: max_tpr,
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
        retain_within_tpr_band(&mut summaries);
        assert_eq!(summaries.len(), 29);
    }

    #[test]
    fn cuts_off_below_anchor_tpr_minus_band() {
        // 30 players at 2700, plus one at 2550 (kept) and one at 2549 (below 2700-150).
        let mut summaries: Vec<_> = (0..30).map(|i| summary(&format!("top{i}"), 2700)).collect();
        summaries.push(summary("edge", 2550));
        summaries.push(summary("below", 2549));
        sort_summaries_by_tpr(&mut summaries);
        retain_within_tpr_band(&mut summaries);
        assert_eq!(summaries.len(), 31);
        assert!(summaries.iter().any(|s| s.name == "edge"));
        assert!(summaries.iter().all(|s| s.name != "below"));
        assert_eq!(tpr_list_floor(&summaries), Some(2550));
    }
}
