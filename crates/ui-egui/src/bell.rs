//! The header's bell: ArtCraft's announcements from the release feed, out of the way until the
//! user looks. A count on the bell says how many are unread; opening it marks them all read.
//! Nothing here opens by itself, makes a sound or raises a system notification.

use artcraft_launcher_core::notices::Notice;
use egui::{CornerRadius, Rect, RichText, Ui, vec2};

use crate::app::LauncherUi;
use crate::i18n::tn;
use crate::theme::{self, Tokens};
use crate::widgets::{self, Glyph};

/// The panel's width, and the most of the window's height its list takes before it scrolls.
const PANEL_W: f32 = 360.0;
const LIST_MAX_H: f32 = 420.0;

impl LauncherUi {
    /// Announcements the user has neither dismissed nor seen in the bell.
    pub(crate) fn unread_announcements(&self) -> usize {
        self.listed().filter(|n| !self.settings.read_notices.contains(&n.id)).count()
    }

    /// Announcements neither dismissed nor past their end date.
    fn listed(&self) -> impl Iterator<Item = &Notice> {
        let now = self.backend.now();
        self.announcements.iter().filter(move |n| n.current(now) && !self.settings.dismissed_notices.contains(&n.id))
    }

    /// The bell's header cell, its unread count, and the panel it opens.
    pub(crate) fn bell(&mut self, ui: &mut Ui, height: f32) {
        let unread = self.unread_announcements();
        let name =
            if unread == 0 { tl!("Announcements").to_owned() } else { tn(unread as u64, "Announcements, {n} unread", "Announcements, {n} unread") };
        let bell = widgets::nav_icon(ui, crate::icons::bell(), &name, height, false);
        // A polite live region: a screen reader says the new name when the count goes up while
        // the user is elsewhere, and nothing changes on screen but the badge.
        ui.ctx().accesskit_node_builder(bell.id, |node| node.set_live(egui::accesskit::Live::Polite));
        if unread > 0 {
            count_badge(ui, bell.rect, unread);
        }
        if bell.clicked() {
            self.bell_open = !self.bell_open;
            if self.bell_open {
                self.bell_new = self.listed().filter(|n| !self.settings.read_notices.contains(&n.id)).map(|n| n.id.clone()).collect();
            }
        }
        let was_open = self.bell_open;
        let just_opened = bell.clicked() && self.bell_open;
        if self.bell_open {
            // Everything the panel shows has been seen, including what arrives while it is open.
            let shown: Vec<String> = self.listed().map(|n| n.id.clone()).collect();
            self.settings.read_notices.extend(shown);
        }
        let mut list: Vec<Notice> = self.listed().cloned().collect();
        // Newest first; an undated announcement (`None`) sorts last.
        list.sort_by_key(|n| std::cmp::Reverse(n.created_at));
        let t = Tokens::get(ui.ctx());
        let mut open = self.bell_open;
        let new = &self.bell_new;
        let dismissed = egui::Popup::from_response(&bell)
            .open_bool(&mut open)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .align(egui::RectAlign::BOTTOM_END)
            .align_alternatives(&[egui::RectAlign::BOTTOM_START])
            .gap(0.0)
            .width(PANEL_W)
            .accessible_name(tl!("Announcements"))
            .frame(egui::Frame::new().fill(t.bg).stroke(t.hairline()).corner_radius(CornerRadius::ZERO).inner_margin(egui::Margin::same(14)))
            .show(|ui| panel(ui, &list, new, just_opened))
            .and_then(|r| r.inner);
        self.bell_open = open;
        if let Some(id) = dismissed {
            self.settings.dismissed_notices.insert(id);
        }
        // Escape hands focus back to the bell, so keyboard users carry on where they were.
        if was_open && !self.bell_open && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            bell.request_focus();
        }
    }
}

/// The panel's list. Returns the id of an announcement the user dismissed.
fn panel(ui: &mut Ui, list: &[Notice], new: &std::collections::BTreeSet<String>, focus_first: bool) -> Option<String> {
    let t = Tokens::get(ui.ctx());
    ui.set_width(PANEL_W - 28.0);
    widgets::hud(ui, tl!("Announcements"), t.muted);
    ui.add_space(10.0);
    if list.is_empty() {
        ui.label(RichText::new(tl!("No announcements")).color(t.muted));
        return None;
    }
    let mut dismissed = None;
    let mut focused = !focus_first;
    let mut focus = |r: &egui::Response| {
        if !focused {
            r.request_focus();
            focused = true;
        }
    };
    egui::ScrollArea::vertical().max_height(LIST_MAX_H).auto_shrink([false, true]).show(ui, |ui| {
        for (i, notice) in list.iter().enumerate() {
            if i > 0 {
                ui.add_space(10.0);
                let (rule, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
                ui.painter().hline(rule.x_range(), rule.center().y, t.hairline());
                ui.add_space(10.0);
            }
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    let close = widgets::glyph_button(ui, Glyph::Close, tl!("Dismiss"));
                    if close.clicked() {
                        dismissed = Some(notice.id.clone());
                    }
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.horizontal(|ui| {
                            if new.contains(&notice.id) {
                                // The dot says "new since you last looked"; the title says the rest.
                                let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
                                ui.painter().rect_filled(Rect::from_center_size(dot.center(), vec2(6.0, 6.0)), CornerRadius::ZERO, t.accent);
                            }
                            if let Some(secs) = notice.created_at {
                                widgets::hud(ui, &crate::views::date_text(secs), t.faint);
                            }
                        });
                        if !notice.title.is_empty() {
                            ui.add(egui::Label::new(RichText::new(&notice.title).family(theme::semibold()).color(t.ink_strong)).wrap());
                        }
                        if !notice.body.is_empty() {
                            ui.add(egui::Label::new(RichText::new(&notice.body).color(t.muted)).wrap());
                        }
                        if let Some(link) = &notice.link {
                            focus(&widgets::hud_link(ui, tl!("Learn more"), link));
                        }
                    });
                    focus(&close);
                });
            });
        }
    });
    dismissed
}

/// The unread count on the bell's top-right corner: accent square, mono digits, "9+" above nine.
fn count_badge(ui: &Ui, cell: Rect, unread: usize) {
    let t = Tokens::get(ui.ctx());
    let text = if unread > 9 { "9+".to_owned() } else { unread.to_string() };
    // Near-black digits in both themes: white on the accent is under 4.5:1.
    let ink = if t.dark { t.bg } else { t.ink_strong };
    let galley = widgets::caps(ui, &text, 9.0, theme::mono_bold(), 0.0, ink);
    let size = vec2((galley.size().x + 6.0).max(14.0), 14.0);
    let badge = Rect::from_min_size(cell.center() + vec2(2.0, -12.0), size);
    let p = ui.painter();
    p.rect_filled(badge, CornerRadius::ZERO, t.accent);
    p.galley_with_override_text_color(badge.center() - galley.size() / 2.0, galley, ink);
}
