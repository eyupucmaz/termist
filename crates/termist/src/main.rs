use clap::Parser;

/// terminal istanbul — mission control for your coding agents
#[derive(Parser)]
#[command(name = "termist", version)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
