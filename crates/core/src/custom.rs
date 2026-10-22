//! Third-party apps: GitHub repositories the user added by hand so the launcher can keep them
//! up to date.
//!
//! ArtCraft provides **no support** for these apps and does not vouch for them. The launcher only
//! does for them what it does for the Craft App Suite: read the repository's GitHub releases,
//! pick the right file for this machine, verify it against the release's `SHA256SUMS.txt`, and
//! install it. Downloads still only come from `https://github.com/<owner>/<repo>/releases/`.
//!
//! This module is pure data and logic (no I/O), so it builds for wasm like the rest of `core`.
//! The list the user keeps is a [`CustomList`]; [`set_custom`] turns it into the apps every other
//! part of the launcher sees through [`all_apps`] and [`CraftApp::by_id`].
//!
//! A [`CraftApp`] holds `&'static str`s. A third-party app is built once per repository, with
//! its strings leaked on purpose, and kept for the life of the process: at most [`MAX_APPS`]
//! (plus whatever was added and removed in the same session, which is bounded by the same cap
//! per add) so the cost is a few hundred bytes.

use std::sync::{Mutex, PoisonError, RwLock};

use serde::{Deserialize, Serialize};

use crate::catalog::{CraftApp, GITHUB_OWNER, LAUNCHER_ID, SUITE};

/// The most third-party apps the launcher keeps.
pub const MAX_APPS: usize = 12;
/// The largest saved list [`CustomList::parse`] reads.
pub const MAX_LIST_BYTES: usize = 64 * 1024;

/// One app the user added, as saved on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomApp {
    /// GitHub `owner/repo`, as GitHub spells it.
    pub repo: String,
    /// Unix seconds when it was added (0 when unknown).
    #[serde(default)]
    pub added_at: i64,
}

/// The saved list of third-party apps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomList {
    pub apps: Vec<CustomApp>,
}

/// Why a repository can't be added. The UI turns each into a sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddError {
    /// Not `owner/repo` or a `https://github.com/owner/repo` link.
    NotARepo,
    /// It's one of ArtCraft's own repositories, which the suite already covers.
    ArtCraft,
    /// Its name is already used by a Craft App, ArtCraft Launcher or another added app.
    NameTaken,
    /// The list is full.
    TooMany,
    /// Already in the list.
    AlreadyAdded,
}

impl CustomList {
    /// Reads a saved list. Anything unreadable, oversized or invalid gives an empty or shorter
    /// list, never an error: a damaged file must not stop the launcher from starting.
    pub fn parse(bytes: &[u8]) -> Self {
        if bytes.len() > MAX_LIST_BYTES {
            return Self::default();
        }
        let Ok(list) = serde_json::from_slice::<Self>(bytes) else { return Self::default() };
        let mut clean = Self::default();
        for app in list.apps {
            // `add` applies every rule again, so a hand-edited file can't smuggle anything in.
            let _ = clean.add(&app.repo, app.added_at);
        }
        clean
    }

    /// Serialises the list as pretty-printed JSON for saving, or the serde error as text.
    pub fn to_json(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec_pretty(self).map_err(|e| e.to_string())
    }

    /// Adds `input` (`owner/repo` or a GitHub link), returning the repo as stored.
    pub fn add(&mut self, input: &str, now: i64) -> Result<String, AddError> {
        let repo = parse_repo(input).ok_or(AddError::NotARepo)?;
        let (owner, name) = split(&repo).ok_or(AddError::NotARepo)?;
        if owner.eq_ignore_ascii_case(GITHUB_OWNER) {
            return Err(AddError::ArtCraft);
        }
        if self.apps.iter().any(|a| a.repo.eq_ignore_ascii_case(&repo)) {
            return Err(AddError::AlreadyAdded);
        }
        if self.apps.len() >= MAX_APPS {
            return Err(AddError::TooMany);
        }
        let id = id_for(name);
        if reserved_id(&id) || self.apps.iter().any(|a| split(&a.repo).is_some_and(|(_, n)| id_for(n) == id)) {
            return Err(AddError::NameTaken);
        }
        self.apps.push(CustomApp { repo: repo.clone(), added_at: now });
        Ok(repo)
    }

    /// Removes the app with this id. Returns whether there was one.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.apps.len();
        self.apps.retain(|a| split(&a.repo).is_none_or(|(_, n)| id_for(n) != id));
        self.apps.len() != before
    }
}

