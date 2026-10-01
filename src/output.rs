use crate::model::PlayerSummary;
use chrono::{Datelike, Utc};
use std::error::Error;
use std::path::{Path, PathBuf};
use tokio::fs;

const OUTPUT_DIR: &str = "output";

pub fn summary_csv_path(prefix: &str) -> PathBuf {
    let today = Utc::now().date_naive();
    PathBuf::from(OUTPUT_DIR).join(format!(
        "{prefix}-{:04}-{:02}.csv",
        today.year(),
        today.month()
    ))
}

pub async fn write_summary_csv(
    path: impl AsRef<Path>,
    summaries: &[PlayerSummary],
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(OUTPUT_DIR).await?;
    let mut out = String::from("name,fide_id,elo_year,rating,diff,games\n");
    for s in summaries {
        out.push_str(&format!(
            "{},{},{},{},{},{}\n",
            csv_escape(&s.name),
            csv_escape(&s.fide_id),
            s.elo_year,
            csv_escape(&s.rating),
            s.diff(),
            s.total_games
        ));
    }
    fs::write(path, out).await?;
    Ok(())
}

fn csv_escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

pub fn print_summary_table(summaries: &[PlayerSummary]) {
    println!();
    for s in summaries {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            s.name,
            s.elo_year,
            s.all_games_tpr,
            s.raw_games_tpr,
            s.rating,
            s.diff(),
            s.total_games
        );
    }
    println!();
}
