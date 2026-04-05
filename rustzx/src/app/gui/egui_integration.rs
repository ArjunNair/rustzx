//! egui integration with SDL2 and OpenGL
use egui_sdl2_gl::sdl2::video::Window;
use egui_sdl2_gl::{egui, gl, painter::Painter, DpiScaling, EguiStateHandler, ShaderVersion};

/// Wrapper around egui_sdl2_gl for easier integration
pub struct EguiIntegration {
    pub painter: Painter,
    pub state_handler: EguiStateHandler,
    pub context: egui::Context,
}

impl EguiIntegration {
    pub fn new(window: &Window) -> Self {
        // NOTE: GL context should already be current and GL functions loaded
        // by the caller (in RustzxApp::from_config) before calling this.
        // We're just initializing egui_sdl2_gl here.

        let (painter, state_handler) =
            egui_sdl2_gl::with_sdl2(window, ShaderVersion::Default, DpiScaling::Default);
        let context = egui::Context::default();

        // The default context should already have fonts loaded
        // Don't override them as that might cause issues

        Self {
            painter,
            state_handler,
            context,
        }
    }

    /// Handle SDL2 events and update egui state
    pub fn handle_event(&mut self, window: &Window, event: &sdl2::event::Event) {
        self.state_handler
            .process_input(window, event.clone(), &mut self.painter);
    }

    /// Begin a new egui frame
    pub fn begin_frame(&mut self, window: &Window) {
        // Update the painter's screen rect through the state_handler
        // This ensures the painter knows the correct screen dimensions
        let size = window.size();
        let screen_rect = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(size.0 as f32, size.1 as f32),
        );
        
        // Update painter's screen rect directly
        self.painter.screen_rect = screen_rect;
        
        // Update input for the frame
        // process_input() should have updated state_handler.input, so we take it here
        let mut raw_input = self.state_handler.input.take();

        // Update screen rect in raw_input as well
        raw_input.screen_rect = Some(screen_rect);

        self.context.begin_pass(raw_input);
    }

    /// Get the egui context for rendering UI
    pub fn context(&self) -> egui::Context {
        self.context.clone()
    }

    /// End the frame and render egui
    pub fn end_frame(&mut self, window: &Window) -> egui::FullOutput {
        let full_output = self.context.end_pass();

        // Process output (clipboard, cursor, etc.)
        self.state_handler
            .process_output(window, &full_output.platform_output);

        full_output
    }

    /// Paint egui to the screen
    pub fn paint(&mut self, window: &Window, full_output: egui::FullOutput) {
        // Clear any OpenGL errors from previous operations (like video rendering)
        unsafe {
            loop {
                let err = gl::GetError();
                if err == gl::NO_ERROR {
                    break;
                }
            }
        }
        
        // Update painter's screen rect to match window size
        // The state_handler should update this, but we ensure it's correct
        let size = window.size();
        let screen_rect = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(size.0 as f32, size.1 as f32),
        );
        // Update through state_handler if possible, otherwise directly
        self.painter.screen_rect = screen_rect;
        
        // Reset OpenGL state that video rendering might have modified
        // This is critical to avoid conflicts with egui's painter
        // But be careful not to reset things the painter needs
        unsafe {
            // Set viewport
            gl::Viewport(0, 0, size.0 as i32, size.1 as i32);
            
            // Reset texture bindings - video code might have left textures bound
            // Unbind all texture units that might be in use
            for i in 0..8 {
                gl::ActiveTexture(gl::TEXTURE0 + i);
                gl::BindTexture(gl::TEXTURE_2D, 0);
            }
            // Make sure we're back on texture unit 0 (the default)
            gl::ActiveTexture(gl::TEXTURE0);
            
            // Reset vertex array - video code uses its own VAO
            gl::BindVertexArray(0);
            
            // Reset framebuffer to default
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            
            // Reset buffer bindings
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, 0);
            
            // Ensure blending is enabled (egui needs this)
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
            
            // Disable depth test (egui doesn't use it)
            gl::Disable(gl::DEPTH_TEST);
            
            // Disable any other state that might interfere
            gl::Disable(gl::CULL_FACE);
            gl::Disable(gl::SCISSOR_TEST);
            
            // Ensure pixel store is set to defaults (important for texture uploads)
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 4);
            gl::PixelStorei(gl::UNPACK_ROW_LENGTH, 0);
            gl::PixelStorei(gl::UNPACK_SKIP_PIXELS, 0);
            gl::PixelStorei(gl::UNPACK_SKIP_ROWS, 0);
        }
        
        // Tessellate shapes into primitives
        let primitives = self.context.tessellate(
            full_output.shapes,
            self.state_handler.native_pixels_per_point,
        );
        
        // The painter will handle its own OpenGL state setup
        // Pass None as clear color to prevent clearing the framebuffer (we want to render on top of existing content)
        self.painter
            .paint_jobs(None, full_output.textures_delta, primitives);
    }
}
