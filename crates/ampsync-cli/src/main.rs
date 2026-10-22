//! `ampsync` command-line interface.

mod cli;
mod commands;
mod exit;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};

#[tokio::main]
async fn main() -> ExitCode {
    // clap exits with status 2 on usage errors, matching `exit::USAGE`.
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Login(a) => commands::login(a).await,
        Command::Logout(a) => commands::logout(a).await,
        Command::Sessions => commands::sessions().await,
        Command::Ls(a) => commands::ls(a).await,
        Command::Get(a) => commands::get(a).await,
    };
    ExitCode::from(code)
}
