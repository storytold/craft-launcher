//! The native pane: everything the web build's note stands in for.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use egui::{Context, Ui};

use super::{engine, hub, models, tasks};
use crate::i18n;
use crate::theme::Tokens;

/// Which tab the pane shows.
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Assistant,
    Models,
    About,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Self::Assistant => "Assistant",
            Self::Models => "Models",
            Self::About => "About",
        }
    }
}

/// What the one background job is doing right now (a second click waits for it). Pulls and
/// saves carry their model's id, so a progress line shows under its own model's card only.
enum Work {
    None,
    /// Looking for runtimes and reading the machine's figures.
    Detecting,
    /// Asking the model a question.
    Asking,
    /// The runtime is pulling a model; the last progress line, and its bytes when it streams.
    Pulling {
        id: &'static str,
        note: String,
        done: u64,
        total: u64,
    },
    /// The shared folder is being filled; how many bytes of the file are done.
    Saving {
        id: &'static str,
        done: u64,
        total: u64,
    },
}

/// The pane's state, shared with its worker threads.
struct State {
    tab: Tab,
    /// First open: what runtimes, models and machine figures were found.
    detected: bool,
    runtimes: Vec<engine::Runtime>,
    runtime: usize,
    installed: Vec<String>,
    model: usize,
    device: Option<models::Device>,
    saved: Vec<String>,
    task: usize,
    input: String,
    /// The conversation so far, oldest first.
    history: Vec<engine::ChatMessage>,
    error: Option<String>,
    work: Work,
    started: Option<Instant>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            tab: Tab::Assistant,
            detected: false,
            runtimes: Vec::new(),
            runtime: 0,
            installed: Vec::new(),
            model: 0,
            device: None,
            saved: Vec::new(),
            task: 0,
            input: String::new(),
            history: Vec::new(),
            error: None,
            work: Work::None,
            started: None,
        }
    }
}

impl State {
    fn busy(&self) -> bool {
        !matches!(self.work, Work::None)
    }
}

/// The pane's state, created on first draw and kept in the egui data.
fn state(ctx: &Context) -> Arc<Mutex<State>> {
    let id = egui::Id::new("craft-ai-state");
    if let Some(s) = ctx.data(|d| d.get_temp::<Arc<Mutex<State>>>(id)) {
        return s;
    }
    let s = Arc::new(Mutex::new(State::default()));
    ctx.data_mut(|d| d.insert_temp(id, s.clone()));
    s
}

/// The state under lock: a poisoned state (a worker thread that panicked mid-write) still
/// reads — a task pane is not worth taking the app down with it.
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The pane's native body.
pub(super) fn show(_app: &mut crate::LauncherUi, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let state = state(&ctx);
    {
        let s = &mut *lock(&state);
        if !s.detected {
            s.detected = true;
            s.work = Work::Detecting;
            spawn_detect(&ctx, &state);
        }
        // A busy timer should tick even when the pointer is still.
        if s.busy() {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
    }
    let t = Tokens::get(ui.ctx());
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        for tab in [Tab::Assistant, Tab::Models, Tab::About] {
            let selected = lock(&state).tab == tab;
            if ui.add(egui::Button::selectable(selected, i18n::t(tab.label()))).clicked() {
                lock(&state).tab = tab;
            }
        }
    });
    ui.add_space(4.0);
    match lock(&state).tab {
        Tab::Assistant => assistant(ui, &t, &ctx, &state),
        Tab::Models => model_cards(ui, &t, &ctx, &state),
        Tab::About => about(ui, &t),
    }
}

