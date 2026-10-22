use crate::bonus::{Top30ByMonth, year_win_bonus};
use crate::model::{PeriodResult, PlayerSummary};
use std::error::Error;

/// Development coefficient used when checking that |ΔR| < 0.5 at the TPR.
const RATING_K: f64 = 10.0;
const TPR_GAME_THRESHOLD: usize = 50;
const VIRTUAL_RATING_OFFSET: f64 = 0.0;

/// Drop games where the opponent is more than this many points below the player.
pub const MAX_WEAKER_OPPONENT_DIFF: f64 = 350.0;

/// Minimum games for a period set to count toward ELO_YEAR, for a player to
/// appear on the output lists, and for activity in the top-30 bonus index.
pub const MIN_GAMES: usize = 30;

/// Whether a game against `opponent_rating` counts for the player rated `player_rating`.
pub fn opponent_counts(player_rating: f64, opponent_rating: f64) -> bool {
    opponent_rating + MAX_WEAKER_OPPONENT_DIFF >= player_rating
}

/// Drop games against opponents more than [`MAX_WEAKER_OPPONENT_DIFF`] below `player_rating`.
pub fn filter_period_games(
    period: &PeriodResult,
    player_rating: f64,
) -> Result<PeriodResult, Box<dyn Error>> {
    let mut games = Vec::with_capacity(period.games.len());
    for g in &period.games {
        let opp: f64 = g.opponent_rating.trim().parse()?;
        if opponent_counts(player_rating, opp) {
            games.push(g.clone());
        }
    }
    Ok(PeriodResult {
        fide_id: period.fide_id.clone(),
        period: period.period.clone(),
        games,
    })
}

/// Games in `results` that count for `player_rating` (after the weaker-opponent filter).
pub fn counted_games(
    results: &[PeriodResult],
    player_rating: f64,
) -> Result<usize, Box<dyn Error>> {
    let mut n = 0;
    for r in results {
        for g in &r.games {
            let opp: f64 = g.opponent_rating.trim().parse()?;
            if opponent_counts(player_rating, opp) {
                n += 1;
            }
        }
    }
    Ok(n)
}

pub struct TprWindow {
    pub start_period: String,
    pub end_period: String,
    pub games: usize,
    pub tpr: i32,
}

pub struct TprReport {
    pub windows: Vec<TprWindow>,
    /// Max period ELO_YEAR plus rounded top-30 win bonuses for the year.
    pub elo_year: i32,
    /// Max ELO_YEAR across qualifying period sets (before bonuses).
    pub elo_year_periods: i32,
    /// Sum of top-30 win bonuses over the year (before rounding into `elo_year`).
    pub bonus_sum: f64,
    pub all_games_tpr: i32,
    pub raw_games_tpr: i32,
    pub total_games: usize,
}

/// Opponent rating and game score (0, 0.5, or 1).
type RatedGame = (f64, f64);
type PeriodGameList = (String, Vec<RatedGame>);

struct ConsecutiveWindow {
    start: usize,
    end: usize,
    games: Vec<RatedGame>,
}

struct SetRatings {
    all_games_tpr: i32,
    raw_games_tpr: i32,
    elo_year: i32,
}

pub fn tpr_report(
    results: &[PeriodResult],
    player_rating: f64,
    top30: &Top30ByMonth,
) -> Result<TprReport, Box<dyn Error>> {
    let counted: Vec<PeriodResult> = results
        .iter()
        .map(|r| filter_period_games(r, player_rating))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;

    let period_games: Vec<PeriodGameList> = counted
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
    let sets = consecutive_period_sets(&period_games);
    if sets.is_empty() {
        return Err("no period sets".into());
    }

    let set_ratings: Vec<SetRatings> = sets
        .iter()
        .map(|window| ratings_for_games(&window.games, player_rating))
        .collect();

    let elo_year_periods = set_ratings
        .iter()
        .map(|r| r.elo_year)
        .max()
        .ok_or("no period sets")?;

    let bonus_sum = year_win_bonus(&counted, top30)?;
    let elo_year = elo_year_periods + bonus_sum.round() as i32;

    // Longest set by period span; that span is the whole active year
    // (first non-empty period through last non-empty period).
    let longest_idx = sets
        .iter()
        .enumerate()
        .max_by_key(|(_, w)| w.end - w.start)
        .map(|(i, _)| i)
        .ok_or("no period sets")?;
    let longest = &set_ratings[longest_idx];

    let windows = sets
        .iter()
        .zip(set_ratings.iter())
        .map(|(window, ratings)| TprWindow {
            start_period: period_games[window.start].0.clone(),
            end_period: period_games[window.end].0.clone(),
            games: window.games.len(),
            tpr: ratings.elo_year,
        })
        .collect();

    Ok(TprReport {
        windows,
        elo_year,
        elo_year_periods,
        bonus_sum,
        all_games_tpr: longest.all_games_tpr,
        raw_games_tpr: longest.raw_games_tpr,
        total_games,
    })
}

