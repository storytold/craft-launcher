//! Published-release selector. A choice changes the release used by future checks, not a running install.
use artcraft_launcher_core::catalog::{CraftApp, LAUNCHER, LAUNCHER_ID};
use artcraft_launcher_core::platform::pick_asset;
use artcraft_launcher_core::protocol::VersionTrack;
use artcraft_launcher_core::release::{Asset, Release, choose_version, is_nightly};
use artcraft_launcher_core::{Os, version};
use egui::{RichText, Ui};

use crate::app::LauncherUi;
use crate::theme::Tokens;
use crate::widgets::{self, Kind};

impl LauncherUi {
    /// The exact selected version, as distinct from following stable or nightly releases.
    pub(crate) fn pinned_version(&self, id: &str) -> Option<&str> {
        let track = if id == LAUNCHER_ID { Some(&self.settings.launcher_version) } else { self.settings.versions.get(id) };
        match track {
            Some(VersionTrack::Pinned(tag)) => Some(tag),
            _ => None,
        }
    }

    /// A lock beside a version opens a confirmation; clicking it never changes the saved choice.
    pub(crate) fn version_lock(&mut self, ui: &mut Ui, id: &str) {
        let Some(tag) = self.pinned_version(id).map(str::to_string) else { return };
        let tip = crate::fmt(tl!("Version pinned to {version}. Click to unlock."), &[("version", &tag)]);
        if widgets::inline_glyph_button(ui, widgets::Glyph::Lock, &tip).clicked() {
            self.dialog = Some(crate::app::Dialog::UnlockVersion { app: id.to_string(), tag });
        }
    }

    /// Unlock only the choice the prompt showed: a newer choice made while it was open survives.
    fn unlock_version(&mut self, id: &str, tag: &str) {
        if self.pinned_version(id) != Some(tag) {
            return;
        }
        if id == LAUNCHER_ID {
            self.settings.launcher_version = VersionTrack::Stable;
        } else {
            self.settings.versions.remove(id);
        }
        self.configure();
        self.check_now_from_ui();
    }

    /// Confirms returning to the latest stable release without directly installing anything.
    pub(crate) fn unlock_version_dialog(&mut self, ui: &mut Ui, id: &str, tag: &str) -> bool {
        let app = if id == LAUNCHER_ID {
            &LAUNCHER
        } else if let Some(app) = CraftApp::by_id(id) {
            app
        } else {
            return true;
        };
        ui.set_width(440.0f32.min((ui.ctx().content_rect().width() - 80.0).max(240.0)));
        ui.heading(tl!("Unlock version?"));
        ui.add_space(8.0);
        ui.add(
            egui::Label::new(crate::fmt(
                tl!("{app} is pinned to {version}. Follow the latest release instead?"),
                &[("app", app.name), ("version", tag)],
            ))
            .wrap(),
        );
        ui.add_space(16.0);
        let (mut cancel, mut unlock) = (false, false);
        ui.horizontal(|ui| {
            cancel = widgets::button(ui, tl!("Cancel"), Kind::Secondary, 100.0).clicked();
            unlock = widgets::button(ui, tl!("Unlock"), Kind::Primary, 100.0).clicked();
        });
        if unlock {
            self.unlock_version(id, tag);
        }
        cancel || unlock
    }