/// The Assistant tab: runtime and model pickers, the task, the question, the answer.
fn assistant(ui: &mut Ui, t: &Tokens, ctx: &Context, state: &Arc<Mutex<State>>) {
    let mut detect = false;
    let mut ask: Option<(engine::Runtime, String, String)> = None;
    {
        let s = &mut *lock(state);
        if s.runtimes.is_empty() {
            if matches!(s.work, Work::Detecting) {
                ui.horizontal(|ui| {
                    ui.spinner();
                    dim(ui, t, i18n::t("Looking for a local model runtime…"));
                });
            } else {
                ui.label(egui::RichText::new(i18n::t("No local model runtime found.")).size(13.0).color(t.ink));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(i18n::t(
                        "Craft AI talks to a model runtime on this machine — Ollama, LM Studio or llama.cpp's server — and never to a cloud service. Start one, then Refresh.",
                    ))
                    .size(12.0)
                    .weak(),
                );
                if ui.button(i18n::t("Refresh")).clicked() {
                    detect = true;
                }
            }
        } else {
            let rt = s.runtimes[s.runtime.min(s.runtimes.len() - 1)];
            let names: Vec<String> = s.runtimes.iter().map(|r| r.to_string()).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let selected = names[s.runtime.min(names.len() - 1)].clone();
            label_row(ui, t, &i18n::t("Runtime"), |ui| {
                if let Some(i) = dropdown(ui, "craft-ai-runtime", &selected, &refs) {
                    s.runtime = i.min(s.runtimes.len() - 1);
                    detect = true;
                }
            });
            if s.installed.is_empty() {
                if !matches!(s.work, Work::Detecting) {
                    dim(ui, t, i18n::t("No model installed yet — see the Models tab."));
                }
            } else {
                // A Gemma model, if any, is the obvious default; an index that outlived its
                // list lands back on one.
                if s.model >= s.installed.len() {
                    s.model = s.installed.iter().position(|m| m.contains("gemma")).unwrap_or(0);
                }
                let model_selected = s.installed[s.model.min(s.installed.len() - 1)].clone();
                let model_refs: Vec<&str> = s.installed.iter().map(String::as_str).collect();
                label_row(ui, t, &i18n::t("Model"), |ui| {
                    if let Some(i) = dropdown(ui, "craft-ai-model", &model_selected, &model_refs) {
                        s.model = i.min(s.installed.len() - 1);
                    }
                });
                let labels: Vec<&str> = tasks::TASKS.iter().map(|task| task.label).collect();
                let task_selected = i18n::t(labels[s.task.min(labels.len() - 1)]);
                label_row(ui, t, &i18n::t("Task"), |ui| {
                    if let Some(i) = dropdown(ui, "craft-ai-task", &task_selected, &labels) {
                        s.task = i;
                    }
                });
                ui.add_space(4.0);
                let hint = i18n::t(tasks::TASKS[s.task.min(tasks::TASKS.len() - 1)].hint);
                ui.add(
                    egui::TextEdit::multiline(&mut s.input).hint_text(hint).desired_rows(4).desired_width(f32::INFINITY).size(12.0),
                );
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let busy = s.busy();
                    let ready = !busy && !s.input.trim().is_empty();
                    if ui.add_enabled(ready, egui::Button::new(i18n::t("Ask"))).clicked() {
                        let model = s.installed[s.model.min(s.installed.len() - 1)].clone();
                        ask = Some((rt, model, tasks::user_prompt(s.task, &s.input)));
                    }
                    if !s.history.is_empty() && !busy && ui.small_button(i18n::t("Clear")).clicked() {
                        s.history.clear();
                    }
                    if busy {
                        ui.spinner();
                        let secs = s.started.map(|at| at.elapsed().as_secs()).unwrap_or(0);
                        dim(ui, t, i18n::t("Thinking… {s}s").replace("{s}", &secs.to_string()));
                    }
                });
            }
            if let Some(e) = &s.error {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(e).size(12.0).color(t.ink).weak());
            }
            if !s.history.is_empty() {
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                    let rtl = i18n::current().script() == Some(i18n::Script::Arabic);
                    for m in &s.history {
                        if m.role == "user" {
                            ui.label(egui::RichText::new(format!("› {}", m.content)).size(12.0).color(t.muted));
                        } else {
                            // What the model says may be Arabic the user wrote in: it keeps its
                            // logical order (clipboard order) on arrival and is drawn visual,
                            // like every Arabic the interface shows.
                            let text = if rtl { i18n::bidi::visual_rtl(&m.content) } else { m.content.clone() };
                            ui.add(egui::Label::new(egui::RichText::new(text).size(12.0)).wrap());
                            ui.add_space(2.0);
                            if ui.small_button(i18n::t("Copy")).clicked() {
                                ctx.copy_text(m.content.clone());
                            }
                        }
                    }
                });
            }
        }
    }
    if let Some((rt, model, prompt)) = ask {
        let s = &mut *lock(state);
        s.work = Work::Asking;
        s.started = Some(Instant::now());
        s.error = None;
        let question = s.input.trim().to_string();
        let history = s.history.clone();
        spawn_ask(ctx, state, rt, &model, &history, &question, &prompt);
    }
    if detect {
        let s = &mut *lock(state);
        s.work = Work::Detecting;
        spawn_detect(ctx, state);
    }
}

