//! Store navigation and discovery, shared by the desktop and web shells.
use egui::{Align, FontId, Layout, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::app::{Dialog, LauncherUi, TitleBar};
use crate::settings::{Filter, ViewMode};
use crate::theme::{self, Tokens};
use crate::widgets::{self, Kind};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Page {
    #[default]
    Home,
    Applications,
    Games,
    Library,
}

impl Page {
    fn label(self) -> &'static str {
        match self {
            Self::Home => tl!("Home"),
            Self::Applications => tl!("Applications"),
            Self::Games => tl!("Games"),
            Self::Library => tl!("Library"),
        }
    }
}

impl LauncherUi {
    pub(crate) fn store_header(&mut self, ui: &mut Ui, bar: TitleBar) {
        let t = Tokens::get(ui.ctx());
        if bar != TitleBar::Native {
            widgets::title_bar_drag(ui);
        }
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.add_space(if bar == TitleBar::MacOs { 78.0 } else { 16.0 });
            let (mark, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
            ui.painter().rect_filled(mark, 7, t.accent);
            ui.painter().text(mark.center(), egui::Align2::CENTER_CENTER, "Z", FontId::new(19.0, theme::semibold()), t.sunken);
            if ui.available_width() > 420.0 {
                ui.label(RichText::new("Zendaya Launcher").size(14.0).family(theme::semibold()).color(t.ink_strong));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if bar == TitleBar::Custom {
                    widgets::window_buttons(ui, crate::views::HEADER_H);
                }
                if widgets::nav_icon(ui, "⚙", tl!("Settings"), 40.0, false).clicked() {
                    self.dialog = Some(Dialog::Settings);
                }
                let width = (ui.available_width() - 24.0).clamp(40.0, 600.0);
                let available = ui.available_rect_before_wrap();
                let center = ui.ctx().content_rect().center().x.max(available.left() + width * 0.5);
                ui.add_space((available.right() - center - width * 0.5).max(0.0));
                let response = ui.add_sized(
                    vec2(width, 36.0),
                    egui::TextEdit::singleline(&mut self.search).hint_text(tl!("Search apps")).vertical_align(Align::Center).margin(vec2(14.0, 8.0)),
                );
                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, tl!("Search apps")));
                if response.changed() {
                    self.detail = None;
                    self.detail_shown = None;
                    self.page = Page::Applications;
                    self.settings.filter = Filter::All;
                }
            });
        });
    }

    fn navigate(&mut self, page: Page) {
        self.page = page;
        self.detail = None;
        self.detail_shown = None;
        self.detail_zoom = false;
        self.search.clear();
        self.settings.filter = Filter::All;
    }

    pub(crate) fn store_navigation(&mut self, ui: &mut Ui) {
        for page in [Page::Home, Page::Applications, Page::Games] {
            if navigation_button(ui, page, self.page == page) {
                self.navigate(page);
            }
            ui.add_space(6.0);
        }
        ui.add_space((ui.available_height() - 78.0).max(0.0));
        if navigation_button(ui, Page::Library, self.page == Page::Library) {
            self.navigate(Page::Library);
        }
    }

    pub(crate) fn store_page(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        egui::ScrollArea::vertical().id_salt(("store-page", self.page as u8)).auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if self.page == Page::Home {
                self.store_home(ui);
                ui.add_space(28.0);
            } else if self.page == Page::Games {
                ui.add_space(70.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(tl!("Games")).size(38.0).family(theme::semibold()).color(t.ink_strong));
                    ui.add_space(16.0);
                    widgets::tag(ui, tl!("Coming soon"), t.accent, t.sunken);
                    ui.add_space(20.0);
                    ui.label(RichText::new(tl!("Games will arrive later. Discover our applications in the meantime.")).size(16.0).color(t.muted));
                    ui.add_space(24.0);
                    if widgets::button(ui, tl!("Explore applications"), Kind::Primary, 220.0).clicked() {
                        self.navigate(Page::Applications);
                    }
                });
                return;
            }
            let title = if self.page == Page::Home { tl!("Discover applications") } else { self.page.label() };
            ui.label(RichText::new(title).size(26.0).family(theme::semibold()).color(t.ink_strong));
            ui.add_space(18.0);
            self.toolbar(ui);
            ui.add_space(18.0);
            let ids: Vec<_> = self
                .visible_ids()
                .into_iter()
                .filter(|id| self.page != Page::Library || self.entry(id).is_some_and(|e| e.installed.is_some()))
                .collect();
            if ids.is_empty() {
                ui.add_space(36.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(if self.page == Page::Library { tl!("Your library is empty") } else { tl!("No apps match") })
                            .size(20.0)
                            .color(t.ink),
                    );
                    if self.page == Page::Library {
                        ui.add_space(12.0);
                        if widgets::button(ui, tl!("Explore applications"), Kind::Secondary, 220.0).clicked() {
                            self.navigate(Page::Applications);
                        }
                    }
                });
            } else {
                match self.settings.view {
                    ViewMode::Grid => self.grid(ui, &ids),
                    ViewMode::Table => self.table(ui, &ids),
                }
            }
            ui.add_space(24.0);
        });
    }

    fn store_home(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let total = ui.available_width();
        let wide = total > 900.0;
        let hero_w = if wide { (total - 18.0) * 0.70 } else { total };
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            ui.allocate_ui_with_layout(vec2(hero_w, 390.0), Layout::top_down(Align::Min), |ui| {
                egui::Frame::new().fill(t.surface).corner_radius(10).inner_margin(28).show(ui, |ui| {
                    ui.set_width((hero_w - 56.0).max(1.0));
                    ui.set_min_height(334.0);
                    ui.label(RichText::new(tl!("Featured application")).size(12.0).color(t.accent_ink));
                    ui.add_space(18.0);
                    ui.horizontal_top(|ui| {
                        let content_w = ui.available_width();
                        let split = content_w > 500.0;
                        ui.vertical(|ui| {
                            ui.set_width(if split { content_w * 0.43 } else { content_w });
                            ui.label(RichText::new("Photozen").size(38.0).family(theme::semibold()).color(t.ink_strong));
                            ui.add_space(12.0);
                            ui.label(RichText::new(tl!("Make room for your creativity.")).size(24.0).family(theme::semibold()).color(t.ink));
                            ui.add_space(12.0);
                            ui.label(RichText::new(tl!("Edit photos, compose with layers and bring your ideas to life.")).size(14.0).color(t.muted));
                            ui.add_space(22.0);
                            if widgets::button(ui, tl!("Discover Photozen"), Kind::Primary, 180.0).clicked() {
                                self.detail = Some("photozen".into());
                            }
                        });
                        if split {
                            ui.add_space(14.0);
                            let size = vec2((content_w * 0.57 - 24.0).max(1.0), 260.0);
                            if ui
                                .add(
                                    egui::Image::new(egui::include_image!("../../../assets/store/photozen-demo.jpg"))
                                        .fit_to_exact_size(size)
                                        .corner_radius(8)
                                        .sense(Sense::click())
                                        .alt_text("Photozen"),
                                )
                                .clicked()
                            {
                                self.detail = Some("photozen".into());
                            }
                        }
                    });
                });
            });
            if wide {
                ui.vertical(|ui| {
                    ui.set_width((total - hero_w - 18.0).max(1.0));
                    for (title, caption, action) in [
                        (tl!("Your creative space"), tl!("All your installed applications, together."), Page::Library),
                        (tl!("Games"), tl!("Coming soon"), Page::Games),
                    ] {
                        let response = egui::Frame::new().fill(t.surface).corner_radius(10).inner_margin(24).show(ui, |ui| {
                            ui.set_width(ui.available_width().max(1.0));
                            ui.set_min_height(140.0);
                            ui.label(RichText::new(title).size(22.0).family(theme::semibold()).color(t.ink_strong));
                            ui.add_space(14.0);
                            ui.label(RichText::new(caption).size(14.0).color(t.muted));
                            ui.add_space(14.0);
                            ui.label(RichText::new("→").size(24.0).color(t.accent_ink));
                        });
                        if ui
                            .interact(response.response.rect, ui.id().with(action as u8), Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            self.navigate(action);
                        }
                        ui.add_space(10.0);
                    }
                });
            }
        });
    }
}