pub fn summary_from_report(
    name: String,
    fide_id: String,
    rating: String,
    report: &TprReport,
) -> PlayerSummary {
    PlayerSummary {
        name,
        fide_id,
        rating,
        elo_year: report.elo_year,
        bonus: report.bonus_sum,
        all_games_tpr: report.all_games_tpr,
        raw_games_tpr: report.raw_games_tpr,
        total_games: report.total_games,
    }
}

/// TPR_ALL (with stuffing), TPR_RAW, and ELO_YEAR for one period set.
fn ratings_for_games(games: &[RatedGame], player_rating: f64) -> SetRatings {
    let raw_games_tpr = tournament_performance_rating(games);

    let mut stuffed: Vec<RatedGame> = games.to_vec();
    if stuffed.len() < TPR_GAME_THRESHOLD {
        let virtual_opp = player_rating - VIRTUAL_RATING_OFFSET;
        while stuffed.len() < TPR_GAME_THRESHOLD {
            stuffed.push((virtual_opp, 0.5));
        }
    }
    let all_games_tpr = tournament_performance_rating(&stuffed);

    let elo_year = if games.len() < TPR_GAME_THRESHOLD {
        let games_short = (TPR_GAME_THRESHOLD - games.len()) as i32;
        all_games_tpr.min(raw_games_tpr - 2 * games_short)
    } else {
        all_games_tpr
    };

    SetRatings {
        all_games_tpr,
        raw_games_tpr,
        elo_year,
    }
}

