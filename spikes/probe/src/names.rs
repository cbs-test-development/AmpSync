//! Windows file-name legality checks (used by the recursive-count experiment).

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

pub fn check_name(name: &str) -> NameIssues {
    let illegal_chars = name.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20);
    let trailing_dot_space = name.ends_with('.') || name.ends_with(' ');
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit() && stem.as_bytes()[3] != b'0');
    NameIssues { illegal_chars, reserved, trailing_dot_space }
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
}
