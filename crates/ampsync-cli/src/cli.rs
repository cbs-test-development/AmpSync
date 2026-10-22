//! Command-line definition.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "ampsync",
    version,
    about = "Download large SharePoint folders reliably, with resume and verification"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign in to a SharePoint site and store the session in the OS keychain
    Login(LoginArgs),
    /// Forget the stored session for a host
    Logout(LogoutArgs),
    /// List stored sessions
    Sessions,
    /// List the contents of a remote folder
    Ls(LsArgs),
    /// Download a remote folder (re-running resumes and skips unchanged files)
    Get(GetArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Method {
    /// Embedded webview window
    Webview,
    /// Installed Microsoft Edge with a throwaway profile
    Edge,
    /// Installed Google Chrome with a throwaway profile
    Chrome,
    /// Paste a Cookie header
    Paste,
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// A SharePoint link (the tenant host is taken from it)
    pub url: String,
    /// Sign-in method; default tries webview, then Edge/Chrome, then paste
    #[arg(long, value_enum)]
    pub method: Option<Method>,
    /// File containing a Cookie header to use with `--method paste` (instead of typing it)
    #[arg(long, value_name = "FILE")]
    pub cookie_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct LogoutArgs {
    /// Host to sign out of, e.g. contoso.sharepoint.com
    pub host: String,
}

#[derive(Debug, Args)]
pub struct LsArgs {
    /// A SharePoint folder or sharing link
    pub url: String,
    /// How many folder levels to list (default: 1)
    #[arg(long)]
    pub depth: Option<u32>,
}

#[derive(Debug, Args)]
pub struct GetArgs {
    /// A SharePoint folder or sharing link
    pub url: String,
    /// Local destination folder
    pub dest: PathBuf,
    /// Only download files matching this glob (repeatable)
    #[arg(long, value_name = "GLOB", action = clap::ArgAction::Append)]
    pub include: Vec<String>,
    /// Skip files matching this glob (repeatable)
    #[arg(long, value_name = "GLOB", action = clap::ArgAction::Append)]
    pub exclude: Vec<String>,
    /// Simultaneous transfers (1-16)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(1..=16))]
    pub parallel: Option<u8>,
    /// Bandwidth limit in bytes per second; accepts suffixes such as 500K, 20M
    #[arg(long, value_name = "RATE", value_parser = crate::human::parse_rate)]
    pub limit: Option<u64>,
    /// Download files again even if they look unchanged
    #[arg(long)]
    pub redownload: bool,
    /// Delete local files that no longer exist remotely
    #[arg(long)]
    pub prune: bool,
    /// Do not ask for confirmation
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Print one JSON event per line instead of a progress display
    #[arg(long)]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn get_flags_parse() {
        let cli = Cli::try_parse_from([
            "ampsync",
            "get",
            "https://x",
            "out",
            "--include",
            "*.pdf",
            "--include",
            "a/**",
            "--exclude",
            "tmp/**",
            "--parallel",
            "8",
            "--limit",
            "20M",
            "--redownload",
            "--prune",
            "--yes",
            "--json",
        ])
        .unwrap();
        let Command::Get(g) = cli.command else {
            panic!("not get")
        };
        assert_eq!(g.include, ["*.pdf", "a/**"]);
        assert_eq!(g.exclude, ["tmp/**"]);
        assert_eq!(g.parallel, Some(8));
        assert_eq!(g.limit, Some(20 * 1024 * 1024));
        assert!(g.redownload && g.prune && g.yes && g.json);
    }

    #[test]
    fn login_method_values() {
        for (arg, want) in [
            ("webview", Method::Webview),
            ("edge", Method::Edge),
            ("chrome", Method::Chrome),
            ("paste", Method::Paste),
        ] {
            let cli = Cli::try_parse_from(["ampsync", "login", "https://x", "--method", arg]).unwrap();
            let Command::Login(l) = cli.command else {
                panic!("not login")
            };
            assert_eq!(l.method, Some(want));
        }
        assert!(Cli::try_parse_from(["ampsync", "login", "https://x", "--method", "nope"]).is_err());
    }

    #[test]
    fn login_cookie_file_and_ls_depth() {
        let cli = Cli::try_parse_from(["ampsync", "login", "https://x", "--cookie-file", "c.txt"]).unwrap();
        let Command::Login(l) = cli.command else {
            panic!("not login")
        };
        assert_eq!(l.cookie_file.as_deref(), Some(std::path::Path::new("c.txt")));
        let cli = Cli::try_parse_from(["ampsync", "ls", "https://x", "--depth", "3"]).unwrap();
        let Command::Ls(ls) = cli.command else {
            panic!("not ls")
        };
        assert_eq!(ls.depth, Some(3));
    }

    #[test]
    fn bad_limit_is_a_usage_error() {
        let err = Cli::try_parse_from(["ampsync", "get", "u", "d", "--limit", "fast"]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(Cli::try_parse_from(["ampsync", "get", "u", "d", "--limit", "500K"]).is_ok());
    }

    #[test]
    fn missing_arguments_are_usage_errors() {
        for args in [
            vec!["ampsync"],
            vec!["ampsync", "get", "only-url"],
            vec!["ampsync", "logout"],
        ] {
            assert_eq!(Cli::try_parse_from(args).unwrap_err().exit_code(), 2);
        }
    }

    #[test]
    fn parallel_range_enforced() {
        assert!(Cli::try_parse_from(["ampsync", "get", "u", "d", "--parallel", "0"]).is_err());
        assert!(Cli::try_parse_from(["ampsync", "get", "u", "d", "--parallel", "17"]).is_err());
    }
}
