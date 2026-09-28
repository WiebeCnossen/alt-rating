mod cache;
mod fide;
mod http;
mod model;
mod output;
mod pipeline;
mod rating_list;
mod tpr;

use output::{summary_csv_path, write_summary_csv};
use pipeline::{last_12_complete_months, process_players};
use rating_list::fetch_top_players;
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let periods = last_12_complete_months();
    let (men_players, women_players) = fetch_top_players().await?;

    let men = process_players("open", &men_players, &periods).await?;
    write_summary_csv(summary_csv_path("open"), &men).await?;

    let women = process_players("women", &women_players, &periods).await?;
    write_summary_csv(summary_csv_path("women"), &women).await?;

    Ok(())
}
