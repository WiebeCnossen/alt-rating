use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Player {
    pub fide_id: String,
    pub name: String,
    pub rating: String,
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
    pub max_tpr: i32,
    pub all_games_tpr: i32,
    pub raw_games_tpr: i32,
    pub total_games: usize,
}

impl PlayerSummary {
    pub fn diff(&self) -> i32 {
        self.max_tpr - self.rating.parse::<i32>().unwrap_or(0)
    }
}
