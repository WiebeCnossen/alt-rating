mod cache;
mod fide;
mod http;
mod model;
mod output;
mod pipeline;
mod rating_list;
mod tpr;

use clap::Parser;
use http::DEFAULT_WAIT_MILLIS;
use output::{print_summary_table, summary_csv_path, write_summary_csv};
use pipeline::{
    TPR_LIST_BAND_INCOMPLETE, complete_incomplete_players, highest_rated, last_12_rating_periods,
    process_players,
};
use rating_list::fetch_top_players;
use std::error::Error;

#[derive(Parser, Debug)]
#[command(name = "alt-rating")]
struct Args {
    /// Maximum fetch attempts (initial try + retries) before giving up.
    #[arg(long, default_value_t = 8)]
    retries: u32,

    /// Initial wait before each fetch attempt, in milliseconds (doubles on retry).
    #[arg(long, default_value_t = DEFAULT_WAIT_MILLIS)]
    wait: u64,

    /// TPR band below the anchor while incomplete players remain.
    #[arg(long, default_value_t = TPR_LIST_BAND_INCOMPLETE)]
    band_incomplete: i32,

    /// Skip players whose period fetches are incomplete instead of retrying them.
    #[arg(long)]
    ignore_incomplete: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    println!("INIT");
    let periods = last_12_rating_periods();
    let (men_players, women_players) = fetch_top_players(args.retries, args.wait).await?;

    loop {
        let men = process_players(
            "open",
            &men_players,
            &periods,
            args.retries,
            args.wait,
            args.band_incomplete,
            args.ignore_incomplete,
        )
        .await?;
        let women = process_players(
            "women",
            &women_players,
            &periods,
            args.retries,
            args.wait,
            args.band_incomplete,
            args.ignore_incomplete,
        )
        .await?;

        let mut incomplete = men.incomplete;
        incomplete.extend(women.incomplete);
        if !incomplete.is_empty() {
            println!("INCOMPLETE_REMAINING\t{}", incomplete.len());
            for player in &incomplete {
                println!("INCOMPLETE\t{}\t{}", player.name, player.fide_id);
            }
            complete_incomplete_players(&incomplete, &periods, args.retries, args.wait).await?;
            println!("RESTART");
            continue;
        }

        print_summary_table(&men.summaries);
        print_summary_table(&women.summaries);
        write_summary_csv(summary_csv_path("open"), &men.summaries).await?;
        write_summary_csv(summary_csv_path("women"), &women.summaries).await?;
        println!("INCOMPLETE_REMAINING\t0");
        report_ignored_highest("open", &men.ignored_incomplete, men.cutoff_rating);
        report_ignored_highest("women", &women.ignored_incomplete, women.cutoff_rating);
        return Ok(());
    }
}

fn report_ignored_highest(
    list: &str,
    ignored: &[model::IncompletePlayer],
    cutoff_rating: Option<i32>,
) {
    if let Some(player) = highest_rated(ignored) {
        let cutoff = cutoff_rating
            .map(|r| r.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "IGNORED_INCOMPLETE_HIGHEST\t{list}\t{}\t{}\t{}\tcutoff\t{cutoff}",
            player.name, player.fide_id, player.rating
        );
    }
}
