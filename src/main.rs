use clap::Parser;
use wahoo_map::cli::{Cli, run};
use wahoo_map::download::HttpFetcher;

fn main() -> anyhow::Result<()> {
    run(Cli::parse(), &HttpFetcher, &mut std::io::stdout())
}