/// Consecutive period ranges [start, end] with at least [`MIN_GAMES`] games that
/// all end on the most recent non-empty period and do not start on an empty
/// period (empty periods in the middle are allowed).
fn consecutive_period_sets(period_games: &[PeriodGameList]) -> Vec<ConsecutiveWindow> {
    let counts: Vec<usize> = period_games.iter().map(|(_, g)| g.len()).collect();
    let Some(end) = counts.iter().rposition(|&c| c > 0) else {
        return Vec::new();
    };

    let mut windows = Vec::new();
    for start in 0..=end {
        if counts[start] == 0 {
            continue;
        }
        let total: usize = counts[start..=end].iter().sum();
        if total < MIN_GAMES {
            continue;
        }
        let games: Vec<RatedGame> = period_games[start..=end]
            .iter()
            .flat_map(|(_, g)| g.iter().copied())
            .collect();
        windows.push(ConsecutiveWindow { start, end, games });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bonus::Top30ByMonth;
    use crate::model::Game;

    fn empty_top30() -> Top30ByMonth {
        Top30ByMonth::default()
    }

    fn period(period: &str, n: usize, opp: f64, score: f64) -> PeriodResult {
        PeriodResult {
            fide_id: "1".into(),
            period: period.into(),
            games: (0..n)
                .map(|_| Game {
                    color: "white".into(),
                    opponent_name: "Opp".into(),
                    opponent_rating: opp.to_string(),
                    result: format!("{score:.2}"),
                })
                .collect(),
        }
    }

    #[test]
    fn finds_sets_ending_on_most_recent_non_empty_period() {
        let periods = vec![
            ("a".into(), vec![(2000.0, 1.0); 30]),
            ("b".into(), vec![(2000.0, 1.0); 30]),
            ("c".into(), vec![(2000.0, 1.0); 30]),
        ];
        let windows = consecutive_period_sets(&periods);
        // Only sets ending on the last non-empty period: (0,2),(1,2),(2,2)
        assert_eq!(windows.len(), 3);
        assert_eq!(
            (windows[0].start, windows[0].end, windows[0].games.len()),
            (0, 2, 90)
        );
        assert_eq!(
            (windows[1].start, windows[1].end, windows[1].games.len()),
            (1, 2, 60)
        );
        assert_eq!(
            (windows[2].start, windows[2].end, windows[2].games.len()),
            (2, 2, 30)
        );
    }

    #[test]
    fn omits_sets_that_start_on_empty_or_end_before_latest() {
        let periods = vec![
            ("empty".into(), vec![]),
            ("a".into(), vec![(2000.0, 1.0); 30]),
            ("b".into(), vec![(2000.0, 1.0); 30]),
            ("also_empty".into(), vec![]),
        ];
        let windows = consecutive_period_sets(&periods);
        // Latest non-empty is index 2; empty start skipped → (1,2),(2,2)
        assert_eq!(windows.len(), 2);
        assert_eq!(
            (windows[0].start, windows[0].end, windows[0].games.len()),
            (1, 2, 60)
        );
        assert_eq!(
            (windows[1].start, windows[1].end, windows[1].games.len()),
            (2, 2, 30)
        );
    }

    #[test]
    fn omits_sets_below_min_games() {
        let periods = vec![
            ("a".into(), vec![(2000.0, 1.0); MIN_GAMES - 1]),
            ("b".into(), vec![(2000.0, 1.0); MIN_GAMES - 1]),
        ];
        let windows = consecutive_period_sets(&periods);
        // Only sets ending on b: a alone is under the floor; a+b and b alone
        // (also under floor) → only the combined set qualifies.
        assert_eq!(windows.len(), 1);
        assert_eq!(
            (windows[0].start, windows[0].end, windows[0].games.len()),
            (0, 1, 2 * (MIN_GAMES - 1))
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
    fn tpr_report_errors_when_total_games_below_min() {
        let results = vec![period("2025-09-01", MIN_GAMES - 1, 2400.0, 1.0)];
        assert!(tpr_report(&results, 2500.0, &empty_top30()).is_err());
    }

    #[test]
    fn elo_year_is_min_of_stuffed_and_penalized_raw_when_under_threshold() {
        let results = vec![period("2025-09-01", MIN_GAMES, 2400.0, 1.0)];
        let report = tpr_report(&results, 2500.0, &empty_top30()).unwrap();
        assert_eq!(report.windows.len(), 1);
        assert_eq!(report.windows[0].games, MIN_GAMES);
        let games_short = (TPR_GAME_THRESHOLD - MIN_GAMES) as i32;
        let penalized_raw = report.raw_games_tpr - 2 * games_short;
        assert_eq!(
            report.elo_year_periods,
            report.all_games_tpr.min(penalized_raw)
        );
        assert_eq!(report.elo_year, report.elo_year_periods);
        assert_ne!(report.all_games_tpr, report.raw_games_tpr);
    }

    #[test]
    fn reported_tpr_comes_from_longest_set_elo_year_is_max() {
        // Strong recent month vs weaker full span: max ELO_YEAR can come from
        // the latest-ending subset, while TPR_ALL/TPR_RAW stay on the longest set.
        // Opponents stay within MAX_WEAKER_OPPONENT_DIFF of the player (2500).
        let results = vec![
            period("2025-01-01", 40, 2200.0, 0.0),
            period("2025-02-01", 40, 2800.0, 1.0),
        ];
        let report = tpr_report(&results, 2500.0, &empty_top30()).unwrap();
        assert_eq!(report.total_games, 80);
        // Only sets ending on Feb: full year and Feb alone.
        assert_eq!(report.windows.len(), 2);

        let full = ratings_for_games(
            &[(2200.0, 0.0); 40]
                .into_iter()
                .chain([(2800.0, 1.0); 40])
                .collect::<Vec<_>>(),
            2500.0,
        );
        let latest_only = ratings_for_games(&[(2800.0, 1.0); 40], 2500.0);

        assert_eq!(report.all_games_tpr, full.all_games_tpr);
        assert_eq!(report.raw_games_tpr, full.raw_games_tpr);
        assert_eq!(
            report.elo_year_periods,
            full.elo_year.max(latest_only.elo_year)
        );
        assert_eq!(report.elo_year, report.elo_year_periods);
    }

    #[test]
    fn short_subset_uses_stuffing_and_penalty_for_elo_year() {
        let n = MIN_GAMES;
        let results = vec![
            period("2025-01-01", n, 2200.0, 0.0),
            period("2025-02-01", n, 2700.0, 1.0),
        ];
        let report = tpr_report(&results, 2500.0, &empty_top30()).unwrap();
        let latest_games = vec![(2700.0, 1.0); n];
        let latest = ratings_for_games(&latest_games, 2500.0);
        let games_short = (TPR_GAME_THRESHOLD - n) as i32;
        assert_eq!(
            latest.elo_year,
            latest
                .all_games_tpr
                .min(latest.raw_games_tpr - 2 * games_short)
        );
        assert_eq!(
            report.elo_year_periods,
            report.windows.iter().map(|w| w.tpr).max().unwrap()
        );
        assert!(
            report
                .windows
                .iter()
                .any(|w| w.games == n && w.tpr == latest.elo_year)
        );
        // Reported TPR_* still from the longest set.
        let full_games: Vec<_> = vec![(2200.0, 0.0); n]
            .into_iter()
            .chain(latest_games)
            .collect();
        let full = ratings_for_games(&full_games, 2500.0);
        assert_eq!(report.all_games_tpr, full.all_games_tpr);
        assert_eq!(report.raw_games_tpr, full.raw_games_tpr);
    }

    #[test]
    fn disregards_opponents_more_than_350_weaker() {
        let results = vec![
            period("2025-09-01", MIN_GAMES, 2149.0, 1.0), // 351 below 2500 → drop
            period("2025-10-01", MIN_GAMES, 2150.0, 0.5), // exactly 350 → keep
        ];
        let report = tpr_report(&results, 2500.0, &empty_top30()).unwrap();
        assert_eq!(report.total_games, MIN_GAMES);
        assert_eq!(report.windows.len(), 1);
        assert_eq!(report.windows[0].games, MIN_GAMES);
        assert_eq!(report.windows[0].start_period, "2025-10-01");
    }

    #[test]
    fn keeps_opponent_exactly_350_weaker() {
        assert!(opponent_counts(2500.0, 2150.0));
        assert!(!opponent_counts(2500.0, 2149.0));
    }

    #[test]
    fn elo_year_adds_rounded_year_bonuses_to_max_period() {
        use crate::bonus::{bonus_for_rank, build_top30_month};
        use crate::model::CompactPlayer;
        use crate::rating_list::period_minus_months;
        use std::collections::HashMap;

        let list_month = "2026-06-01";
        let mut players = Vec::new();
        for i in 0..30u32 {
            players.push(CompactPlayer {
                fide_id: format!("{i}"),
                name: format!("Player, {i}"),
                sex: 'M',
                rating: 2800 - i,
                gms: 3,
            });
        }
        let mut lists = HashMap::new();
        for offset in 0..12u32 {
            let p = period_minus_months(list_month, offset).unwrap();
            lists.insert(p, players.clone());
        }
        let month = build_top30_month(list_month, 'M', &lists).unwrap();
        let mut top30 = Top30ByMonth::default();
        top30.insert(list_month.into(), month);

        let mut results = vec![period("2026-07-01", MIN_GAMES, 2400.0, 0.5)];
        results[0].games[0] = Game {
            color: "white".into(),
            opponent_name: "Player, 0".into(),
            opponent_rating: "2800".into(),
            result: "1.00".into(),
        };
        let report = tpr_report(&results, 2500.0, &top30).unwrap();
        assert!((report.bonus_sum - bonus_for_rank(1)).abs() < 1e-9);
        assert_eq!(
            report.elo_year,
            report.elo_year_periods + report.bonus_sum.round() as i32
        );
    }
}
