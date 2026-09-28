use crate::model::{PeriodResult, PlayerSummary};
use std::error::Error;

/// Development coefficient used when checking that |ΔR| < 0.5 at the TPR.
const RATING_K: f64 = 10.0;
const TPR_GAME_THRESHOLD: usize = 50;

pub struct TprWindow {
    pub start_period: String,
    pub end_period: String,
    pub games: usize,
    pub tpr: i32,
}

pub struct TprReport {
    pub windows: Vec<TprWindow>,
    pub max_tpr: i32,
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
    let actual_games: Vec<RatedGame> = period_games
        .iter()
        .flat_map(|(_, g)| g.iter().copied())
        .collect();
    let raw_games_tpr = tournament_performance_rating(&actual_games);

    let mut all_games = actual_games;
    if all_games.len() < TPR_GAME_THRESHOLD {
        let virtual_opp = player_rating - 100.0;
        while all_games.len() < TPR_GAME_THRESHOLD {
            all_games.push((virtual_opp, 0.5));
        }
    }
    let all_games_tpr = tournament_performance_rating(&all_games);

    let windows = if total_games < TPR_GAME_THRESHOLD {
        let start = period_games
            .first()
            .map(|(p, _)| p.clone())
            .unwrap_or_default();
        let end = period_games
            .last()
            .map(|(p, _)| p.clone())
            .unwrap_or_default();
        vec![TprWindow {
            start_period: start,
            end_period: end,
            games: total_games,
            tpr: all_games_tpr,
        }]
    } else {
        consecutive_windows_with_at_least(&period_games, TPR_GAME_THRESHOLD)
            .into_iter()
            .map(|window| TprWindow {
                start_period: period_games[window.start].0.clone(),
                end_period: period_games[window.end].0.clone(),
                games: window.games.len(),
                tpr: tournament_performance_rating(&window.games),
            })
            .collect()
    };

    let max_tpr = windows
        .iter()
        .map(|w| w.tpr)
        .max()
        .ok_or("no TPR windows")?;
    Ok(TprReport {
        windows,
        max_tpr,
        all_games_tpr,
        raw_games_tpr,
        total_games,
    })
}

pub fn summary_from_report(name: String, rating: String, report: &TprReport) -> PlayerSummary {
    PlayerSummary {
        name,
        rating,
        max_tpr: report.max_tpr,
        all_games_tpr: report.all_games_tpr,
        raw_games_tpr: report.raw_games_tpr,
        total_games: report.total_games,
    }
}

/// All consecutive period ranges [start, end] with at least `min_games` games,
/// excluding ranges that start or end on an empty period.
fn consecutive_windows_with_at_least(
    period_games: &[PeriodGameList],
    min_games: usize,
) -> Vec<ConsecutiveWindow> {
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
            if total >= min_games {
                let games: Vec<RatedGame> = period_games[start..=end]
                    .iter()
                    .flat_map(|(_, g)| g.iter().copied())
                    .collect();
                windows.push(ConsecutiveWindow { start, end, games });
            }
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

    #[test]
    fn finds_all_consecutive_period_windows_with_enough_games() {
        let periods = vec![
            ("a".into(), vec![(2000.0, 1.0); 30]),
            ("b".into(), vec![(2000.0, 1.0); 30]),
            ("c".into(), vec![(2000.0, 1.0); 30]),
        ];
        let windows = consecutive_windows_with_at_least(&periods, 50);
        assert_eq!(windows.len(), 3);
        assert_eq!(
            (windows[0].start, windows[0].end, windows[0].games.len()),
            (0, 1, 60)
        );
        assert_eq!(
            (windows[1].start, windows[1].end, windows[1].games.len()),
            (0, 2, 90)
        );
        assert_eq!(
            (windows[2].start, windows[2].end, windows[2].games.len()),
            (1, 2, 60)
        );
    }

    #[test]
    fn omits_windows_that_start_or_end_on_empty_period() {
        let periods = vec![
            ("empty".into(), vec![]),
            ("a".into(), vec![(2000.0, 1.0); 30]),
            ("b".into(), vec![(2000.0, 1.0); 30]),
            ("also_empty".into(), vec![]),
        ];
        let windows = consecutive_windows_with_at_least(&periods, 50);
        assert_eq!(windows.len(), 1);
        assert_eq!(
            (windows[0].start, windows[0].end, windows[0].games.len()),
            (1, 2, 60)
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
    fn tpr_pads_virtual_draws_when_under_threshold() {
        let results = vec![PeriodResult {
            fide_id: "1".into(),
            period: "2025-09-01".into(),
            games: vec![Game {
                color: "white".into(),
                opponent_rating: "2400".into(),
                result: "1.00".into(),
            }],
        }];
        let report = tpr_report(&results, 2500.0).unwrap();
        assert_eq!(report.windows.len(), 1);
        assert_eq!(report.windows[0].games, 1);
    }
}
