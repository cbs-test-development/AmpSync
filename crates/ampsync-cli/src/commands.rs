//! Subcommand implementations. Each returns the process exit code (see `exit`).

use crate::cli::{GetArgs, LoginArgs, LogoutArgs, LsArgs};

/// `ampsync login`: obtain a session (webview, then Edge/Chrome, then paste unless `--method`
/// says otherwise) and save it to the keychain.
#[allow(unused_variables)]
pub async fn login(args: LoginArgs) -> u8 {
    todo!()
}

/// `ampsync logout <host>`.
#[allow(unused_variables)]
pub async fn logout(args: LogoutArgs) -> u8 {
    todo!()
}

/// `ampsync sessions`: print stored hosts.
pub async fn sessions() -> u8 {
    todo!()
}

/// `ampsync ls <url> [--depth N]`.
#[allow(unused_variables)]
pub async fn ls(args: LsArgs) -> u8 {
    todo!()
}

/// `ampsync get <url> <dest> ...`: plan, confirm unless `--yes`, download. Exit codes: 0 ok,
/// 3 auth expired (state kept; rerun resumes), 4 some files failed.
#[allow(unused_variables)]
pub async fn get(args: GetArgs) -> u8 {
    todo!()
}
