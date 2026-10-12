//! Native file services for the hosted PhotoCraft interface.
pub fn supported_file(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| ["png", "jpg", "jpeg", "pcraft"].iter().any(|ext| s.eq_ignore_ascii_case(ext)))
}

pub fn photo() -> photocraft_ui_egui::Services {
    use photocraft_ui_egui::file_dialog::{FileDialogAnswer, FileDialogRequest};
    photocraft_ui_egui::Services {
        import: Some(Box::new(|name, bytes, depth| {
            if !supported_file(name) {
                return Err("This shared viewport preview opens PNG, JPEG, and .pcraft files.".into());
            }
            photocraft_io::import_with_svg_group_depth(name, bytes, depth).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string())
        })),
        export: Some(Box::new(|doc, path, settings| {
            let mut opts = photocraft_io::ExportOptions {
                tiff_layers: settings.tiff_layers,
                xmp: if settings.xmp_all { photocraft_io::XmpEmbed::All } else { photocraft_io::XmpEmbed::None },
                ..Default::default()
            };
            if let Some(q) = settings.jpeg_quality {
                opts.encode.jpeg_quality = q;
            }
            opts.encode.webp_lossless = settings.webp_lossless;
            if let Some(q) = settings.webp_quality {
                opts.encode.webp_quality = q;
            }
            photocraft_io::export(doc, path, &opts).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string())
        })),
        write: Some(Box::new(|path, bytes| photocraft_format::atomic_write(std::path::Path::new(path), bytes).map_err(|e| e.to_string()))),
        file_dialog: Some(Box::new(|request, _, reply| {
            let result = std::thread::Builder::new().name("studio-file-dialog".into()).spawn(move || {
                let answer = match request {
                    FileDialogRequest::Open { multiple, initial_dir, extensions } => {
                        let mut dialog = rfd::FileDialog::new();
                        if let Some(dir) = initial_dir {
                            dialog = dialog.set_directory(dir);
                        }
                        if let Some(ext) = extensions {
                            dialog = dialog.add_filter("Files", &ext);
                        }
                        let paths = if multiple { dialog.pick_files() } else { dialog.pick_file().map(|p| vec![p]) };
                        paths.map(|paths| FileDialogAnswer::Paths(paths.into_iter().map(|p| p.to_string_lossy().into_owned()).collect()))
                    }
                    FileDialogRequest::Save { suggested } => rfd::FileDialog::new()
                        .set_file_name(suggested)
                        .save_file()
                        .map(|p| FileDialogAnswer::SaveTo(p.to_string_lossy().into_owned())),
                };
                reply.send(answer);
            });
            if let Err(error) = result {
                log::error!("could not open file dialog: {error}");
            }
        })),
        ..Default::default()
    }
}