    /// The per-app selector stays in its dialog. The launcher's selector is drawn in Settings.
    pub(crate) fn version_dialog(&mut self, ui: &mut Ui, id: &str) -> bool {
        let Some(app) = CraftApp::by_id(id) else { return true };
        let t = Tokens::get(ui.ctx());
        ui.set_width(480.0f32.min((ui.ctx().content_rect().width() - 80.0).max(240.0)));
        ui.heading(format!("{} · {}", app.name, tl!("Choose version")));
        ui.add_space(8.0);
        let label = ui.label(tl!("Version"));
        // A choice keeps the dialog open: the Install button below follows it.
        self.version_combo(ui, id, app, 440.0, label.id);
        ui.add_space(10.0);
        ui.label(RichText::new(tl!("Your files may not open in older versions.")).color(t.muted));
        ui.label(RichText::new(tl!("Enable automatic installs in Settings to follow new nightly builds.")).color(t.muted));
        ui.add_space(12.0);
        let (mut close, mut install) = (false, false);
        ui.horizontal(|ui| {
            close = widgets::button(ui, tl!("Close"), Kind::Secondary, 100.0).clicked();
            if let Some((label, ready, why)) = self.chosen_install(id) {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let resp = ui.add_enabled_ui(ready, |ui| widgets::button(ui, &label, Kind::Primary, 220.0)).inner;
                    let resp = match why {
                        Some(why) => resp.on_disabled_hover_text(why),
                        None => resp,
                    };
                    install = resp.clicked();
                });
            }
        });
        if install {
            // Installing keeps the new choice. May open the folder dialog in this one's place;
            // `dialogs` keeps it.
            self.version_before = None;
            self.install_app(id);
            return true;
        }
        close
    }

    /// The dialog closed without installing: the choice made in it was only a draft, so put the
    /// one it had before back, and check again so the card shows that release (Open, not Install).
    pub(crate) fn revert_version_choice(&mut self) {
        let Some((id, before)) = self.version_before.take() else { return };
        if self.settings.versions.get(&id).cloned().unwrap_or_default() == before {
            return;
        }
        if before == VersionTrack::Stable {
            self.settings.versions.remove(&id);
        } else {
            self.settings.versions.insert(id, before);
        }
        self.configure();
        self.check_now_from_ui();
    }

    /// The dialog's Install button for the chosen version: its label, whether that exact release has
    /// been checked, would install here ([`Self::version_installs`]) and isn't the installed copy,
    /// and why not when the install would be refused ([`Self::version_refusal`]). `None` when there
    /// is nothing to install (no choice the backend can answer, or a job already running).
    fn chosen_install(&self, id: &str) -> Option<(String, bool, Option<String>)> {
        let entry = self.entry(id)?;
        if !self.caps.manage_apps || entry.job.is_some() {
            return None;
        }
        let release = entry.release();
        let tag = match self.settings.versions.get(id).cloned().unwrap_or_default() {
            VersionTrack::Pinned(tag) => tag,
            VersionTrack::Nightly => match release {
                Some(r) if is_nightly(r) => r.tag.clone(),
                _ if self.checking => return Some((tl!("Checking…").to_string(), false, None)),
                _ => return None,
            },
            VersionTrack::Stable => release.filter(|r| !r.prerelease)?.tag.clone(),
        };
        let installed = entry.installed.as_ref().and_then(|i| i.version.as_ref());
        if installed.is_some() && installed == version::parse(&tag).as_ref() {
            return Some((tl!("Installed").to_string(), false, None));
        }
        let ready = self.version_installs(entry, &tag) == Some(true);
        Some((crate::fmt(tl!("Install version {version}"), &[("version", &tag)]), ready, self.version_refusal(entry, &tag)))
    }

    /// Whether version `tag` of `entry` can be installed now: `None` until the release on screen
    /// is that version (it is still being checked); else whether an install of it would go ahead
    /// here ([`AppEntry::installs_here`](crate::model::AppEntry::installs_here)). The card's
    /// "Install version X" and the version dialog's Install button both ask this.
    pub(crate) fn version_installs(&self, entry: &crate::model::AppEntry, tag: &str) -> Option<bool> {
        entry.release().filter(|r| r.tag == tag).map(|_| entry.installs_here(self.platform))
    }

    /// Why "Install version X" is disabled, for an installed copy the install would refuse to
    /// replace with it (a system binary, a repository package, a format the release doesn't
    /// have). The button can't explain on click, so its hover does.
    pub(crate) fn version_refusal(&self, entry: &crate::model::AppEntry, tag: &str) -> Option<String> {
        (entry.installed.is_some() && self.version_installs(entry, tag) == Some(false))
            .then(|| crate::fmt(tl!("ArtCraft Launcher can't install version {version} over this copy."), &[("version", tag)]))
    }

    pub(crate) fn version_combo(&mut self, ui: &mut Ui, id: &str, app: &CraftApp, width: f32, label: egui::Id) -> bool {
        let t = Tokens::get(ui.ctx());
        let choice =
            if id == LAUNCHER_ID { self.settings.launcher_version.clone() } else { self.settings.versions.get(id).cloned().unwrap_or_default() };
        let list = self.version_lists.get(id).cloned();
        let mut picked = None;
        // The newest stable release that installs here is labelled as the latest, and choosing it
        // follows new releases. Until the list is in, a plain "Latest release" row keeps that choice.
        let latest = list
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .and_then(|releases| choose_version(releases, &VersionTrack::Stable))
            .filter(|r| self.has_version_download(app, r))
            .map(|r| r.tag);
        let latest_label = |tag: &str| crate::fmt(tl!("{version} (latest release)"), &[("version", tag)]);
        let summary = match &choice {
            VersionTrack::Stable => latest.as_deref().map_or_else(|| tl!("Latest release").to_string(), latest_label),
            VersionTrack::Nightly => tl!("Nightly").to_string(),
            VersionTrack::Pinned(tag) => tag.clone(),
        };
        craft_ui::ComboBox::from_id_salt(("version", id))
            .selected_text(summary)
            .width(width)
            .height(390.0)
            .show_ui(ui, |ui| {
                // The combo already scrolls its entire menu. Keep every option, including
                // Nightly, in that one scroll region. Rows touch: with a gap, the selected row's
                // highlight covers only part of its slot and the spacing looks uneven around it.
                ui.spacing_mut().item_spacing.y = 0.0;
                let option_size = egui::vec2(ui.available_width(), 42.0);
                if latest.is_none()
                    && ui.add(egui::Button::selectable(choice == VersionTrack::Stable, tl!("Latest release")).min_size(option_size)).clicked()
                {
                    picked = Some(VersionTrack::Stable);
                }
                let nightly = list
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .is_some_and(|rs| rs.iter().any(|r| is_nightly(r) && self.has_version_download(app, r)));
                // Without a build, the reason rides in the disabled row itself, not a line of its own.
                let label = if nightly { tl!("Nightly") } else { tl!("Nightly (none published yet)") };
                if ui.add_enabled(nightly, egui::Button::selectable(choice == VersionTrack::Nightly, label).min_size(option_size)).clicked() {
                    picked = Some(VersionTrack::Nightly);
                }
                match &list {
                    None => {
                        ui.spinner();
                        ui.label(tl!("Checking…"));
                    }
                    Some(Err(why)) => {
                        ui.label(RichText::new(why).color(t.bad));
                    }
                    Some(Ok(releases)) => {
                        if releases.is_empty() {
                            ui.label(tl!("No release published yet"));
                        }
                        for r in releases {
                            if !self.has_version_download(app, r) {
                                continue;
                            }
                            let pin = VersionTrack::Pinned(r.tag.clone());
                            let (selected, text, track) = if latest.as_ref() == Some(&r.tag) {
                                (choice == VersionTrack::Stable || choice == pin, latest_label(&r.tag), VersionTrack::Stable)
                            } else {
                                (choice == pin, r.tag.clone(), pin)
                            };
                            if self.version_row(ui, selected, text, app, r, option_size).clicked() {
                                picked = Some(track);
                            }
                        }
                    }
                }
            })
            .response
            .labelled_by(label);
        if let Some(track) = picked {
            if id == LAUNCHER_ID {
                self.settings.launcher_version = track;
            } else if track == VersionTrack::Stable {
                self.settings.versions.remove(id);
            } else {
                self.settings.versions.insert(id.to_string(), track);
            }
            // Refresh the checked release before the new main action can install it.
            self.configure();
            self.check_now_from_ui();
            return true;
        }
        false
    }

    /// Launcher downgrades are manual. Only offer the published release selected above.
    pub(crate) fn launcher_version_notes(&self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        if let VersionTrack::Pinned(tag) = &self.settings.launcher_version {
            if let Some(release) =
                self.version_lists.get(LAUNCHER_ID).and_then(|r| r.as_ref().ok()).and_then(|releases| releases.iter().find(|r| &r.tag == tag))
                && self.has_version_download(&LAUNCHER, release)
                && ui.link(tl!("Download selected version")).clicked()
            {
                let url = self
                    .platform
                    .and_then(|p| crate::views::launcher_download(release, p, crate::settings::is_flatpak_sandbox()))
                    .unwrap_or_else(|| release.html_url.clone());
                widgets::open_external_link(ui.ctx(), &url);
            }
            ui.label(RichText::new(tl!("Older launcher versions require a manual reinstall.")).color(t.muted));
        }
        if self.settings.launcher_version == VersionTrack::Nightly {
            ui.label(RichText::new(tl!("Enable automatic installs in Settings to follow new nightly builds.")).color(t.muted));
        }
    }

    /// One release in the version list: its tag, and at the right, muted, the day it came out and
    /// the size of the package this machine would download, each left out when unknown. On the
    /// selected row they take the row's own text colour, as muted grey is unreadable on the
    /// accent. The row stays named by its tag; the date and size are its description.
    fn version_row(&self, ui: &mut Ui, selected: bool, text: String, app: &CraftApp, r: &Release, size: egui::Vec2) -> egui::Response {
        let facts = version_facts(r.published_at.map(crate::views::date_text), self.version_asset(app, r).map(|a| a.size));
        let mut button = egui::Button::selectable(selected, text).min_size(size);
        if !facts.is_empty() {
            let galley = egui::WidgetText::from(RichText::new(&facts).size(12.5).color(egui::Color32::PLACEHOLDER)).into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Body,
            );
            let muted = Tokens::get(ui.ctx()).muted;
            // Painted, not a text atom: a text atom would join the row's name.
            button = button.right_text(egui::Atom::paint(galley.size(), move |ui, args| {
                let at = egui::Align2::LEFT_CENTER.align_size_within_rect(galley.size(), args.rect).min;
                ui.painter().galley(at, galley.clone(), if selected { args.fallback_text_color } else { muted });
            }));
        }
        let response = ui.add(button);
        if !facts.is_empty() {
            ui.ctx().accesskit_node_builder(response.id, |node| node.set_description(facts));
        }
        response
    }

    /// The package this machine would download for `r`: the one the launcher updates itself from
    /// or the app installs or updates from. `None` where the UI can't tell which.
    fn version_asset<'r>(&self, app: &CraftApp, r: &'r Release) -> Option<&'r Asset> {
        if app.id == LAUNCHER_ID {
            return self.launcher_version_asset(r);
        }
        self.install_asset(self.entry(app.id)?, r)
    }

    /// The launcher's own package of `r`: the format the running copy was installed as, which its
    /// self-update downloads. Until the backend knows that, the manual download (on the web, the
    /// only one), except on Linux, where the running copy's format is anyone's guess.
    fn launcher_version_asset<'r>(&self, r: &'r Release) -> Option<&'r Asset> {
        let platform = self.platform?;
        match self.backend.launcher_kind() {
            Some(kind) => pick_asset(r, &[LAUNCHER_ID], platform, &[kind]).map(|(asset, _)| asset),
            None if self.caps.manage_apps && platform.os == Os::Linux => None,
            None => crate::views::launcher_asset(r, platform, crate::settings::is_flatpak_sandbox()),
        }
    }

    fn has_version_download(&self, app: &CraftApp, r: &Release) -> bool {
        let Some(platform) = self.platform else { return false };
        let stems: Vec<&str> = app.stems().collect();
        pick_asset(r, &stems, platform, platform.download_kinds()).is_some_and(|(asset, _)| asset.sha256.is_some() || r.sums_asset().is_some())
    }

    /// The newer release an installed copy is behind without it being an update, for a quiet note
    /// beside its version: a version kept on purpose (its published list, fetched in the
    /// background by [`request_pinned_lists`](Self::request_pinned_lists), has a newer stable
    /// release), or a newer release the launcher can't install over this copy
    /// ([`AppEntry::newer_out_of_reach`](crate::model::AppEntry::newer_out_of_reach)). It is never
    /// an update: no badge, Update all or notification counts it.
    pub(crate) fn behind_latest(&self, entry: &crate::model::AppEntry) -> Option<String> {
        self.kept_behind(entry).or_else(|| self.out_of_reach(entry))
    }

    /// [`AppEntry::newer_out_of_reach`](crate::model::AppEntry::newer_out_of_reach), for an app
    /// following the latest release. Right after another version is chosen, the release on screen
    /// is the old track's until the check finishes, and an install of it is refused for that
    /// reason alone, so the note would wrongly say it can't install.
    pub(crate) fn out_of_reach(&self, entry: &crate::model::AppEntry) -> Option<String> {
        matches!(self.settings.versions.get(entry.app.id), None | Some(VersionTrack::Stable))
            .then(|| entry.newer_out_of_reach(self.platform))
            .flatten()
    }

    /// The newer stable release a version kept on purpose is behind.
    fn kept_behind(&self, entry: &crate::model::AppEntry) -> Option<String> {
        let track = self.settings.versions.get(entry.app.id)?;
        let releases = self.version_lists.get(entry.app.id)?.as_ref().ok()?;
        behind(track, entry.installed.as_ref()?.version.as_ref()?, releases)
    }

    /// Asks once for the published versions of each app kept on an exact version, so its card can
    /// say when a newer release is out.
    pub(crate) fn request_pinned_lists(&mut self) {
        let pinned: Vec<String> =
            self.settings.versions.iter().filter(|(_, track)| matches!(track, VersionTrack::Pinned(_))).map(|(id, _)| id.clone()).collect();
        for id in pinned {
            self.request_versions(&id);
        }
    }
}