/// The Models tab: the catalogue with each model's figures, and its download buttons.
fn model_cards(ui: &mut Ui, t: &Tokens, ctx: &Context, state: &Arc<Mutex<State>>) {
    let mut detect = false;
    let mut pull: Option<&'static models::ModelSpec> = None;
    let mut save: Option<&'static models::ModelSpec> = None;
    let mut remove: Option<&'static models::ModelSpec> = None;
    {
        let s = &mut *lock(state);
        if let Some(d) = &s.device {
            let ram = d.ram_gb.map(|g| g.to_string()).unwrap_or_else(|| "?".into());
            let disk = d.free_gb.map(|g| g.to_string()).unwrap_or_else(|| "?".into());
            dim(ui, t, i18n::t("This machine: {ram} GB RAM, {disk} GB free").replace("{ram}", &ram).replace("{disk}", &disk));
        } else if matches!(s.work, Work::Detecting) {
            ui.horizontal(|ui| {
                ui.spinner();
                dim(ui, t, i18n::t("Looking for a local model runtime…"));
            });
        }
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(i18n::t("One download serves every ArtCraft app: the model lands in a shared folder all of them read."))
                .size(12.0)
                .weak(),
        );
        ui.add_space(4.0);
        let ollama = s.runtimes.iter().copied().find(|r| r.kind == engine::RuntimeKind::Ollama);
        for m in models::MODELS {
            ui.add(egui::Label::new(egui::RichText::new(m.name).size(13.0).strong().color(t.accent_ink)).wrap());
            let (ram_ok, disk_ok) = match &s.device {
                Some(d) => (d.ram_gb.is_none_or(|g| g >= m.ram_gb), d.free_gb.is_none_or(|g| g >= m.bytes / 1_000_000_000)),
                None => (true, true),
            };
            ui.label(egui::RichText::new(format!("{} · {} · {}", m.params, m.context, m.inputs)).size(11.5).color(t.muted));
            ui.label(egui::RichText::new(i18n::t(m.best)).size(11.5).color(t.muted));
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(i18n::t("Disk {size}").replace("{size}", &m.gb())).size(11.5).color(t.muted));
                ui.separator();
                ui.label(
                    egui::RichText::new(i18n::t("RAM {gb} GB").replace("{gb}", &m.ram_gb.to_string())).size(11.5).color(t.muted),
                );
                if s.device.is_some() {
                    ui.separator();
                    if ram_ok && disk_ok {
                        ui.label(egui::RichText::new(i18n::t("Fits this machine")).size(11.5).color(t.accent_ink));
                    } else if !ram_ok {
                        ui.label(
                            egui::RichText::new(i18n::t("Needs {ram} GB of RAM").replace("{ram}", &m.ram_gb.to_string()))
                                .size(11.5)
                                .color(t.ink),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new(i18n::t("Needs {disk} GB of disk").replace("{disk}", &models::gb(m.bytes)))
                                .size(11.5)
                                .color(t.ink),
                        );
                    }
                }
            });
            let saved = s.saved.iter().any(|id| id == m.id);
            ui.horizontal(|ui| {
                let busy = s.busy();
                if ollama.is_some() && !busy && ui.small_button(i18n::t("Download with Ollama")).clicked() {
                    pull = Some(m);
                }
                if !busy && !saved && ui.small_button(i18n::t("Save model file")).clicked() {
                    save = Some(m);
                }
                if !busy && saved && ui.small_button(i18n::t("Delete file")).clicked() {
                    remove = Some(m);
                }
            });
            match &s.work {
                Work::Pulling { id, note, done, total } if *id == m.id => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let pct = ((*done as f64 / (*total as f64).max(1.0)) * 100.0).round() as u64;
                        dim(ui, t, i18n::t("Downloading… {pct}% — {note}").replace("{pct}", &pct.to_string()).replace("{note}", note));
                    });
                }
                Work::Saving { id, done, total } if *id == m.id => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let pct = ((*done as f64 / (*total as f64).max(1.0)) * 100.0).round() as u64;
                        dim(ui, t, i18n::t("Saving… {pct}%").replace("{pct}", &pct.to_string()));
                    });
                }
                _ if saved => {
                    dim(ui, t, i18n::t("Saved in the shared folder"));
                }
                _ => {}
            }
            if let Some(e) = &s.error {
                ui.label(egui::RichText::new(e).size(11.5).color(t.ink).weak());
            }
            ui.add_space(6.0);
        }
        if !s.runtimes.is_empty() && !s.busy() && ui.small_button(i18n::t("Refresh")).clicked() {
            detect = true;
        }
    }
    // The pull button only shows when an Ollama was found, so the runtime is there to find.
    if let Some(m) = pull
        && let Some(rt) = lock(state).runtimes.iter().copied().find(|r| r.kind == engine::RuntimeKind::Ollama)
    {
        let s = &mut *lock(state);
        s.work = Work::Pulling { id: m.id, note: String::new(), done: 0, total: 0 };
        s.error = None;
        spawn_pull(ctx, state, rt, m);
    }
    if let Some(m) = save {
        let s = &mut *lock(state);
        s.work = Work::Saving { id: m.id, done: 0, total: m.bytes };
        s.error = None;
        spawn_save(ctx, state, m);
    }
    if let Some(m) = remove {
        lock(state).error = hub::remove(m).err();
        detect = true;
    }
    if detect {
        let s = &mut *lock(state);
        s.work = Work::Detecting;
        spawn_detect(ctx, state);
    }
}

