//! `ampsync ls` output: an indented tree with sizes and a totals line.

use std::collections::BTreeMap;

use crate::human;

/// One remote item, path relative to the listed folder (`/`-separated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub path: String,
    /// `Some(size)` for a file, `None` for a folder.
    pub size: Option<u64>,
}

#[derive(Default)]
struct Dir {
    name: String,
    dirs: BTreeMap<String, Dir>,
    files: BTreeMap<String, (String, u64)>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Totals {
    pub files: u64,
    pub folders: u64,
    pub bytes: u64,
}

impl Dir {
    fn entry(&mut self, name: &str) -> &mut Dir {
        let d = self.dirs.entry(name.to_lowercase()).or_default();
        if d.name.is_empty() {
            d.name = name.to_string();
        }
        d
    }

    fn totals(&self) -> Totals {
        let mut t = Totals::default();
        for (_, size) in self.files.values() {
            t.files += 1;
            t.bytes += size;
        }
        for d in self.dirs.values() {
            let s = d.totals();
            t.files += s.files;
            t.folders += s.folders + 1;
            t.bytes += s.bytes;
        }
        t
    }
}

fn build(items: &[Item]) -> Dir {
    let mut root = Dir::default();
    for item in items {
        let parts: Vec<&str> = item.path.split('/').filter(|p| !p.is_empty()).collect();
        let Some((last, parents)) = parts.split_last() else {
            continue;
        };
        let mut cur = &mut root;
        for p in parents {
            cur = cur.entry(p);
        }
        match item.size {
            Some(size) => {
                cur.files.insert(last.to_lowercase(), ((*last).to_string(), size));
            }
            None => {
                cur.entry(last);
            }
        }
    }
    root
}

/// Render `items` as lines of text, listing `depth` levels (1 = just the folder's contents).
/// Folders show the size and file count of everything below them, even past `depth`.
/// Returns the lines and the totals for the whole listing.
pub fn render(root_name: &str, items: &[Item], depth: u32) -> (Vec<String>, Totals) {
    let root = build(items);
    let totals = root.totals();
    let mut rows: Vec<(String, String)> = Vec::new();
    rows.push((
        format!("{root_name}/"),
        format!(
            "{}  ({})",
            human::bytes(totals.bytes),
            human::count(totals.files, "file", "files")
        ),
    ));
    walk(&root, 1, depth.max(1), &mut rows);
    let width = rows
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0)
        .min(70);
    let lines = rows
        .into_iter()
        .map(|(label, size)| {
            let pad = width.saturating_sub(label.chars().count());
            format!("{label}{}  {size}", " ".repeat(pad))
        })
        .collect();
    (lines, totals)
}

fn walk(dir: &Dir, level: u32, max: u32, rows: &mut Vec<(String, String)>) {
    let indent = "  ".repeat(level as usize);
    for d in dir.dirs.values() {
        let t = d.totals();
        rows.push((
            format!("{indent}{}/", d.name),
            format!(
                "{}  ({})",
                human::bytes(t.bytes),
                human::count(t.files, "file", "files")
            ),
        ));
        if level < max {
            walk(d, level + 1, max, rows);
        }
    }
    for (name, size) in dir.files.values() {
        rows.push((format!("{indent}{name}"), human::bytes(*size)));
    }
}

/// `3 files, 2 folders, 1.5 MiB`.
pub fn totals_line(t: &Totals) -> String {
    format!(
        "{}, {}, {}",
        human::count(t.files, "file", "files"),
        human::count(t.folders, "folder", "folders"),
        human::bytes(t.bytes)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(p: &str, s: u64) -> Item {
        Item {
            path: p.into(),
            size: Some(s),
        }
    }
    fn folder(p: &str) -> Item {
        Item {
            path: p.into(),
            size: None,
        }
    }

    fn sample() -> Vec<Item> {
        vec![
            folder("Drawings"),
            folder("Drawings/Rev A"),
            file("Drawings/Rev A/plan.pdf", 2048),
            file("Drawings/index.txt", 100),
            file("readme.txt", 1024),
            folder("empty"),
        ]
    }

    #[test]
    fn totals_count_everything() {
        let (_, t) = render("Root", &sample(), 1);
        assert_eq!(
            t,
            Totals {
                files: 3,
                folders: 3,
                bytes: 3172
            }
        );
        assert_eq!(totals_line(&t), "3 files, 3 folders, 3.1 KiB");
    }

    #[test]
    fn depth_one_lists_only_top_level_with_rolled_up_sizes() {
        let (lines, _) = render("Root", &sample(), 1);
        let text = lines.join("\n");
        assert!(lines[0].starts_with("Root/"), "{text}");
        assert!(text.contains("  Drawings/"), "{text}");
        assert!(text.contains("2.1 KiB  (2 files)"), "{text}");
        assert!(!text.contains("Rev A"), "{text}");
        assert!(!text.contains("index.txt"), "{text}");
        assert!(text.contains("  readme.txt"), "{text}");
    }

    #[test]
    fn deeper_listing_folders_first_then_files() {
        let (lines, _) = render("Root", &sample(), 3);
        let pos = |needle: &str| {
            lines
                .iter()
                .position(|l| l.contains(needle))
                .unwrap_or(usize::MAX)
        };
        assert!(pos("Drawings/") < pos("Rev A/"));
        assert!(pos("Rev A/") < pos("plan.pdf"));
        assert!(pos("plan.pdf") < pos("index.txt"));
        assert!(pos("empty/") < pos("readme.txt"));
        assert!(lines[pos("plan.pdf")].starts_with("      plan.pdf"));
    }

    #[test]
    fn depth_zero_behaves_like_one() {
        assert_eq!(render("R", &sample(), 0).0, render("R", &sample(), 1).0);
    }

    #[test]
    fn empty_listing() {
        let (lines, t) = render("R", &[], 1);
        assert_eq!(lines.len(), 1);
        assert_eq!(t, Totals::default());
    }
}
