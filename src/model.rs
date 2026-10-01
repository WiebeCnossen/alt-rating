use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Player {
    pub fide_id: String,
    pub name: String,
    pub rating: String,
    /// Games in the most recent rating-list month (`Gms` column).
    pub recent_games: u32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Game {
    pub color: String,
    pub opponent_rating: String,
    pub result: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PeriodResult {
    pub fide_id: String,
    pub period: String,
    pub games: Vec<Game>,
}

pub struct PlayerSummary {
    pub name: String,
    pub rating: String,
    pub elo_year: i32,
    pub all_games_tpr: i32,
    pub raw_games_tpr: i32,
    pub total_games: usize,
}

/// Player whose period games could not be fully loaded (e.g. fetch retries exhausted).
#[derive(Clone)]
pub struct IncompletePlayer {
    pub name: String,
    pub fide_id: String,
}

impl PlayerSummary {
    pub fn diff(&self) -> i32 {
        self.elo_year - self.rating.parse::<i32>().unwrap_or(0)
    }
}
