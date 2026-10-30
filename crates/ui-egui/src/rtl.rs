//! Scoped Persian content layout. The upstream egui fork handles logical Unicode text, bidi,
//! shaping, wrapping, selection and accessibility; never pre-reorder strings here.

pub(crate) fn content_row(align: egui::Align) -> egui::Layout {
    if crate::i18n::current().is_rtl() { egui::Layout::right_to_left(align) } else { egui::Layout::left_to_right(align) }
}
pub(crate) fn content_trailing(align: egui::Align) -> egui::Layout {
    if crate::i18n::current().is_rtl() { egui::Layout::left_to_right(align) } else { egui::Layout::right_to_left(align) }
}
pub(crate) fn content_column() -> egui::Layout {
    egui::Layout::top_down(if crate::i18n::current().is_rtl() { egui::Align::Max } else { egui::Align::Min })
}

pub(crate) fn paragraph(ui: &mut egui::Ui, text: &str, font: egui::FontId, color: egui::Color32) -> egui::Response {
    let label = egui::Label::new(egui::RichText::new(text).font(font).color(color)).wrap();
    ui.add(if crate::i18n::current().is_rtl() { label.halign(egui::Align::Max) } else { label })
}

pub(crate) fn label(ui: &mut egui::Ui, text: impl AsRef<str>, font: egui::FontId, color: egui::Color32) -> egui::Response {
    ui.label(egui::RichText::new(text.as_ref()).font(font).color(color))
}

pub(crate) trait LocalizedTooltip {
    fn localized_hover_text(self, text: impl AsRef<str>) -> Self;
}
impl LocalizedTooltip for egui::Response {
    fn localized_hover_text(self, text: impl AsRef<str>) -> Self {
        let text = text.as_ref();
        if !crate::i18n::current().is_rtl() {
            return self.on_hover_text(text);
        }
        self.on_hover_ui(|ui| {
            ui.set_max_width(320.0);
            paragraph(ui, text, egui::TextStyle::Body.resolve(ui.style()), ui.visuals().text_color());
        })
    }
}

pub(crate) fn row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| ui.with_layout(content_row(egui::Align::Center), add).inner).inner
}
