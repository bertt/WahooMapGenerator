use clap::Parser;
use wahoo_map_generator::cli::{Cli, run};
use wahoo_map_generator::download::HttpFetcher;

fn main() -> anyhow::Result<()> {
    run(Cli::parse(), &HttpFetcher, &mut std::io::stdout())
}
