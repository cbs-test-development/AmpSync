//! Process exit codes. Part of the CLI contract (docs/ARCHITECTURE.md).
#![allow(dead_code)] // contract table: constants are used as the commands are implemented

/// Everything succeeded.
pub const OK: u8 = 0;
/// Any other error.
pub const ERROR: u8 = 1;
/// Bad command line (clap also exits with 2 for usage errors).
pub const USAGE: u8 = 2;
/// The session expired mid-job; rerunning the same command resumes.
pub const AUTH_EXPIRED: u8 = 3;
/// The job finished but some files failed.
pub const SOME_FAILED: u8 = 4;
