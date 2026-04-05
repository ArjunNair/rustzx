//! GUI module using egui for overlay UI
//!
//! This module provides GUI functionality using egui, which renders as an overlay
//! on top of the emulator screen. The GUI can be toggled with F1 (settings) and F2 (demo).
//!
//! # Usage
//!
//! The GUI is automatically initialized when creating a RustzxApp. To add new panels,
//! modify the `render` method in `GuiContext`.
mod egui_integration;

pub use egui_file_dialog::FileDialog;
pub use egui_integration::EguiIntegration;
pub use egui_sdl2_gl::egui;
use std::path::PathBuf;
use std::sync::Arc;

/// GUI context that can be used to render egui panels
pub struct GuiContext {
    pub egui_integration: EguiIntegration,
    pub show_demo: bool,
    pub show_settings: bool,
    pub file_dialog: FileDialog,
    pub picked_file: Option<PathBuf>,
}

const SUPPORTED_TAPE_EXT: [&str; 2] = ["tap", "tzx"];
const SUPPORTED_SNAPSHOT_EXT: [&str; 2] = ["sna", "szx"];

impl GuiContext {
    pub fn new(window: &sdl2::video::Window) -> Self {
        Self {
            egui_integration: EguiIntegration::new(window),
            show_demo: false,
            show_settings: false,
            file_dialog: FileDialog::new() // Add file filters the user can select in the bottom right
                .add_file_filter(
                    "Tape files",
                    Arc::new(|p| {
                        SUPPORTED_TAPE_EXT.contains(
                            &p.extension()
                                .unwrap_or_default()
                                .to_str()
                                .unwrap_or_default(),
                        )
                    }),
                )
                .add_file_filter(
                    "Snapshots files",
                    Arc::new(|p| {
                        SUPPORTED_SNAPSHOT_EXT.contains(
                            &p.extension()
                                .unwrap_or_default()
                                .to_str()
                                .unwrap_or_default(),
                        )
                    }),
                ),
            picked_file: None,
        }
    }

    /// Handle SDL2 events and update egui state
    pub fn handle_event(&mut self, window: &sdl2::video::Window, event: &sdl2::event::Event) {
        self.egui_integration.handle_event(window, event);
    }

    /// Render the GUI panels
    pub fn render(&mut self, ctx: &egui::Context) {
        // Example: Settings panel
        if self.show_settings {
            egui::Window::new("Rust ZX")
                .collapsible(true)
                .resizable(true)
                .default_width(400.0)
                .show(ctx, |ui| {
                    if ui.button("Open File").clicked() {
                        self.file_dialog.pick_file();
                    }
                    ui.label(format!("Picked file: {:?}", self.picked_file));
                    self.file_dialog.update(ctx);
                    if let Some(path) = self.file_dialog.take_picked() {
                        self.picked_file = Some(path);
                    }
                });
        }
    }
}
