//! What the screen shows and what a click means, as plain data with no drawing.
//!
//! Invariants: nothing here touches the store or egui; every click is turned into a list of
//! [`Step`]s the screen then runs in order, so what a click does can be tested without a window.

use craft_ext_core::id::{AppId, ExtensionId};
use craft_ext_core::manifest::Kind;
use craft_ext_core::tier::Tier;

use super::source::Row;

/// One change to the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    SetEnabled { app: AppId, id: ExtensionId, on: bool },
    Wear { app: AppId, id: Option<ExtensionId> },
    Remove { id: ExtensionId },
}

/// What a click on `app` in a theme's "Use in" menu does: take the theme off if the app wears it,
/// otherwise switch it on for the app where needed and have the app wear it.
pub fn wear_steps(row: &Row, app: &AppId) -> Vec<Step> {
    if !row.apps.contains(app) {
        return Vec::new();
    }
    if row.worn.contains(app) {
        return vec![Step::Wear { app: app.clone(), id: None }];
    }
    let mut steps = Vec::new();
    if !row.enabled.contains(app) {
        steps.push(Step::SetEnabled { app: app.clone(), id: row.id.clone(), on: true });
    }
    steps.push(Step::Wear { app: app.clone(), id: Some(row.id.clone()) });
    steps
}

/// What "Use in all apps" does: every app that does not yet wear the theme, in app id order.
pub fn everywhere_steps(row: &Row) -> Vec<Step> {
    let mut apps: Vec<&AppId> = row.apps.iter().filter(|a| !row.worn.contains(*a)).collect();
    apps.sort();
    apps.dedup();
    apps.into_iter().flat_map(|app| wear_steps(row, app)).collect()
}

/// What switching an extension on or off for one app does.
pub fn enable_steps(row: &Row, app: &AppId) -> Vec<Step> {
    if !row.apps.contains(app) {
        return Vec::new();
    }
    vec![Step::SetEnabled { app: app.clone(), id: row.id.clone(), on: !row.enabled.contains(app) }]
}

fn by_name(a: &&Row, b: &&Row) -> std::cmp::Ordering {
    a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id))
}

/// Themes the Themes tab can draw (their colours could be read), in a stable order.
pub fn themes(rows: &[Row]) -> Vec<&Row> {
    let mut out: Vec<&Row> = rows.iter().filter(|r| r.kind == Kind::Theme && r.palette.is_some()).collect();
    out.sort_by(by_name);
    out
}

/// All rows, in a stable order.
pub fn ordered(rows: &[Row]) -> Vec<&Row> {
    let mut out: Vec<&Row> = rows.iter().collect();
    out.sort_by(by_name);
    out
}

/// The name of an app for a person: its catalogue name, or the id itself for an app this launcher
/// does not know.
pub fn app_name(id: &AppId) -> String {
    artcraft_launcher_core::CraftApp::by_id(id.as_str()).map_or_else(|| id.as_str().to_string(), |a| a.name.to_string())
}

/// The apps wearing a theme, by name and in a stable order, or `None` when no app does.
pub fn in_use_line(row: &Row) -> Option<String> {
    let mut names: Vec<String> = row.worn.iter().map(app_name).collect();
    names.sort();
    if names.is_empty() { None } else { Some(names.join(", ")) }
}

/// The words that say where an extension came from, in the current language.
pub fn trust_label(tier: Tier) -> &'static str {
    match tier {
        Tier::FirstParty => tl!("Made by ArtCraft"),
        Tier::FromFile => tl!("Added from a file"),
        Tier::ReviewedThirdParty => tl!("From an outside publisher"),
    }
}

/// The word for what an extension is, in the current language.
pub fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Theme => tl!("Theme"),
        Kind::Plugin => tl!("Plug-in"),
        Kind::Resource => tl!("Resource"),
    }
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