/// `owner/repo` from `owner/repo` or a `https://github.com/owner/repo[/...]` link; `None` when it
/// isn't a repository GitHub could have. Case is kept (GitHub ignores it, people read it).
pub fn parse_repo(input: &str) -> Option<String> {
    let s = input.trim();
    let link = s.strip_prefix("https://github.com/").or_else(|| s.strip_prefix("https://www.github.com/"));
    let from_link = link.is_some();
    // A link may carry a query and a path after the repo (`/releases/tag/v1?x=1`); a bare
    // `owner/repo` carries nothing.
    let s = if from_link { link.unwrap_or(s).split(['?', '#']).next().unwrap_or_default() } else { s };
    let s = s.trim_end_matches('/');
    let mut parts = s.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    if !from_link && parts.next().is_some() {
        return None;
    }
    let name = name.strip_suffix(".git").unwrap_or(name);
    valid_owner(owner).then_some(())?;
    valid_name(name).then_some(())?;
    Some(format!("{owner}/{name}"))
}

/// Whether `s` is a legal GitHub owner name: 1 to 39 letters, digits or hyphens, with no hyphen at either end.
fn valid_owner(s: &str) -> bool {
    (1..=39).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !s.starts_with('-') && !s.ends_with('-')
}

/// Whether `s` is a legal GitHub repository name: 1 to 100 letters, digits, `-`, `_` or `.`, and not `.` or `..`.
fn valid_name(s: &str) -> bool {
    (1..=100).contains(&s.len()) && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Splits `owner/name` into its two parts, or `None` when there is no slash.
fn split(repo: &str) -> Option<(&str, &str)> {
    repo.split_once('/')
}

/// The machine name an app is known by: the repository name, lower-case. It is also the stem its
/// release files are expected to carry (`<id>-<version>-<os>-<arch>.<ext>`).
pub fn id_for(repo_name: &str) -> String {
    repo_name.to_ascii_lowercase()
}

/// Whether `id` belongs to the launcher or a Craft App Suite app (including its former ids), so a third-party app can't take it.
fn reserved_id(id: &str) -> bool {
    id == LAUNCHER_ID || SUITE.iter().any(|a| a.id == id || a.former_ids.contains(&id))
}

// ---- the registry -------------------------------------------------------------------------

/// The apps in use right now.
static ACTIVE: RwLock<Vec<&'static CraftApp>> = RwLock::new(Vec::new());
/// Every app ever built this run, so adding one back doesn't leak it twice.
static BUILT: Mutex<Vec<&'static CraftApp>> = Mutex::new(Vec::new());

/// Builds the `CraftApp` for a registered repository, reusing the one built earlier this run if there is one.
fn build(repo: &str) -> Option<&'static CraftApp> {
    let (owner, name) = split(repo)?;
    let mut built = BUILT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(app) = built.iter().find(|a| a.repo.eq_ignore_ascii_case(repo)).copied() {
        return Some(app);
    }
    // Leaked once per repository on purpose: see the module docs.
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    let grey = [0x8a, 0x8a, 0x8a];
    let app: &'static CraftApp = Box::leak(Box::new(CraftApp {
        id: leak(id_for(name)),
        name: leak(name.to_string()),
        tagline: leak(format!("{owner}/{name}")),
        color: grey,
        ink_light: [0x55, 0x55, 0x55],
        ink_dark: [0xb5, 0xb5, 0xb5],
        repo: leak(repo.to_string()),
        former_ids: &[],
        former_names: &[],
        third_party: true,
    }));
    built.push(app);
    Some(app)
}

/// Makes `list` the third-party apps the launcher knows. Returns them in list order.
pub fn set_custom(list: &CustomList) -> Vec<&'static CraftApp> {
    let apps: Vec<&'static CraftApp> = list.apps.iter().filter_map(|a| build(&a.repo)).collect();
    *ACTIVE.write().unwrap_or_else(PoisonError::into_inner) = apps.clone();
    apps
}

/// The third-party apps, in the order they were added.
pub fn custom_apps() -> Vec<&'static CraftApp> {
    ACTIVE.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The Craft App Suite followed by the third-party apps.
pub fn all_apps() -> Vec<&'static CraftApp> {
    SUITE.iter().chain(custom_apps()).collect()
}