/// The About tab: what runs where, and its honest limits.
fn about(ui: &mut Ui, t: &Tokens) {
    ui.label(egui::RichText::new(i18n::t("Local and private")).size(13.0).strong().color(t.accent_ink));
    ui.add_space(2.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new(i18n::t(
                "Craft AI runs a local Gemma model on this machine. Nothing you write here leaves it: no account, no cloud service, no telemetry — the panel talks to a model runtime on 127.0.0.1.",
            ))
            .size(12.0),
        )
        .wrap(),
    );
    ui.add_space(6.0);
    ui.label(egui::RichText::new(i18n::t("Limits")).size(13.0).strong().color(t.accent_ink));
    ui.add_space(2.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new(i18n::t(
                "A local model is not a frontier cloud model: it can be wrong, and a long answer takes its time on CPU. Check anything critical, and mind a model's size against this machine before downloading it.",
            ))
            .size(12.0),
        )
        .wrap(),
    );
    ui.add_space(6.0);
    if let Some(dir) = hub::models_dir() {
        ui.label(egui::RichText::new(i18n::t("The shared folder:")).size(12.0).color(t.muted));
        ui.add(egui::Label::new(egui::RichText::new(dir.display().to_string()).size(11.5).color(t.muted).monospace()).wrap());
    }
}

/// Find the runtimes, their models, the machine's figures and the shared folder — once, off the
/// UI thread.
fn spawn_detect(ctx: &Context, state: &Arc<Mutex<State>>) {
    let ctx = ctx.clone();
    let state = state.clone();
    let state_runtime = lock(&state).runtime;
    spawn("craft-ai-detect", move || {
        let runtimes = engine::detect();
        let installed = runtimes.get(state_runtime).copied().and_then(|rt| engine::installed(rt).ok()).unwrap_or_default();
        let device = models::device(hub::models_dir().as_deref());
        let saved = hub::saved_ids();
        let mut s = lock(&state);
        if s.runtime >= runtimes.len() {
            s.runtime = 0;
        }
        s.runtimes = runtimes;
        s.installed = installed;
        s.device = Some(device);
        s.saved = saved;
        s.work = Work::None;
        ctx.request_repaint();
    });
}

