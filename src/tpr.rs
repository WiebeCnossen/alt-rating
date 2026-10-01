use crate::model::{PeriodResult, PlayerSummary};
use std::error::Error;

/// Development coefficient used when checking that |ΔR| < 0.5 at the TPR.
const RATING_K: f64 = 10.0;
const TPR_GAME_THRESHOLD: usize = 50;

/// Minimum games for a period set to count toward ELO_YEAR, and for a player
/// to appear on the output lists.
pub const MIN_GAMES: usize = 12;

pub struct TprWindow {
    pub start_period: String,
    pub end_period: String,
    pub games: usize,
    pub tpr: i32,
}

pub struct TprReport {
    pub windows: Vec<TprWindow>,
    pub elo_year: i32,
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
) -> Result<TprReport, Box<dyn Error>> {
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
    let sets = consecutive_period_sets(&period_games);
    if sets.is_empty() {
        return Err("no period sets".into());
    }

    let set_ratings: Vec<SetRatings> = sets
        .iter()
        .map(|window| ratings_for_games(&window.games, player_rating))
        .collect();

    let elo_year = set_ratings
        .iter()
        .map(|r| r.elo_year)
        .max()
        .ok_or("no period sets")?;

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
        all_games_tpr: longest.all_games_tpr,
        raw_games_tpr: longest.raw_games_tpr,
        total_games,
    })
}