/// "Oct 8, 2026 · 150 MB": what a version row says beside its tag, from the release's date and its
/// download's published size. A size of zero means none was published, not an empty file.
fn version_facts(date: Option<String>, size: Option<u64>) -> String {
    let size = size.filter(|bytes| *bytes > 0).map(widgets::human_bytes);
    [date, size].into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

/// The newest stable release in `releases`, when `track` keeps an exact version and that release is
/// newer than `installed`.
fn behind(track: &VersionTrack, installed: &version::Version, releases: &[Release]) -> Option<String> {
    if !matches!(track, VersionTrack::Pinned(_)) {
        return None;
    }
    let newest = choose_version(releases, &VersionTrack::Stable)?;
    newest.version.as_ref().is_some_and(|v| version::is_newer(v, installed)).then(|| newest.version_label())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tagged(v: &str, prerelease: bool) -> Release {
        Release { tag: format!("v{v}"), version: version::parse(v), prerelease, ..Default::default() }
    }

    #[test]
    fn a_version_row_names_only_what_the_release_publishes() {
        let date = || Some("Oct 8, 2026".to_string());
        assert_eq!(version_facts(date(), Some(150 << 20)), "Oct 8, 2026 · 150 MB");
        assert_eq!(version_facts(date(), None), "Oct 8, 2026");
        assert_eq!(version_facts(date(), Some(0)), "Oct 8, 2026", "an unpublished size is not 1 KB");
        assert_eq!(version_facts(None, Some(3 << 30)), "3.0 GB");
        assert_eq!(version_facts(None, None), "");
    }

    #[test]
    fn only_a_kept_version_behind_the_newest_stable_release_is_noted() {
        let releases = [tagged("0.18.0-nightly.20261029", true), tagged("0.17.0", false), tagged("0.16.0", false)];
        let pinned = VersionTrack::Pinned("v0.16.0".into());
        let installed = |v: &str| version::parse(v).unwrap();
        // Kept on 0.16.0 with 0.17.0 out: noted, and a nightly is never "the latest".
        assert_eq!(behind(&pinned, &installed("0.16.0"), &releases), Some("0.17.0".to_string()));
        // Up to date, or ahead: nothing to say.
        assert_eq!(behind(&pinned, &installed("0.17.0"), &releases), None);
        assert_eq!(behind(&pinned, &installed("0.18.0"), &releases), None);
        // Following the latest release or nightly is not keeping a version.
        assert_eq!(behind(&VersionTrack::Stable, &installed("0.16.0"), &releases), None);
        assert_eq!(behind(&VersionTrack::Nightly, &installed("0.16.0"), &releases), None);
    }
}