/// Ask the model the question, then bring the answer back.
fn spawn_ask(
    ctx: &Context,
    state: &Arc<Mutex<State>>,
    rt: engine::Runtime,
    model: &str,
    history: &[engine::ChatMessage],
    question: &str,
    prompt: &str,
) {
    let ctx = ctx.clone();
    let state = state.clone();
    let model = model.to_string();
    let history = history.to_vec();
    let question = question.to_string();
    let prompt = prompt.to_string();
    spawn("craft-ai-ask", move || {
        let reply = engine::chat(rt, &model, &tasks::system_prompt(), &history, &prompt);
        let mut s = lock(&state);
        match reply {
            Ok(answer) => {
                s.history.push(engine::ChatMessage { role: "user", content: question });
                s.history.push(engine::ChatMessage { role: "assistant", content: answer });
                // Keep the oldest turns out of the context when the conversation grows.
                if s.history.len() > 12 {
                    let cut = s.history.len() - 12;
                    s.history.drain(..cut);
                }
                s.input.clear();
                s.error = None;
            }
            Err(e) => s.error = Some(e),
        }
        s.work = Work::None;
        s.started = None;
        ctx.request_repaint();
    });
}

/// Have Ollama pull the model (its own download manager, into its own store — shared by every
/// app on this machine in its own way).
fn spawn_pull(ctx: &Context, state: &Arc<Mutex<State>>, rt: engine::Runtime, m: &'static models::ModelSpec) {
    let ctx = ctx.clone();
    let state = state.clone();
    let tag = m.ollama.to_string();
    spawn("craft-ai-pull", move || {
        let reply = engine::pull(rt, &tag, &mut |note, progress| {
            let mut s = lock(&state);
            if let Work::Pulling { note: n, done, total, .. } = &mut s.work {
                *n = note.to_string();
                if let Some((d, t)) = progress {
                    *done = d;
                    *total = t;
                }
            }
            ctx.request_repaint();
        });
        let mut s = lock(&state);
        match reply {
            Ok(()) => s.error = None,
            Err(e) => s.error = Some(e),
        }
        s.work = Work::None;
        ctx.request_repaint();
    });
}

/// Download the model file into the shared folder every ArtCraft app reads.
fn spawn_save(ctx: &Context, state: &Arc<Mutex<State>>, m: &'static models::ModelSpec) {
    let ctx = ctx.clone();
    let state = state.clone();
    let spec = m;
    spawn("craft-ai-save", move || {
        let reply = hub::save(spec, tasks::APP, &mut |done, total| {
            let mut s = lock(&state);
            if let Work::Saving { done: d, total: t, .. } = &mut s.work {
                *d = done;
                *t = total;
            }
            ctx.request_repaint();
        });
        let mut s = lock(&state);
        match reply {
            Ok(()) => {
                s.saved = hub::saved_ids();
                s.error = None;
            }
            Err(e) => s.error = Some(e),
        }
        s.work = Work::None;
        ctx.request_repaint();
    });
}

/// Run `body` on a named worker thread; if threads are unavailable the pane's error line says
/// so on the next draw.
fn spawn(name: &str, body: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.to_string()).spawn(body);
}

/// A quiet aside line, the style the app's other panes use for theirs.
fn dim(ui: &mut Ui, t: &Tokens, text: impl Into<String>) {
    ui.label(egui::RichText::new(text).size(12.0).color(t.muted));
}

/// A labelled row: the label at a fixed width, so the pickers under it align.
fn label_row(ui: &mut Ui, t: &Tokens, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 20.0], egui::Label::new(egui::RichText::new(label).size(12.0).color(t.muted)));
        add(ui);
    });
}

/// A picker of the options given, returning the index chosen this frame.
fn dropdown(ui: &mut Ui, id: &str, selected: &str, options: &[&str]) -> Option<usize> {
    let mut picked = None;
    egui::ComboBox::from_id(id).width(ui.available_width()).selected_text(selected).show_ui(ui, |ui| {
        for (i, opt) in options.iter().enumerate() {
            if ui.selectable_label(*opt == selected, i18n::t(opt)).clicked() {
                picked = Some(i);
            }
        }
    });
    picked
}
