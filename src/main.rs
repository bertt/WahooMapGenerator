use clap::Parser;
use wahoo_tile_generator::cli::{Cli, run};
use wahoo_tile_generator::download::HttpFetcher;

fn main() -> anyhow::Result<()> {
    run(Cli::parse(), &HttpFetcher, &mut std::io::stdout())
}
