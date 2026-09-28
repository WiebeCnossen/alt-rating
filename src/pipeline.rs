use crate::fide::periods_for_player;
use crate::model::{Player, PlayerSummary};
use crate::output::print_summary_table;
use crate::tpr::{summary_from_report, tpr_report};
use chrono::{Datelike, Months, Utc};
use std::error::Error;

/// Minimum actual games required for a player to appear in the output lists.
const MIN_GAMES_FOR_LIST: usize = 12;

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

        summaries.push(summary_from_report(
            player.name.clone(),
            player.rating.clone(),
            &report,
        ));
    }

    summaries.sort_by(|a, b| {
        b.max_tpr
            .cmp(&a.max_tpr)
            .then_with(|| b.all_games_tpr.cmp(&a.all_games_tpr))
    });

    print_summary_table(&summaries);

    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_twelve_complete_months_are_ordered() {
        let periods = last_12_complete_months();
        assert_eq!(periods.len(), 12);
        assert!(periods[0].ends_with("-01"));
        assert_eq!(&periods[0][8..], "01");
        assert!(periods[0] < periods[11]);
    }
}
