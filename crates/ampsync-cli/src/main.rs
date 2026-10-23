//! `ampsync` command-line interface.
//!
//! Output conventions: results go to stdout, progress and messages to stderr. Exit codes are
//! listed in `exit`. Ctrl+C stops a download cleanly (exit code 1); a second press quits at once.

mod cli;
mod commands;
mod exit;
mod get;
mod human;
mod paths;
mod progress;
mod session_check;
mod signin;
#[cfg(feature = "webview")]
mod signin_webview;
#[cfg(not(feature = "webview"))]
#[path = "signin_webview_stub.rs"]
mod signin_webview;
mod space;
mod tree;

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
        Command::Get(a) => get::get(a).await,
        Command::Purge(a) => commands::purge(a).await,
    };
    ExitCode::from(code)
}
