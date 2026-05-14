mod cli;
mod cloud;
mod db;
mod install;
mod log;
mod paths;
mod record;
mod statusline;
mod transcript;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();
    let code = cli.run();
    std::process::exit(code);
}
