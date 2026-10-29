//! Golden fixtures for the theme install: the style in each egui slot per mode, and the tokens
//! `Tokens::get` returns per mode and OS answer. `UPDATE_GOLDEN=1` writes
//! them; otherwise a missing fixture fails and the output must match byte for byte.

use std::path::PathBuf;

use artcraft_launcher_ui_egui::theme::{self, ThemeChoice, Tokens};
use egui::Theme;

fn fixture(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/theme").join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().expect("fixture dir")).expect("create fixture dir");
        std::fs::write(&path, actual).expect("write fixture");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("fixture missing: {}; run with UPDATE_GOLDEN=1", path.display()));
    // A Windows checkout may turn the fixtures' line endings into CRLF.
    let expected = expected.replace("\r\n", "\n");
    assert!(expected == actual, "{name} differs from {} (rerun with UPDATE_GOLDEN=1 to review the change)", path.display());
}

fn tokens_under(choice: ThemeChoice, os: Option<Theme>) -> Tokens {
    let ctx = egui::Context::default();
    theme::apply(&ctx, choice);
    let mut tokens = None;
    ctx.run_ui(egui::RawInput { system_theme: os, ..Default::default() }, |ui| tokens = Some(Tokens::get(ui.ctx()))).textures_delta.clear();
    tokens.expect("the pass ran")
}

#[test]
fn theme_slots_and_tokens_match_the_golden_fixtures() {
    for (mode, choice) in [("system", ThemeChoice::System), ("dark", ThemeChoice::Dark), ("light", ThemeChoice::Light)] {
        let ctx = egui::Context::default();
        theme::apply(&ctx, choice);
        for (slot, name) in [(Theme::Light, "light"), (Theme::Dark, "dark")] {
            fixture(&format!("style-{mode}-{name}-slot"), &format!("{:#?}\n", ctx.style_of(slot)));
        }
        for (os, name) in [(Some(Theme::Light), "os-light"), (Some(Theme::Dark), "os-dark"), (None, "os-none")] {
            fixture(&format!("tokens-{mode}-{name}"), &format!("{:#?}\n", tokens_under(choice, os)));
        }
    }
}

#[test]
fn fixed_dark_under_a_light_os_answer_draws_dark() {
    let ctx = egui::Context::default();
    theme::apply(&ctx, ThemeChoice::Dark);
    let mut seen = None;
    ctx.run_ui(egui::RawInput { system_theme: Some(Theme::Light), ..Default::default() }, |ui| {
        seen = Some((ui.ctx().global_style().visuals.dark_mode, Tokens::get(ui.ctx())));
    })
    .textures_delta
    .clear();
    let (dark_mode, tokens) = seen.expect("the pass ran");
    assert!(dark_mode, "egui used light visuals");
    assert_eq!(tokens, Tokens::DARK);
}
