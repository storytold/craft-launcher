//! Shared, platform-independent validation for the editor provider dialects.
pub(crate) const MAX_ID: usize = 160;
pub(crate) const MAX_FILENAME: usize = 255;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Editor {
    Word,
    Grid,
    Photo,
    Cad,
}

impl Editor {
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn from_id(id: &str) -> Option<Self> {
        match id {
            "wordcraft" => Some(Self::Word),
            "gridcraft" => Some(Self::Grid),
            "photocraft" => Some(Self::Photo),
            "cadcraft" => Some(Self::Cad),
            _ => None,
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Word => "wordcraft",
            Self::Grid => "gridcraft",
            Self::Photo => "photocraft",
            Self::Cad => "cadcraft",
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn names(self) -> (&'static str, &'static str) {
        match self {
            Self::Word => ("wordcraftFileProviders", "wordcraft-file-provider"),
            Self::Grid => ("gridcraftFileProviders", "gridcraft-file-provider"),
            Self::Photo => ("photocraftFileProviders", "photocraft-file-provider"),
            Self::Cad => ("cadcraftFileProviders", "cadcraft-file-provider"),
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn editor_download(self) -> bool {
        matches!(self, Self::Word | Self::Grid)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveOutcome {
    Saved,
    Downloaded,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenOutcome {
    Opened,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventKind {
    ChooseSave,
    Save,
    Open,
    OpenFinished(OpenOutcome),
    SaveUnchanged,
    SaveFinished(SaveOutcome),
}

impl EventKind {
    pub(crate) fn parse(editor: Editor, kind: &str) -> Option<Self> {
        match kind {
            "saveDialog" => Some(Self::ChooseSave),
            "save" => Some(Self::Save),
            "openRequested" if editor == Editor::Cad => Some(Self::Open),
            "openRequest" if editor != Editor::Cad => Some(Self::Open),
            "opened" => Some(Self::OpenFinished(OpenOutcome::Opened)),
            "openFailed" => Some(Self::OpenFinished(OpenOutcome::Failed)),
            "openCancelled" => Some(Self::OpenFinished(OpenOutcome::Cancelled)),
            "saveUnchanged" if editor == Editor::Photo => Some(Self::SaveUnchanged),
            "saved" => Some(Self::SaveFinished(SaveOutcome::Saved)),
            "downloaded" => Some(Self::SaveFinished(SaveOutcome::Downloaded)),
            "saveFailed" => Some(Self::SaveFinished(SaveOutcome::Failed)),
            "saveCancelled" => Some(Self::SaveFinished(SaveOutcome::Cancelled)),
            _ => None,
        }
    }
}

pub(crate) fn decimal_revision(value: &str) -> Option<u64> {
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

pub(crate) fn valid_filename(editor: Editor, name: &str) -> bool {
    valid_text(name, MAX_FILENAME, false)
        && !matches!(name, "." | "..")
        && !name.contains(['/', '\\'])
        && (editor != Editor::Cad || name.to_ascii_lowercase().ends_with(".dxf"))
}

pub(crate) fn valid_id(editor: Editor, id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_') || (editor == Editor::Cad && matches!(b, b'.' | b':')))
}

pub(crate) fn valid_text(value: &str, max: usize, allow_empty: bool) -> bool {
    value.len() <= max && (allow_empty || !value.trim().is_empty()) && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::{Editor, EventKind, decimal_revision, valid_filename, valid_id};

    #[test]
    fn event_dialects_keep_their_distinct_requests() {
        assert_eq!(EventKind::parse(Editor::Cad, "openRequested"), Some(EventKind::Open));
        assert_eq!(EventKind::parse(Editor::Cad, "openRequest"), None);
        assert_eq!(EventKind::parse(Editor::Grid, "openRequest"), Some(EventKind::Open));
        assert_eq!(EventKind::parse(Editor::Photo, "saveUnchanged"), Some(EventKind::SaveUnchanged));
        assert_eq!(EventKind::parse(Editor::Word, "saveUnchanged"), None);
    }

    #[test]
    fn decimal_revisions_are_exact() {
        assert_eq!(decimal_revision("18446744073709551615"), Some(u64::MAX));
        assert_eq!(decimal_revision("18446744073709551616"), None);
        assert_eq!(decimal_revision("+1"), None);
    }

    #[test]
    fn native_filenames_and_ids_are_bounded() {
        assert!(valid_filename(Editor::Cad, "Plan.DXF"));
        assert!(!valid_filename(Editor::Cad, "Plan.dwg"));
        assert!(!valid_filename(Editor::Word, "../report.docx"));
        assert!(!valid_filename(Editor::Photo, &"é".repeat(128)));
        assert!(valid_id(Editor::Cad, "drawing:1.v2"));
        assert!(!valid_id(Editor::Grid, "drawing:1.v2"));
    }
}
