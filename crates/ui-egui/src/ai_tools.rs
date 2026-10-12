//! The AI tools dialog: adds the Craft apps' MCP servers to the settings of AI tools (Claude,
//! Codex, Cursor, VS Code…), or takes them out again.

use artcraft_launcher_core::mcp::{AiTool, Entry, McpReport, McpServer};
use artcraft_launcher_core::protocol::Command;
use artcraft_launcher_core::{CraftApp, SUITE};
use egui::RichText;

use crate::app::{Dialog, LauncherUi};
use crate::i18n::{fmt, msg};
use crate::theme::{self, Tokens};
use crate::widgets::{self, Kind};

/// The height of an AI tool's row: its buttons, or its name over its settings file.
const ROW_H: f32 = 36.0;
/// The least room between a tool's name and path and the buttons right of them.
const ROW_GAP: f32 = 16.0;
/// Room right of the list for its scroll bar, taken from the dialog's margin, so the rows are as
/// wide as the rest of the dialog and their buttons end where Done does.
const SCROLL_GUTTER: f32 = 14.0;

impl LauncherUi {
    /// Opens the dialog for one app (`Some(id)`) or every installed app, and looks at the AI tools.
    pub(crate) fn open_ai_tools(&mut self, app: Option<String>) {
        self.dialog = Some(Dialog::AiTools(app));
        self.mcp_busy = true;
        self.send(Command::McpScan);
    }

    /// Asks the backend to add (`connect`) or remove `apps` in `tool`'s settings. The buttons wait
    /// until it has answered.
    fn connect(&mut self, tool: &AiTool, apps: &[&CraftApp], connect: bool) {
        self.mcp_busy = true;
        self.send(Command::McpConnect { tool: tool.id.clone(), apps: apps.iter().map(|a| a.id.to_string()).collect(), connect });
    }

