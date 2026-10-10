use crate::bonus::Top30ByMonth;
use crate::cache::player_gms_sum;
use crate::fide::{PeriodsFetch, periods_for_player};
use crate::model::{IncompletePlayer, Player, PlayerSummary};
use crate::rating_list::parse_period;
use crate::tpr::{MIN_GAMES, summary_from_report, tpr_report};
use chrono::{Datelike, Months, Utc};
use std::error::Error;

/// Once the TPR list has this many players, cut off anyone more than
/// [`TPR_LIST_BAND`] below the player in this position (1-based).
const TPR_LIST_ANCHOR_RANK: usize = 30;

/// Include players whose ELO_YEAR is at least this many points below the anchor.
const TPR_LIST_BAND: i32 = 150;

/// Default TPR band once any player is incomplete, so missing players do not pull the floor down as far.
pub const TPR_LIST_BAND_INCOMPLETE: i32 = 150;

/// Assumed anchor ELO_YEAR for the open list before the 30th place exists.
const PROVISIONAL_ANCHOR_OPEN: i32 = 2750;

/// Assumed anchor ELO_YEAR for the women's list before the 30th place exists.
const PROVISIONAL_ANCHOR_WOMEN: i32 = 2500;

pub struct ProcessOutcome {
    pub summaries: Vec<PlayerSummary>,
    pub incomplete: Vec<IncompletePlayer>,
    /// Incomplete players skipped because `--ignore-incomplete` was set.
    pub ignored_incomplete: Vec<IncompletePlayer>,
    /// FIDE rating floor used to stop scanning the list, if reached.
    pub cutoff_rating: Option<i32>,
}

pub struct ProcessPlayersArgs<'a> {
    pub list: &'a str,
    pub players: &'a [Player],
    pub periods: &'a [String],
    pub top30: &'a Top30ByMonth,
    pub max_attempts: u32,
    pub initial_wait_millis: u64,
    pub band_incomplete: i32,
    pub ignore_incomplete: bool,
}

/// Last 12 FIDE rating periods ending at the current calendar month.
///
/// Period `YYYY-MM-01` means games rated into the ratings list **as of** that
/// date (not games played during that month). Once the current month's list is
/// published, that period belongs in the window.
pub fn last_12_rating_periods() -> Vec<String> {
    let today = Utc::now().date_naive();
    let first_of_this_month =
        chrono::NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();

    (0..12)
        .rev()
        .map(|i| {
            let d = first_of_this_month - Months::new(i);
            format!("{:04}-{:02}-01", d.year(), d.month())
        })
        .collect()
}

