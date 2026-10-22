//! Windows-safe file names.
//!
//! SharePoint allows names that Windows cannot store. [`check_name`] diagnoses a name and
//! [`sanitize`] produces a legal one using visible substitutes (so the user can still recognise
//! the file). The engine applies [`sanitize`] on Windows and records every `original -> local`
//! mapping in `state.db`.

use std::path::{Path, PathBuf};

use crate::plan::RelPath;

/// What is wrong with a name, if anything.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct NameIssues {
    /// Contains one of `<>:"/\|?*` or a control character (< 0x20).
    pub illegal_chars: bool,
    /// Device name such as `CON`, `NUL`, `COM1`, `LPT9` (with or without extension).
    pub reserved: bool,
    /// Ends with a dot or a space.
    pub trailing_dot_space: bool,
}

impl NameIssues {
    pub fn any(&self) -> bool {
        self.illegal_chars || self.reserved || self.trailing_dot_space
    }
}

/// Diagnose a single path component (not a whole path).
pub fn check_name(name: &str) -> NameIssues {
    let illegal_chars = name
        .chars()
        .any(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20);
    let trailing_dot_space = name.ends_with('.') || name.ends_with(' ');
    NameIssues {
        illegal_chars,
        reserved: is_reserved(name),
        trailing_dot_space,
    }
}

fn is_reserved(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

/// Visible full-width stand-in for an illegal character.
fn substitute(c: char) -> Option<char> {
    Some(match c {
        '<' => '\u{FF1C}',
        '>' => '\u{FF1E}',
        ':' => '\u{FF1A}',
        '"' => '\u{FF02}',
        '|' => '\u{FF5C}',
        '?' => '\u{FF1F}',
        '*' => '\u{FF0A}',
        '/' => '\u{FF0F}',
        '\\' => '\u{FF3C}',
        _ => return None,
    })
}

/// Turn one path component into a legal Windows name.
///
/// - `<>:"/\|?*` become their full-width look-alikes (`<`->`＜`, `?`->`？`, ...).
/// - Control characters become `_`.
/// - Trailing dots become `．` (full-width) and trailing spaces become `␣` (U+2423).
/// - Reserved device names (`CON`, `NUL.txt`, `COM1`, ...) get a leading `_`.
/// - An empty result becomes `_`.
///
/// Idempotent: `sanitize(&sanitize(x)) == sanitize(x)`, and legal names are returned unchanged.
/// The mapping is applied on every platform when called; the engine decides when to call it.
pub fn sanitize(name: &str) -> String {
    let mut out: Vec<char> = name
        .chars()
        .map(|c| match substitute(c) {
            Some(s) => s,
            None if (c as u32) < 0x20 => '_',
            None => c,
        })
        .collect();
    // Replace the whole trailing run of dots and spaces.
    let mut end = out.len();
    while end > 0 && matches!(out[end - 1], '.' | ' ') {
        end -= 1;
        out[end] = if out[end] == '.' { '\u{FF0E}' } else { '\u{2423}' };
    }
    let mut s: String = out.into_iter().collect();
    if s.is_empty() {
        return "_".into();
    }
    if is_reserved(&s) {
        s.insert(0, '_');
    }
    s
}

/// [`sanitize`] applied to every component of a relative path.
pub fn sanitize_rel_path(rel: &RelPath) -> RelPath {
    let joined = rel.components().map(sanitize).collect::<Vec<_>>().join("/");
    RelPath::new(joined).expect("sanitised components stay valid")
}

/// On Windows, prefix an absolute path with `\\?\` (or `\\?\UNC\`) so paths over 260 characters work.
/// Relative paths and paths that already have the prefix are returned unchanged. No-op elsewhere.
pub fn long_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let s = path.as_os_str().to_string_lossy();
        if s.starts_with(r"\\?\") || !path.is_absolute() {
            return path.to_path_buf();
        }
        let s = s.replace('/', "\\");
        if let Some(unc) = s.strip_prefix(r"\\") {
            return PathBuf::from(format!(r"\\?\UNC\{unc}"));
        }
        PathBuf::from(format!(r"\\?\{s}"))
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_issues() {
        assert!(!check_name("Report 2024 (final).docx").any());
        assert!(check_name("a:b.txt").illegal_chars);
        assert!(check_name("what?.txt").illegal_chars);
        assert!(check_name("tab\there").illegal_chars);
        assert!(check_name("trailing.").trailing_dot_space);
        assert!(check_name("trailing ").trailing_dot_space);
        assert!(check_name("CON").reserved);
        assert!(check_name("nul.txt").reserved);
        assert!(check_name("COM1.log").reserved);
        assert!(!check_name("COM0").reserved);
        assert!(!check_name("CONSOLE.txt").reserved);
        assert!(!check_name("comment.txt").reserved);
    }

    #[test]
    fn legal_names_unchanged() {
        for n in [
            "Report 2024 (final).docx",
            "a.b.c",
            ".gitignore",
            "ünï.txt",
            "x y",
        ] {
            assert_eq!(sanitize(n), n);
        }
    }

    #[test]
    fn illegal_chars_replaced_visibly() {
        assert_eq!(sanitize("a:b?.txt"), "a\u{FF1A}b\u{FF1F}.txt");
        assert_eq!(sanitize("<>\"|*"), "\u{FF1C}\u{FF1E}\u{FF02}\u{FF5C}\u{FF0A}");
        assert_eq!(sanitize("a\\b/c"), "a\u{FF3C}b\u{FF0F}c");
        assert_eq!(sanitize("tab\there"), "tab_here");
    }

    #[test]
    fn trailing_dots_and_spaces() {
        assert_eq!(sanitize("end."), "end\u{FF0E}");
        assert_eq!(sanitize("end "), "end\u{2423}");
        assert_eq!(sanitize("end. ."), "end\u{FF0E}\u{2423}\u{FF0E}");
        assert!(!check_name(&sanitize("end. .")).any());
        assert_eq!(sanitize("..."), "\u{FF0E}\u{FF0E}\u{FF0E}");
    }

    #[test]
    fn reserved_and_empty() {
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("nul.txt"), "_nul.txt");
        assert_eq!(sanitize("COM1"), "_COM1");
        assert_eq!(sanitize("COM0"), "COM0");
        assert_eq!(sanitize(""), "_");
    }

    #[test]
    fn sanitize_is_idempotent_and_always_legal() {
        for n in [
            "a:b", "CON", "x.", "x ", "", "..", "a\u{1}b", "q?", "LPT3.log", "ok",
        ] {
            let s = sanitize(n);
            assert!(!check_name(&s).any(), "{n:?} -> {s:?}");
            assert_eq!(sanitize(&s), s, "{n:?}");
        }
    }

    #[test]
    fn rel_path_sanitised_per_component() {
        let r = RelPath::new("dir:1/CON/file?.txt").unwrap();
        assert_eq!(
            sanitize_rel_path(&r).as_str(),
            "dir\u{FF1A}1/_CON/file\u{FF1F}.txt"
        );
    }

    #[cfg(windows)]
    #[test]
    fn long_path_prefix() {
        assert_eq!(long_path(Path::new(r"C:\a\b")), PathBuf::from(r"\\?\C:\a\b"));
        assert_eq!(long_path(Path::new(r"\\?\C:\a")), PathBuf::from(r"\\?\C:\a"));
        assert_eq!(
            long_path(Path::new(r"\\srv\share\x")),
            PathBuf::from(r"\\?\UNC\srv\share\x")
        );
        assert_eq!(long_path(Path::new(r"rel\x")), PathBuf::from(r"rel\x"));
    }
}
