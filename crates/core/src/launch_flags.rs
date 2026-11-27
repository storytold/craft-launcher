//! The launch flags each Craft App accepts, offered as switches beside the free-text launch
//! options. Only flags read from the argument parser of the app's latest *release* are listed, not
//! from its `main`: several apps refuse to start on a flag, or a value, they don't know.

/// What a flag does. The UI words it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Control,
    /// The control channel stays closed until the app's token variable is set.
    ControlNeedsToken,
    ControlTokenFile,
    ReadRoot,
    WriteRoot,
    SafeGpu,
    SafeMode,
    NewInstance,
    NewWindow,
    Graphics,
    NoRecover,
    NoAudio,
    Gpu,
    PowerPreference,
    PresentMode,
    MacMenus,
    Sample,
    Empty,
    Recover,
    DataDir,
    Library,
    TwoPaged,
    Theme,
    Scale,
    Language,
}

/// One flag of one app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchFlag {
    pub flag: &'static str,
    pub effect: Effect,
    /// For a flag that takes a value: what it starts with when switched on, empty when only the
    /// user knows it. `None` for a flag that stands alone.
    pub value: Option<&'static str>,
    /// Values to pick from. Another can still be typed.
    pub choices: &'static [&'static str],
}

const fn switch(flag: &'static str, effect: Effect) -> LaunchFlag {
    LaunchFlag { flag, effect, value: None, choices: &[] }
}

const fn valued(flag: &'static str, effect: Effect, value: &'static str) -> LaunchFlag {
    LaunchFlag { flag, effect, value: Some(value), choices: &[] }
}

/// A flag with known values, starting on the first.
const fn choice(flag: &'static str, effect: Effect, choices: &'static [&'static str]) -> LaunchFlag {
    let value = match choices.first() {
        Some(first) => *first,
        None => "",
    };
    LaunchFlag { flag, effect, value: Some(value), choices }
}

/// The control channel agents connect to. Each app starts on its own port, so they can all be open
/// at once.
const fn control(port: &'static str) -> LaunchFlag {
    valued("--control", Effect::Control, port)
}

const TOKEN_FILE: LaunchFlag = valued("--control-token-file", Effect::ControlTokenFile, "");

const READ_ROOT: LaunchFlag = valued("--automation-read-root", Effect::ReadRoot, "");
const WRITE_ROOT: LaunchFlag = valued("--automation-write-root", Effect::WriteRoot, "");
const PRESENT_MODE: LaunchFlag = choice("--present-mode", Effect::PresentMode, &["fifo", "autovsync", "novsync"]);
const MAC_MENUS: LaunchFlag = switch("--in-window-menus", Effect::MacMenus);
const SAMPLE: LaunchFlag = switch("--sample", Effect::Sample);
const DEMO: LaunchFlag = switch("--demo", Effect::Sample);
const EMPTY: LaunchFlag = switch("--empty", Effect::Empty);

impl LaunchFlag {
    /// Whether the app takes the flag more than once, each adding a value.
    pub fn repeats(&self) -> bool {
        matches!(self.effect, Effect::ReadRoot | Effect::WriteRoot)
    }
}

/// The less common flags of the app `id`, picked from a list.
pub fn extras(id: &str) -> &'static [LaunchFlag] {
    use Effect::*;
    match id {
        "photocraft" => const { &[PRESENT_MODE, MAC_MENUS] },
        "vectorcraft" => const { &[MAC_MENUS] },
        "filmcraft" => const { &[DEMO, EMPTY, switch("--recover", Recover), valued("--data-dir", DataDir, "")] },
        "lightcraft" => const { &[valued("--library", Library, ""), DEMO] },
        "pdfcraft" => const { &[switch("--two-paged", TwoPaged)] },
        "effectcraft" | "soundcraft" => const { &[DEMO] },
        "anicraft" => {
            const {
                &[
                    choice("--theme", Theme, &["graphite", "high-contrast"]),
                    choice("--scale", Scale, &["150", "100", "125", "200"]),
                    valued("--lang", Language, "auto"),
                ]
            }
        }
        "designcraft" | "wordcraft" | "deckcraft" | "cadcraft" => const { &[SAMPLE] },
        "gridcraft" => const { &[valued("--sample", Sample, "sales")] },
        "texturecraft" => const { &[EMPTY] },
        _ => &[],
    }
}

/// The flags offered for the app `id`; none for an app whose parser hasn't been read.
pub fn for_app(id: &str) -> &'static [LaunchFlag] {
    use Effect::*;
    match id {
        "photocraft" => {
            const { &[control("7878"), TOKEN_FILE, READ_ROOT, WRITE_ROOT, switch("--safe-gpu", SafeGpu), switch("--new-instance", NewInstance)] }
        }
        "vectorcraft" => const { &[control("7879"), READ_ROOT, WRITE_ROOT, switch("--safe-mode", SafeMode), switch("--new-instance", NewInstance)] },
        "filmcraft" => {
            const {
                &[
                    valued("--control", ControlNeedsToken, "7880"),
                    choice("--graphics", Graphics, &["software", "default", "dx12", "low-power"]),
                    switch("--no-recover", NoRecover),
                ]
            }
        }
        "lightcraft" => const { &[control("7881")] },
        // PdfCraft 0.22.0 takes `--control <file>` where its `main` takes a port, so it isn't offered.
        "pdfcraft" => const { &[switch("--new-window", NewWindow)] },
        "effectcraft" => const { &[control("7883")] },
        "designcraft" => const { &[control("7885")] },
        "figcraft" => const { &[control("7886"), READ_ROOT, WRITE_ROOT] },
        "soundcraft" => const { &[control("7887"), switch("--no-audio", NoAudio)] },
        "wordcraft" => const { &[control("7888")] },
        "deckcraft" => const { &[control("7889")] },
        "cadcraft" => const { &[control("7890")] },
        "gridcraft" => const { &[control("7891")] },
        "texturecraft" => const { &[control("7892"), valued("--gpu", Gpu, ""), choice("--power-pref", PowerPreference, &["high", "low", "none"])] },
        "materialcraft" => const { &[control("7893")] },
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_can_all_be_open_for_agents_at_once() {
        let ports: Vec<&str> = crate::SUITE.iter().filter_map(|app| for_app(app.id).iter().find(|f| f.flag == "--control")?.value).collect();
        let mut distinct = ports.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), ports.len(), "{ports:?}");
    }
}
