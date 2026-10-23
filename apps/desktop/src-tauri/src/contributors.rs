//! The contributor list is generated at build time (`build.rs`, D12) and embedded.

const EMBEDDED: &str = include_str!(concat!(env!("OUT_DIR"), "/contributors.json"));

/// `{ "repo": "owner/name", "items": [ { login, name, avatar, contributions, url } ] }`
pub fn load() -> serde_json::Value {
    serde_json::from_str(EMBEDDED).unwrap_or_else(|_| serde_json::json!({ "repo": "", "items": [] }))
}

/// One contributor as the startup splash shows them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CreditPerson {
    pub login: String,
    pub avatar: Option<String>,
}

/// "Built by @login and N contributors": the person with the most commits, up to three faces,
/// and how many others there are. `None` when the list is empty (a local build), so the line is
/// simply not shown.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Credit {
    pub top: String,
    pub others: usize,
    pub people: Vec<CreditPerson>,
}

pub fn credit(list: &serde_json::Value) -> Option<Credit> {
    let items = list.get("items")?.as_array()?;
    let mut people: Vec<(u64, CreditPerson)> = items
        .iter()
        .filter_map(|c| {
            let login = c.get("login")?.as_str()?.trim();
            if login.is_empty() {
                return None;
            }
            Some((
                c.get("contributions")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                CreditPerson {
                    login: login.to_string(),
                    avatar: c.get("avatar").and_then(|a| a.as_str()).map(str::to_string),
                },
            ))
        })
        .collect();
    if people.is_empty() {
        return None;
    }
    // Stable: ties keep the order of the list (GitHub already sorts by commits).
    people.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    let others = people.len() - 1;
    let top = people[0].1.login.clone();
    Some(Credit {
        top,
        others,
        people: people.into_iter().take(3).map(|(_, p)| p).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_list_no_credit() {
        assert_eq!(credit(&json!({ "repo": "", "items": [] })), None);
        assert_eq!(credit(&json!({})), None);
        assert_eq!(credit(&json!({ "items": [ { "login": "  " } ] })), None);
    }

    #[test]
    fn top_contributor_by_commits_and_the_rest_counted() {
        let list = json!({ "items": [
            { "login": "ann", "contributions": 3, "avatar": "data:image/png;base64,AA" },
            { "login": "bob", "contributions": 40 },
            { "login": "cy", "contributions": 7 },
            { "login": "di", "contributions": 1 },
        ]});
        let c = credit(&list).unwrap();
        assert_eq!(c.top, "bob");
        assert_eq!(c.others, 3);
        let faces: Vec<_> = c.people.iter().map(|p| p.login.as_str()).collect();
        assert_eq!(faces, ["bob", "cy", "ann"]);
        assert_eq!(c.people[2].avatar.as_deref(), Some("data:image/png;base64,AA"));
    }

    #[test]
    fn a_single_contributor_has_no_others() {
        let c = credit(&json!({ "items": [ { "login": "solo", "contributions": 2 } ] })).unwrap();
        assert_eq!((c.top.as_str(), c.others), ("solo", 0));
    }

    #[test]
    fn the_embedded_list_parses_whatever_it_holds() {
        // A build with no repository embeds an empty list; one with a repository embeds its people.
        let embedded = load();
        let _ = credit(&embedded);
    }
}
