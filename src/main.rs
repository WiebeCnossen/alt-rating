mod cache;
mod fide;
mod http;
mod model;
mod output;
mod pipeline;
mod rating_list;
mod tpr;

use clap::Parser;
use output::{print_summary_table, summary_csv_path, write_summary_csv};
use pipeline::{last_12_complete_months, process_players};
use rating_list::fetch_top_players;
use std::error::Error;

#[derive(Parser, Debug)]
#[command(name = "alt-rating")]
struct Args {
    /// Maximum fetch attempts (initial try + retries) before giving up.
    #[arg(long, default_value_t = 8)]
    retries: u32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    println!("INIT");
    let periods = last_12_complete_months();
    let (men_players, women_players) = fetch_top_players(args.retries).await?;

    let men = process_players("open", &men_players, &periods, args.retries).await?;
    let women = process_players("women", &women_players, &periods, args.retries).await?;

    let mut incomplete = men.incomplete;
    incomplete.extend(women.incomplete);
    if !incomplete.is_empty() {
        println!("INCOMPLETE_REMAINING\t{}", incomplete.len());
        for player in &incomplete {
            println!("INCOMPLETE\t{}\t{}", player.name, player.fide_id);
        }
        std::process::exit(1);
    }

    print_summary_table(&men.summaries);
    print_summary_table(&women.summaries);
    write_summary_csv(summary_csv_path("open"), &men.summaries).await?;
    write_summary_csv(summary_csv_path("women"), &women.summaries).await?;
    println!("INCOMPLETE_REMAINING\t0");
    Ok(())
}