    /// Returns whether to close.
    pub(crate) fn ai_tools_dialog(&mut self, ui: &mut egui::Ui, scope: Option<&str>) -> bool {
        let t = Tokens::get(ui.ctx());
        ui.set_width(600.0f32.min(ui.ctx().content_rect().width() - 80.0));
        let icon = scope.and_then(crate::icons::app).unwrap_or_else(crate::icons::artcraft_mark);
        Self::title(ui, Some(icon), tl!("AI tools"), "Model Context Protocol");
        let intro = tl!(
            "Let AI assistants such as Claude, Codex or Cursor work in the Craft apps. ArtCraft Launcher adds each app's MCP server to the settings the AI tool uses for all your projects. Restart the AI tool after a change."
        );
        ui.add(egui::Label::new(RichText::new(intro).color(t.muted)).wrap());
        ui.add_space(14.0);

        // Which apps: one, or every installed one.
        let installed: Vec<&'static CraftApp> = self.entries.iter().filter(|e| e.installed.is_some()).map(|e| e.app).collect();
        let mut choice = scope.map(str::to_string);
        ui.horizontal(|ui| {
            ui.label(tl!("Apps"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = choice.as_deref().and_then(CraftApp::by_id).map_or_else(|| tl!("All installed apps").to_string(), |a| a.name.to_string());
                egui::ComboBox::from_id_salt("ai_tools_apps").selected_text(label).width(220.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut choice, None, tl!("All installed apps"));
                    for app in &installed {
                        ui.selectable_value(&mut choice, Some(app.id.to_string()), app.name);
                    }
                });
            });
        });
        if choice.as_deref() != scope {
            self.dialog = Some(Dialog::AiTools(choice));
        }
        ui.add_space(10.0);

        // The report leaves `self` while the list is drawn: the list's buttons need `self` too.
        let report = self.mcp.take();
        let ready = match &report {
            Some(report) => self.tool_list(ui, scope, report),
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(tl!("Looking for AI tools…")).color(t.muted));
                });
                Vec::new()
            }
        };
        self.mcp = report;

        ui.add_space(16.0);
        let mut close = false;
        ui.horizontal(|ui| {
            let copy = ui
                .add_enabled_ui(!ready.is_empty(), |ui| widgets::button(ui, tl!("Copy configuration"), Kind::Secondary, 0.0))
                .inner
                .on_hover_text(tl!("Copies the MCP settings of these apps, for an AI tool that isn't listed"));
            if copy.clicked()
                && let Some(report) = &self.mcp
            {
                let servers: Vec<(&str, &McpServer)> = ready.iter().filter_map(|a| Some((a.id, report.servers.get(a.id)?.as_ref().ok()?))).collect();
                ui.ctx().copy_text(McpServer::snippet(&servers));
                let ctx = ui.ctx().clone();
                self.toast(&ctx, tl!("Copied to the clipboard"), false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = widgets::button(ui, tl!("Done"), Kind::Primary, 90.0).clicked();
            });
        });
        close
    }

    /// Why some apps can't be connected, then the AI tools, the installed ones first. Returns the
    /// apps in scope whose server can be started here.
    fn tool_list(&mut self, ui: &mut egui::Ui, scope: Option<&str>, report: &McpReport) -> Vec<&'static CraftApp> {
        let t = Tokens::get(ui.ctx());
        // The apps in scope: for "all", every app of the suite, so what an app that is gone left
        // behind can be removed too.
        let all: Vec<&'static CraftApp> = match scope {
            Some(id) => CraftApp::by_id(id).into_iter().collect(),
            None => SUITE.iter().collect(),
        };
        let ready: Vec<&'static CraftApp> = all.iter().copied().filter(|a| matches!(report.servers.get(a.id), Some(Ok(_)))).collect();
        let note = |ui: &mut egui::Ui, text: &str, color| {
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).wrap());
        };
        for app in &all {
            if let Some(Err(why)) = report.servers.get(app.id) {
                note(ui, &format!("{}: {}", app.name, msg(why)), t.warn);
            }
        }
        if all.iter().all(|a| !report.servers.contains_key(a.id)) {
            note(ui, tl!("Install an app first to use it with AI tools."), t.muted);
        }
        ui.add_space(6.0);

        // The list's full height at once (left to grow into the dialog it would open a row per
        // frame): half the window, less where the window is too short for the rest of the dialog.
        let window = ui.ctx().content_rect().height();
        let height = (window * 0.5).min(window - 420.0).max(120.0);
        let width = ui.available_width();
        let list = ui.available_rect_before_wrap();
        let mut list_ui = ui.new_child(egui::UiBuilder::new().max_rect(list.with_max_x(list.right() + SCROLL_GUTTER)));
        egui::ScrollArea::vertical().max_height(height).min_scrolled_height(height).auto_shrink([false, true]).show(&mut list_ui, |ui| {
            ui.set_max_width(width);
            let mut missing_shown = false;
            for tool in &report.tools {
                if !tool.found && !missing_shown {
                    missing_shown = true;
                    ui.add_space(12.0);
                    widgets::hud(ui, tl!("Not found on this computer"), t.faint);
                    ui.add_space(4.0);
                }
                self.tool_row(ui, tool, &all, &ready);
            }
        });
        // The list's height, without its gutter's width.
        ui.allocate_rect(egui::Rect::from_min_size(list.min, egui::vec2(width, list_ui.min_rect().height())), egui::Sense::hover());
        ready
    }

    /// One AI tool: its name and settings file, whether the apps in scope are connected, and the
    /// buttons that change that. `all` is every app in scope; `ready` those that can be added.
    fn tool_row(&mut self, ui: &mut egui::Ui, tool: &AiTool, all: &[&'static CraftApp], ready: &[&'static CraftApp]) {
        let t = Tokens::get(ui.ctx());
        let state = |app: &CraftApp| tool.entries.get(app.id).copied();
        let current = ready.iter().filter(|a| state(a) == Some(Entry::Current)).count();
        let to_add: Vec<&CraftApp> = ready.iter().copied().filter(|a| !matches!(state(a), Some(Entry::Current | Entry::Foreign))).collect();
        let updating = !to_add.is_empty() && to_add.iter().all(|a| state(a) == Some(Entry::Different));
        let to_remove: Vec<&CraftApp> = all.iter().copied().filter(|a| matches!(state(a), Some(Entry::Current | Entry::Different))).collect();
        let foreign: Vec<&str> = all.iter().filter(|a| state(a) == Some(Entry::Foreign)).map(|a| a.name).collect();

        ui.add_space(8.0);
        // One line across the list. The buttons and the status go in from the right; the name
        // and the settings file get what is left, so a long path is cut short of the buttons.
        let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::hover());
        let mut controls = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::right_to_left(egui::Align::Center)));
        let enabled = !self.mcp_busy && tool.error.is_none();
        controls.add_enabled_ui(enabled, |ui| {
            if !to_remove.is_empty() && widgets::button(ui, tl!("Remove"), Kind::Secondary, 0.0).clicked() {
                self.connect(tool, &to_remove, false);
            }
            let add = if updating { tl!("Update") } else { tl!("Add") };
            if !to_add.is_empty() && widgets::button(ui, add, Kind::Primary, 0.0).clicked() {
                self.connect(tool, &to_add, true);
            }
        });
        if tool.error.is_some() {
            widgets::badge(&mut controls, tl!("Can't read"), t.warn);
        } else if !ready.is_empty() && current == ready.len() {
            widgets::badge(&mut controls, tl!("Connected"), t.good);
        } else if current > 0 {
            let text = fmt(tl!("{n} of {m} connected"), &[("n", &current.to_string()), ("m", &ready.len().to_string())]);
            widgets::badge(&mut controls, &text, t.accent_ink);
        }
        let taken = controls.min_rect();
        let text_right = if taken.width() > 0.0 { taken.left() - ROW_GAP } else { row.right() };

        // The name over the path, the pair centred on the row.
        let name_font = egui::FontId::new(egui::TextStyle::Body.resolve(ui.style()).size, theme::medium());
        let path_font = egui::FontId::new(theme::HUD_SIZE, theme::mono());
        let line = |font: &egui::FontId| ui.painter().layout_no_wrap(" ".to_string(), font.clone(), t.ink).size().y;
        let gap = 2.0;
        let pair = line(&name_font) + gap + line(&path_font);
        let text =
            egui::Rect::from_min_max(egui::pos2(row.left(), row.center().y - pair / 2.0), egui::pos2(text_right.max(row.left()), row.bottom()));
        let mut names = ui.new_child(egui::UiBuilder::new().max_rect(text).layout(egui::Layout::top_down(egui::Align::Min)));
        names.spacing_mut().item_spacing.y = gap;
        let name = RichText::new(&tool.name).font(name_font).color(if tool.found { t.ink_strong } else { t.muted });
        names.add(egui::Label::new(name).truncate());
        widgets::path(&mut names, &tool.path, t.faint, text.width());

        if let Some(e) = &tool.error {
            ui.add(egui::Label::new(RichText::new(fmt(tl!("Its settings can't be read: {e}"), &[("e", e)])).size(12.0).color(t.warn)).wrap());
        }
        if !foreign.is_empty() {
            let text = fmt(tl!("It has its own settings for {apps}, which stay as they are."), &[("apps", &foreign.join(", "))]);
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(t.muted)).wrap());
        }
        ui.add_space(8.0);
        let (rule, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
        ui.painter().hline(rule.x_range(), rule.center().y, t.hairline());
    }
}