fn navigation_button(ui: &mut Ui, page: Page, selected: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 68.0), Sense::click());
    let paint = ui.painter();
    if selected || response.hovered() {
        paint.rect_filled(rect, 6, t.surface);
    }
    if selected {
        paint.rect_filled(egui::Rect::from_min_size(rect.left_top() + vec2(0.0, 20.0), vec2(3.0, 26.0)), 2, t.accent);
    }
    let c = pos2(rect.center().x, rect.top() + 23.0);
    let color = if selected { t.accent_ink } else { t.muted };
    let stroke = Stroke::new(1.7, color);
    match page {
        Page::Home => {
            paint.line(vec![c + vec2(-10.0, -1.0), c + vec2(0.0, -10.0), c + vec2(10.0, -1.0)], stroke);
            paint.line(
                vec![
                    c + vec2(-8.0, -2.0),
                    c + vec2(-8.0, 9.0),
                    c + vec2(-2.0, 9.0),
                    c + vec2(-2.0, 2.0),
                    c + vec2(3.0, 2.0),
                    c + vec2(3.0, 9.0),
                    c + vec2(8.0, 9.0),
                    c + vec2(8.0, -2.0),
                ],
                stroke,
            );
        }
        Page::Applications => {
            for x in [-9.0, 2.0] {
                for y in [-9.0, 2.0] {
                    paint.rect_stroke(egui::Rect::from_min_size(c + vec2(x, y), vec2(8.0, 8.0)), 1, stroke, egui::StrokeKind::Inside);
                }
            }
        }
        Page::Games => {
            paint.rect_stroke(egui::Rect::from_center_size(c, vec2(24.0, 16.0)), 5, stroke, egui::StrokeKind::Inside);
            paint.line_segment([c + vec2(-8.0, 0.0), c + vec2(-2.0, 0.0)], stroke);
            paint.line_segment([c + vec2(-5.0, -3.0), c + vec2(-5.0, 3.0)], stroke);
            paint.circle_filled(c + vec2(6.0, -2.0), 1.5, color);
            paint.circle_filled(c + vec2(8.0, 2.0), 1.5, color);
        }
        Page::Library => {
            for x in [-9.0, -2.0, 6.0] {
                paint.rect_stroke(egui::Rect::from_min_size(c + vec2(x, -10.0), vec2(4.0, 20.0)), 1, stroke, egui::StrokeKind::Inside);
            }
        }
    }
    paint.text(pos2(rect.center().x, rect.bottom() - 16.0), egui::Align2::CENTER_CENTER, page.label(), FontId::proportional(10.5), color);
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, page.label()));
    response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}