pub fn summary_from_report(name: String, rating: String, report: &TprReport) -> PlayerSummary {
    PlayerSummary {
        name,
        rating,
        elo_year: report.elo_year,
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
        let virtual_opp = player_rating - 100.0;
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

/// All consecutive period ranges [start, end] with at least [`MIN_GAMES`] games
/// that do not start or end on an empty period (empty periods in the middle are
/// allowed).
fn consecutive_period_sets(period_games: &[PeriodGameList]) -> Vec<ConsecutiveWindow> {
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
            if total < MIN_GAMES {
                continue;
            }
            let games: Vec<RatedGame> = period_games[start..=end]
                .iter()
                .flat_map(|(_, g)| g.iter().copied())
                .collect();
            windows.push(ConsecutiveWindow { start, end, games });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Game;

    fn period(period: &str, n: usize, opp: f64, score: f64) -> PeriodResult {
        PeriodResult {
            fide_id: "1".into(),
            period: period.into(),
            games: (0..n)
                .map(|_| Game {
                    color: "white".into(),
                    opponent_rating: opp.to_string(),
                    result: format!("{score:.2}"),
                })
                .collect(),
        }
    }

    #[test]
    fn finds_all_consecutive_period_sets() {
        let periods = vec![
            ("a".into(), vec![(2000.0, 1.0); 30]),
            ("b".into(), vec![(2000.0, 1.0); 30]),
            ("c".into(), vec![(2000.0, 1.0); 30]),
        ];
        let windows = consecutive_period_sets(&periods);
        // All [start,end] with non-empty endpoints: (0,0),(0,1),(0,2),(1,1),(1,2),(2,2)
        assert_eq!(windows.len(), 6);
        assert_eq!(
            (windows[0].start, windows[0].end, windows[0].games.len()),
            (0, 0, 30)
        );
        assert_eq!(
            (windows[1].start, windows[1].end, windows[1].games.len()),
            (0, 1, 60)
        );
        assert_eq!(
            (windows[2].start, windows[2].end, windows[2].games.len()),
            (0, 2, 90)
        );
        assert_eq!(
            (windows[3].start, windows[3].end, windows[3].games.len()),
            (1, 1, 30)
        );
        assert_eq!(
            (windows[4].start, windows[4].end, windows[4].games.len()),
            (1, 2, 60)
        );
        assert_eq!(
            (windows[5].start, windows[5].end, windows[5].games.len()),
            (2, 2, 30)
        );
    }

    #[test]
    fn omits_sets_that_start_or_end_on_empty_period() {
        let periods = vec![
            ("empty".into(), vec![]),
            ("a".into(), vec![(2000.0, 1.0); 30]),
            ("b".into(), vec![(2000.0, 1.0); 30]),
            ("also_empty".into(), vec![]),
        ];
        let windows = consecutive_period_sets(&periods);
        assert_eq!(windows.len(), 3);
        assert_eq!(
            (windows[0].start, windows[0].end, windows[0].games.len()),
            (1, 1, 30)
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
    fn omits_sets_below_min_games() {
        let periods = vec![
            ("a".into(), vec![(2000.0, 1.0); MIN_GAMES - 1]),
            ("b".into(), vec![(2000.0, 1.0); MIN_GAMES - 1]),
        ];
        let windows = consecutive_period_sets(&periods);
        // Single months are under the floor; only the combined set qualifies.
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
        assert!(tpr_report(&results, 2500.0).is_err());
    }

    #[test]
    fn elo_year_is_min_of_stuffed_and_penalized_raw_when_under_threshold() {
        let results = vec![period("2025-09-01", MIN_GAMES, 2400.0, 1.0)];
        let report = tpr_report(&results, 2500.0).unwrap();
        assert_eq!(report.windows.len(), 1);
        assert_eq!(report.windows[0].games, MIN_GAMES);
        let games_short = (TPR_GAME_THRESHOLD - MIN_GAMES) as i32;
        let penalized_raw = report.raw_games_tpr - 2 * games_short;
        assert_eq!(report.elo_year, report.all_games_tpr.min(penalized_raw));
        assert_ne!(report.all_games_tpr, report.raw_games_tpr);
    }

    #[test]
    fn reported_tpr_comes_from_longest_set_elo_year_is_max() {
        // Strong short month vs weaker full span: max ELO_YEAR can come from a
        // subset, while TPR_ALL/TPR_RAW stay on the longest (full) set.
        let results = vec![
            period("2025-01-01", 40, 2800.0, 1.0),
            period("2025-02-01", 40, 2000.0, 0.0),
        ];
        let report = tpr_report(&results, 2500.0).unwrap();
        assert_eq!(report.total_games, 80);
        assert_eq!(report.windows.len(), 3);

        let full = ratings_for_games(
            &[(2800.0, 1.0); 40]
                .into_iter()
                .chain([(2000.0, 0.0); 40])
                .collect::<Vec<_>>(),
            2500.0,
        );
        let first_only = ratings_for_games(&[(2800.0, 1.0); 40], 2500.0);
        let second_only = ratings_for_games(&[(2000.0, 0.0); 40], 2500.0);

        assert_eq!(report.all_games_tpr, full.all_games_tpr);
        assert_eq!(report.raw_games_tpr, full.raw_games_tpr);
        assert_eq!(
            report.elo_year,
            full.elo_year
                .max(first_only.elo_year)
                .max(second_only.elo_year)
        );
    }

    #[test]
    fn short_subset_uses_stuffing_and_penalty_for_elo_year() {
        let n = MIN_GAMES;
        let results = vec![
            period("2025-01-01", n, 2700.0, 1.0),
            period("2025-02-01", n, 2000.0, 0.0),
        ];
        let report = tpr_report(&results, 2500.0).unwrap();
        let first_games = vec![(2700.0, 1.0); n];
        let first = ratings_for_games(&first_games, 2500.0);
        let games_short = (TPR_GAME_THRESHOLD - n) as i32;
        assert_eq!(
            first.elo_year,
            first
                .all_games_tpr
                .min(first.raw_games_tpr - 2 * games_short)
        );
        assert_eq!(
            report.elo_year,
            report.windows.iter().map(|w| w.tpr).max().unwrap()
        );
        assert!(
            report
                .windows
                .iter()
                .any(|w| w.games == n && w.tpr == first.elo_year)
        );
        // Reported TPR_* still from the longest set.
        let full_games: Vec<_> = first_games
            .into_iter()
            .chain(vec![(2000.0, 0.0); n])
            .collect();
        let full = ratings_for_games(&full_games, 2500.0);
        assert_eq!(report.all_games_tpr, full.all_games_tpr);
        assert_eq!(report.raw_games_tpr, full.raw_games_tpr);
    }
}