/// A third-party app by id.
pub fn custom_by_id(id: &str) -> Option<&'static CraftApp> {
    ACTIVE.read().unwrap_or_else(PoisonError::into_inner).iter().find(|a| a.id == id).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repos_come_from_names_and_links() {
        assert_eq!(parse_repo("octo/tool").as_deref(), Some("octo/tool"));
        assert_eq!(parse_repo("  https://github.com/octo/tool/releases/tag/v1?x=1  ").as_deref(), Some("octo/tool"));
        assert_eq!(parse_repo("https://github.com/octo/tool.git").as_deref(), Some("octo/tool"));
        assert_eq!(parse_repo("octo/tool.git").as_deref(), Some("octo/tool"));
    }

    #[test]
    fn things_that_are_not_repos_are_refused() {
        for bad in [
            "",
            "octo",
            "octo/",
            "/tool",
            "octo/tool/extra",
            "-octo/tool",
            "octo-/tool",
            "octo/..",
            "octo/a b",
            "octo/a\u{0}b",
            "http://github.com/octo/tool",
            "https://evil.example/octo/tool",
            "octo/too?l",
            "o c/tool",
            "octo\\tool",
        ] {
            assert_eq!(parse_repo(bad), None, "{bad:?}");
        }
        assert_eq!(parse_repo(&format!("{}/tool", "a".repeat(40))), None);
        assert_eq!(parse_repo(&format!("octo/{}", "a".repeat(101))), None);
    }

    #[test]
    fn artcraft_repos_and_taken_names_are_refused() {
        let mut list = CustomList::default();
        assert_eq!(list.add("storytold/photocraft", 0), Err(AddError::ArtCraft));
        assert_eq!(list.add("Storytold/newapp", 0), Err(AddError::ArtCraft));
        assert_eq!(list.add("octo/PhotoCraft", 0), Err(AddError::NameTaken));
        assert_eq!(list.add("octo/printcraft", 0), Err(AddError::NameTaken));
        assert_eq!(list.add("octo/artcraft-launcher", 0), Err(AddError::NameTaken));
        assert_eq!(list.add("octo/tool", 5).as_deref(), Ok("octo/tool"));
        assert_eq!(list.add("OCTO/Tool", 0), Err(AddError::AlreadyAdded));
        assert_eq!(list.add("other/tool", 0), Err(AddError::NameTaken));
        assert_eq!(list.add("nonsense", 0), Err(AddError::NotARepo));
    }

    #[test]
    fn the_list_is_capped_and_items_can_be_removed() {
        let mut list = CustomList::default();
        for i in 0..MAX_APPS {
            assert!(list.add(&format!("octo/tool{i}"), 0).is_ok());
        }
        assert_eq!(list.add("octo/one-too-many", 0), Err(AddError::TooMany));
        assert!(list.remove("tool3"));
        assert!(!list.remove("tool3"));
        assert_eq!(list.apps.len(), MAX_APPS - 1);
        assert!(list.add("octo/one-too-many", 0).is_ok());
    }

    #[test]
    fn a_damaged_or_hand_edited_file_never_gets_past_the_rules() {
        assert_eq!(CustomList::parse(b"{not json"), CustomList::default());
        assert_eq!(CustomList::parse(&vec![b' '; MAX_LIST_BYTES + 1]), CustomList::default());
        let sneaky = br#"{"apps":[{"repo":"storytold/photocraft"},{"repo":"https://evil.example/x/y"},{"repo":"octo/tool","added_at":9},{"repo":"octo/tool"}]}"#;
        let list = CustomList::parse(sneaky);
        assert_eq!(list.apps, vec![CustomApp { repo: "octo/tool".into(), added_at: 9 }]);
        let again = CustomList::parse(&list.to_json().unwrap());
        assert_eq!(again, list);
    }

    #[test]
    fn registered_apps_are_third_party_and_found_by_id() {
        let mut list = CustomList::default();
        list.add("octo/RegTool", 0).unwrap();
        let apps = set_custom(&list);
        assert_eq!(apps.len(), 1);
        let app = apps[0];
        assert!(app.third_party);
        assert_eq!((app.id, app.name, app.repo), ("regtool", "RegTool", "octo/RegTool"));
        assert_eq!(app.releases_url(), "https://github.com/octo/RegTool/releases");
        assert!(std::ptr::eq(CraftApp::by_id("regtool").unwrap(), app));
        assert!(all_apps().len() > SUITE.len());
        // Adding it back reuses the same app instead of building another.
        let again = set_custom(&list);
        assert!(std::ptr::eq(again[0], app));
        set_custom(&CustomList::default());
        assert!(CraftApp::by_id("regtool").is_none());
    }
}