pub async fn process_players(
    args: ProcessPlayersArgs<'_>,
) -> Result<ProcessOutcome, Box<dyn Error>> {
    let ProcessPlayersArgs {
        list,
        players,
        periods,
        top30,
        max_attempts,
        initial_wait_millis,
        band_incomplete,
        ignore_incomplete,
    } = args;

    let mut summaries = Vec::new();
    let mut incomplete = Vec::new();
    let mut ignored_incomplete = Vec::new();
    let mut anchor_elo_year: Option<i32> = None;
    let mut rating_floor = tpr_list_floor(&summaries, TPR_LIST_BAND, list, anchor_elo_year);

    println!("LIST\t{list}\t{}", players.len());
    for player in players {
        let rating = player.rating.parse::<i32>().unwrap_or(0);
        if rating < rating_floor {
            println!(
                "STOP\t{}\trating {} below ELO_YEAR floor {}",
                list, player.rating, rating_floor
            );
            break;
        }

        println!("{}\t{}\t{}", player.name, player.fide_id, player.rating);

        let listed_games = listed_games_in_periods(&player.fide_id, periods)?;
        if (listed_games as usize) < MIN_GAMES {
            println!(
                "SKIP\t{}\t{}\tfewer than {} rated games on rating lists in last {} periods ({})",
                player.name,
                player.fide_id,
                MIN_GAMES,
                periods.len(),
                listed_games
            );
            continue;
        }

        let results =
            match periods_for_player(&player.fide_id, periods, max_attempts, initial_wait_millis)
                .await?
            {
                PeriodsFetch::Complete {
                    results,
                    downloaded,
                    progress,
                } => {
                    if downloaded {
                        println!(
                            "COMPLETE\t{}\t{}\tperiods\t{}/{}\tgames\t{}/{}",
                            player.name,
                            player.fide_id,
                            progress.periods_done,
                            progress.periods_total,
                            progress.games_done,
                            progress.games_total
                        );
                    }
                    results
                }
                PeriodsFetch::Incomplete { progress } if ignore_incomplete => {
                    println!(
                        "SKIP\t{}\t{}\tincomplete period fetch\tperiods\t{}/{}\tgames\t{}/{}",
                        player.name,
                        player.fide_id,
                        progress.periods_done,
                        progress.periods_total,
                        progress.games_done,
                        progress.games_total
                    );
                    ignored_incomplete.push(IncompletePlayer {
                        name: player.name.clone(),
                        fide_id: player.fide_id.clone(),
                        rating: player.rating.clone(),
                    });
                    continue;
                }
                PeriodsFetch::Incomplete { progress } => {
                    println!(
                        "INCOMPLETE\t{}\t{}\tperiods\t{}/{}\tgames\t{}/{}",
                        player.name,
                        player.fide_id,
                        progress.periods_done,
                        progress.periods_total,
                        progress.games_done,
                        progress.games_total
                    );
                    incomplete.push(IncompletePlayer {
                        name: player.name.clone(),
                        fide_id: player.fide_id.clone(),
                        rating: player.rating.clone(),
                    });
                    // Tighten the scan; keep summaries so the list can still
                    // grow toward a real 30th for cutoff purposes.
                    let tight_floor =
                        tpr_list_floor(&summaries, band_incomplete, list, anchor_elo_year);
                    rating_floor = rating_floor.max(tight_floor);
                    retain_within_tpr_band(&mut summaries, band_incomplete, list, anchor_elo_year);
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
        let report = tpr_report(&results, player_rating, top30)?;
        let print_outcome = incomplete.is_empty();
        if print_outcome {
            for window in &report.windows {
                println!(
                    "TPR\t{}\t{}\t{}\t{}",
                    window.start_period, window.end_period, window.games, window.tpr
                );
            }
            println!(
                "{}\tBONUS\t{:.2}\t{}",
                player.name, report.bonus_sum, report.elo_year_periods
            );
            println!("{}\tELO_YEAR\t{}", player.name, report.elo_year);
            println!(
                "{}\tTPR_ALL\t{}\t{}",
                player.name, report.all_games_tpr, report.total_games
            );
            println!("{}\tTPR_RAW\t{}", player.name, report.raw_games_tpr);
        }

        summaries.push(summary_from_report(
            player.name.clone(),
            player.fide_id.clone(),
            player.rating.clone(),
            &report,
        ));
        sort_summaries_by_tpr(&mut summaries);
        let band = if incomplete.is_empty() {
            TPR_LIST_BAND
        } else {
            band_incomplete
        };
        retain_within_tpr_band(&mut summaries, band, list, anchor_elo_year);
        rating_floor = tpr_list_floor(&summaries, band, list, anchor_elo_year);
        maybe_print_new_anchor(list, &summaries, &mut anchor_elo_year, print_outcome);
    }

    if !incomplete.is_empty() {
        summaries.clear();
    }

    Ok(ProcessOutcome {
        summaries,
        incomplete,
        ignored_incomplete,
        cutoff_rating: Some(rating_floor),
    })
}

/// Highest FIDE-rated player among `players`, if any.
pub fn highest_rated(players: &[IncompletePlayer]) -> Option<&IncompletePlayer> {
    players
        .iter()
        .max_by_key(|p| p.rating.parse::<i32>().unwrap_or(0))
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
            match periods_for_player(&player.fide_id, periods, max_attempts, initial_wait_millis)
                .await?
            {
                PeriodsFetch::Complete { progress, .. } => {
                    println!(
                        "COMPLETE\t{}\t{}\tperiods\t{}/{}\tgames\t{}/{}",
                        player.name,
                        player.fide_id,
                        progress.periods_done,
                        progress.periods_total,
                        progress.games_done,
                        progress.games_total
                    );
                }
                PeriodsFetch::Incomplete { progress } => {
                    println!(
                        "INCOMPLETE\t{}\t{}\tperiods\t{}/{}\tgames\t{}/{}",
                        player.name,
                        player.fide_id,
                        progress.periods_done,
                        progress.periods_total,
                        progress.games_done,
                        progress.games_total
                    );
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

/// Sum of rating-list `Gms` for `fide_id` across `periods` (`YYYY-MM-01`).
fn listed_games_in_periods(fide_id: &str, periods: &[String]) -> Result<u32, Box<dyn Error>> {
    let months: Vec<(i32, u32)> = periods
        .iter()
        .map(|p| parse_period(p))
        .collect::<Result<_, _>>()?;
    player_gms_sum(fide_id, &months).ok_or_else(|| {
        format!("rating list Gms unavailable for {fide_id} over requested periods").into()
    })
}

/// ELO_YEAR at the anchor rank, if the list is long enough.
fn tpr_list_anchor(summaries: &[PlayerSummary]) -> Option<i32> {
    summaries.get(TPR_LIST_ANCHOR_RANK - 1).map(|s| s.elo_year)
}

/// Provisional anchor used before a real 30th place exists.
fn provisional_anchor(list: &str) -> i32 {
    if list == "women" {
        PROVISIONAL_ANCHOR_WOMEN
    } else {
        PROVISIONAL_ANCHOR_OPEN
    }
}

/// Anchor for cutoff: real 30th if present, else a previously seen 30th, else provisional.
fn cutoff_anchor(summaries: &[PlayerSummary], list: &str, saved_anchor: Option<i32>) -> i32 {
    tpr_list_anchor(summaries)
        .or(saved_anchor)
        .unwrap_or_else(|| provisional_anchor(list))
}

/// Floor ELO_YEAR / scan cutoff: anchor minus `band`.
fn tpr_list_floor(
    summaries: &[PlayerSummary],
    band: i32,
    list: &str,
    saved_anchor: Option<i32>,
) -> i32 {
    cutoff_anchor(summaries, list, saved_anchor) - band
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

/// Keep players within `band` of the cutoff anchor's ELO_YEAR.
fn retain_within_tpr_band(
    summaries: &mut Vec<PlayerSummary>,
    band: i32,
    list: &str,
    saved_anchor: Option<i32>,
) {
    let floor = tpr_list_floor(summaries, band, list, saved_anchor);
    summaries.retain(|s| s.elo_year >= floor);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: &str, elo_year: i32) -> PlayerSummary {
        PlayerSummary {
            name: name.into(),
            fide_id: "0".into(),
            rating: "2500".into(),
            elo_year,
            all_games_tpr: elo_year,
            raw_games_tpr: elo_year,
            total_games: 50,
        }
    }

    fn incomplete(name: &str, rating: &str) -> IncompletePlayer {
        IncompletePlayer {
            name: name.into(),
            fide_id: "0".into(),
            rating: rating.into(),
        }
    }

    #[test]
    fn highest_rated_picks_max_fide_rating() {
        let players = vec![
            incomplete("A", "2600"),
            incomplete("B", "2750"),
            incomplete("C", "2700"),
        ];
        let top = highest_rated(&players).unwrap();
        assert_eq!(top.name, "B");
        assert_eq!(top.rating, "2750");
    }

    #[test]
    fn highest_rated_empty_is_none() {
        assert!(highest_rated(&[]).is_none());
    }

    #[test]
    fn last_twelve_rating_periods_end_at_current_month() {
        let periods = last_12_rating_periods();
        assert_eq!(periods.len(), 12);
        assert!(periods[0].ends_with("-01"));
        assert!(periods[0] < periods[11]);

        let today = Utc::now().date_naive();
        let current = format!("{:04}-{:02}-01", today.year(), today.month());
        assert_eq!(periods.last(), Some(&current));

        let oldest = first_of_this_month_minus(11);
        assert_eq!(periods.first(), Some(&oldest));
    }

    fn first_of_this_month_minus(months: u32) -> String {
        let today = Utc::now().date_naive();
        let first = chrono::NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();
        let d = first - Months::new(months);
        format!("{:04}-{:02}-01", d.year(), d.month())
    }

    #[test]
    fn keeps_everyone_above_provisional_floor_before_anchor() {
        // Open provisional anchor 2750 − 150 = 2600; all of these are above that.
        let mut summaries: Vec<_> = (0..29)
            .map(|i| summary(&format!("p{i}"), 2800 - i))
            .collect();
        retain_within_tpr_band(&mut summaries, TPR_LIST_BAND, "open", None);
        assert_eq!(summaries.len(), 29);
    }

    #[test]
    fn provisional_floor_cuts_below_open_default() {
        let mut summaries = vec![
            summary("high", 2700),
            summary("edge", 2600),
            summary("below", 2599),
        ];
        retain_within_tpr_band(&mut summaries, TPR_LIST_BAND, "open", None);
        assert_eq!(summaries.len(), 2);
        assert!(summaries.iter().all(|s| s.name != "below"));
    }

    #[test]
    fn cuts_off_below_anchor_tpr_minus_band() {
        // 30 players at 2700, plus one at 2550 (kept) and one at 2549 (below 2700-150).
        let mut summaries: Vec<_> = (0..30).map(|i| summary(&format!("top{i}"), 2700)).collect();
        summaries.push(summary("edge", 2550));
        summaries.push(summary("below", 2549));
        sort_summaries_by_tpr(&mut summaries);
        retain_within_tpr_band(&mut summaries, TPR_LIST_BAND, "open", None);
        assert_eq!(summaries.len(), 31);
        assert!(summaries.iter().any(|s| s.name == "edge"));
        assert!(summaries.iter().all(|s| s.name != "below"));
        assert_eq!(
            tpr_list_floor(&summaries, TPR_LIST_BAND, "open", None),
            2550
        );
    }

    #[test]
    fn incomplete_band_keeps_a_higher_floor() {
        let mut summaries: Vec<_> = (0..30).map(|i| summary(&format!("top{i}"), 2700)).collect();
        sort_summaries_by_tpr(&mut summaries);
        assert_eq!(
            tpr_list_floor(&summaries, TPR_LIST_BAND, "open", None),
            2550
        );
        assert_eq!(
            tpr_list_floor(&summaries, TPR_LIST_BAND_INCOMPLETE, "open", None),
            2700 - TPR_LIST_BAND_INCOMPLETE
        );
    }

    #[test]
    fn provisional_floor_before_anchor_rank() {
        let summaries: Vec<_> = (0..29)
            .map(|i| summary(&format!("p{i}"), 2800 - i))
            .collect();
        assert_eq!(
            tpr_list_floor(&summaries, TPR_LIST_BAND, "open", None),
            PROVISIONAL_ANCHOR_OPEN - TPR_LIST_BAND
        );
        assert_eq!(
            tpr_list_floor(&summaries, TPR_LIST_BAND, "women", None),
            PROVISIONAL_ANCHOR_WOMEN - TPR_LIST_BAND
        );
    }

    #[test]
    fn incomplete_floor_uses_provisional_without_anchor() {
        let summaries: Vec<_> = (0..10)
            .map(|i| summary(&format!("p{i}"), 2800 - i))
            .collect();
        assert_eq!(
            tpr_list_floor(&summaries, TPR_LIST_BAND_INCOMPLETE, "open", None),
            PROVISIONAL_ANCHOR_OPEN - TPR_LIST_BAND_INCOMPLETE
        );
    }

    #[test]
    fn incomplete_floor_uses_saved_anchor() {
        let summaries: Vec<PlayerSummary> = Vec::new();
        assert_eq!(
            tpr_list_floor(&summaries, TPR_LIST_BAND_INCOMPLETE, "open", Some(2700)),
            2700 - TPR_LIST_BAND_INCOMPLETE
        );
    }

    #[test]
    fn incomplete_band_still_grows_list_toward_anchor() {
        // With incompletes present we use the tighter band, but summaries are kept
        // so the list can still reach the 30th for cutoff.
        let mut summaries: Vec<_> = (0..29)
            .map(|i| summary(&format!("p{i}"), 2800 - i))
            .collect();
        assert_eq!(
            tpr_list_floor(&summaries, TPR_LIST_BAND_INCOMPLETE, "open", None),
            PROVISIONAL_ANCHOR_OPEN - TPR_LIST_BAND_INCOMPLETE
        );
        summaries.push(summary("p29", 2771));
        sort_summaries_by_tpr(&mut summaries);
        retain_within_tpr_band(&mut summaries, TPR_LIST_BAND_INCOMPLETE, "open", None);
        assert_eq!(summaries.len(), 30);
        assert_eq!(
            tpr_list_floor(&summaries, TPR_LIST_BAND_INCOMPLETE, "open", None),
            2771 - TPR_LIST_BAND_INCOMPLETE
        );
    }
}
