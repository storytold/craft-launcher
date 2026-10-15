//! What Craft AI does inside the launcher. Craft AI is one assistant across the ArtCraft family
//! — the model, the runtime and the shared folder belong to the family — and the launcher is
//! the family's hub, so its tasks are the family's: which app for this, what each app is for,
//! the model advice for this machine, and prompting tips. The same window in every other app
//! is specialised for that app instead — same model, same shared folder, different craft.
//!
//! The labels and hints are catalog keys: the dialog shows them translated.

/// The app Craft AI is embedded in, as the shared folder's records name it.
pub const APP: &str = "Craft Launcher";

/// One ready-made task of the Assistant tab.
pub struct Task {
    /// The label the picker shows (a catalog key).
    pub label: &'static str,
    /// The input field's hint (a catalog key).
    pub hint: &'static str,
}

/// The tasks, in the picker's order.
pub const TASKS: &[Task] = &[
    Task { label: "Ask anything", hint: "Ask in any language" },
    Task { label: "Which app for this?", hint: "What you want to make or do" },
    Task { label: "Family overview", hint: "What each Crafting App is for" },
    Task { label: "Model advice for my machine", hint: "What this machine has (RAM, disk), and what you do with it" },
    Task { label: "Prompting tips", hint: "What you are trying to get from the model" },
];

/// Tell the model where it is and how to answer: briefly, practically, in the language the user
/// wrote in, about the family this launcher serves. The last line is the honesty clause: a
/// local model can be wrong, and it is told to say so rather than invent.
pub fn system_prompt() -> String {
    "You are Craft AI, the assistant built into the ArtCraft Launcher, the hub that installs \
     and updates the ArtCraft family of apps. The family, each in its own words: GridCraft — \
     spreadsheets; WordCraft — documents; DeckCraft — presentations; SoundCraft — audio \
     recording and mixing; CADCraft — 2D/3D drafting; LightCraft — photo developing; \
     FilmCraft — video editing; EffectCraft — motion graphics and visual effects; PhotoCraft — \
     image editing; PDFCraft — PDFs; DesignCraft — page layout; VectorCraft — vector art; \
     ArtCraft — generative image, video and audio creation. The launcher installs them, checks \
     for updates, and shows each app's details. Craft AI's models live in a folder the whole \
     family shares — one download serves every app. Answer in the language the user wrote in, \
     Arabic included. Be brief and practical: name the app or the step, give a short list, and \
     when a question is not about the family, answer it anyway, briefly. Never invent app \
     names, menu entries or version numbers: if you are not sure, say so plainly."
        .to_string()
}

/// Frame the user's input for the task they picked.
pub fn user_prompt(task: usize, input: &str) -> String {
    let input = input.trim();
    match TASKS[task.min(TASKS.len() - 1)].label {
        "Which app for this?" => format!(
            "Which ArtCraft app fits this, and what would it cost to try — name the app and say why: {input}"
        ),
        "Family overview" => format!(
            "Describe the ArtCraft family for this purpose — what each app does, in one line each: {input}"
        ),
        "Model advice for my machine" => format!(
            "Advise on a Craft AI model for this machine and this use — RAM and disk against the catalogue, which Gemma fits, what to watch out for: {input}"
        ),
        "Prompting tips" => format!(
            "Give prompting tips for a local model on this — how to ask, what to include, what to expect: {input}"
        ),
        _ => input.to_string(),
    }
}
