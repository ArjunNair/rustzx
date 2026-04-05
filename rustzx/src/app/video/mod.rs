//! platform-independent traits. Submodules with backends will be selectable
//! via cargo features in future
mod palette;
mod video_sdl;

pub use palette::Palette;
pub use video_sdl::VideoSdl;

/// Texture id binging
#[derive(PartialEq, Eq, Hash, Copy, Clone)]
pub struct TextureInfo {
    pub id: usize,
    pub width: u32,
    pub height: u32,
}

/// Simple rect struct
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// Constructs new rect
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }
}

/// provides video functionality through real backend to emulator
pub trait VideoDevice {
    /// generates and returns texture handle
    fn gen_texture(&mut self, width: u32, height: u32) -> TextureInfo;
    /// changes window title
    fn set_title(&mut self, title: &str);
    /// updates texture data
    fn update_texture(&mut self, tex: TextureInfo, buffer: &[u8]);
    /// starts render block
    fn begin(&mut self);
    /// draws plain texture into destination rect
    fn draw_texture_2d(&mut self, tex: TextureInfo, rect: Option<Rect>);
    /// finishes rendering
    fn end(&mut self);
    /// Get a reference to the SDL2 window (for egui integration)
    fn window(&self) -> &sdl2::video::Window;
    /// Make the OpenGL context current (needed before egui operations)
    /// Default implementation does nothing (for backends that don't use OpenGL)
    fn make_gl_context_current(&self) {
        // Default: no-op
    }
}
