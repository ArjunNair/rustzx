use super::{Rect, TextureInfo, VideoDevice};
use crate::{app::settings::Settings, backends::SDL_CONTEXT};
use rustzx_core::zx::constants::{SCREEN_HEIGHT, SCREEN_WIDTH};
#[cfg(feature = "gui")]
use egui_sdl2_gl::{gl, sdl2};
#[cfg(not(feature = "gui"))]
use gl;
#[cfg(not(feature = "gui"))]
use sdl2;
use sdl2::video::{Window, GLContext};
use std::collections::HashMap;

/// OpenGL texture wrapper
struct GlTexture {
    id: u32,
    width: u32,
    height: u32,
}

impl Drop for GlTexture {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteTextures(1, &self.id);
        }
    }
}

/// Represents SDL video backend using pure OpenGL
/// This allows egui_sdl2_gl to work correctly (no SDL2 Canvas which conflicts with OpenGL)
pub struct VideoSdl {
    window: Window,
    gl_context: GLContext,
    textures: HashMap<TextureInfo, GlTexture>,
    next_tex_id: usize,
    screen_width: u32,
    screen_height: u32,
    // Shader program and VAO for rendering textured quads
    shader_program: u32,
    vao: u32,
    vbo: u32,
}

impl VideoSdl {
    /// constructs new renderer with application settings
    pub fn new(settings: &Settings) -> VideoSdl {
        // init video subsystem
        let mut video_subsystem = None;
        SDL_CONTEXT.with(|sdl| {
            video_subsystem = sdl.borrow_mut().video().ok();
        });
        
        if let Some(video) = video_subsystem {
            // Set OpenGL attributes
            let gl_attr = video.gl_attr();
            gl_attr.set_context_version(3, 3);
            gl_attr.set_context_profile(sdl2::video::GLProfile::Core);
            gl_attr.set_double_buffer(true);
            
            // construct window
            let (width, height) = (
                SCREEN_WIDTH * settings.scale,
                SCREEN_HEIGHT * settings.scale,
            );
            let window = video
                .window("RustZX", width as u32, height as u32)
                .position_centered()
                .opengl()
                .build()
                .expect("[ERROR] Sdl window build fail");
            
            // Create OpenGL context
            let gl_context = window.gl_create_context()
                .expect("[ERROR] Failed to create OpenGL context");
            window.gl_make_current(&gl_context)
                .expect("[ERROR] Failed to make OpenGL context current");
            
            // Enable vsync
            video.gl_set_swap_interval(sdl2::video::SwapInterval::VSync)
                .unwrap_or_else(|e| eprintln!("Warning: Failed to set vsync: {}", e));
            
            // Load OpenGL function pointers
            gl::load_with(|s| video.gl_get_proc_address(s) as *const _);
            
            // Verify OpenGL is working
            unsafe {
                let version = gl::GetString(gl::VERSION);
                if !version.is_null() {
                    let version_str = std::ffi::CStr::from_ptr(version as *const i8);
                    log::info!("OpenGL version: {:?}", version_str);
                } else {
                    log::warn!("Could not get OpenGL version string");
                }
            }
            
            // Create shader program and VAO for rendering
            let (shader_program, vao, vbo) = Self::create_render_resources();
            
            // Set up OpenGL state
            unsafe {
                gl::Viewport(0, 0, width as i32, height as i32);
                gl::Enable(gl::BLEND);
                gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
            }
            
            VideoSdl {
                window,
                gl_context,
                textures: HashMap::new(),
                next_tex_id: 0,
                screen_width: width as u32,
                screen_height: height as u32,
                shader_program,
                vao,
                vbo,
            }
        } else {
            panic!("[ERROR] Sdl video init fail!");
        }
    }
    
    /// Create shader program and VAO for rendering textured quads
    fn create_render_resources() -> (u32, u32, u32) {
        let vertex_shader_src = r#"
            #version 330 core
            layout (location = 0) in vec2 aPos;
            layout (location = 1) in vec2 aTexCoord;
            out vec2 TexCoord;
            uniform vec4 destRect;  // x, y, w, h in normalized coords
            void main() {
                // Transform vertex position from [0,1] to destination rect in normalized coords
                vec2 pos = aPos * destRect.zw + destRect.xy;
                // Convert to NDC [-1, 1] and flip Y for OpenGL coordinate system
                gl_Position = vec4(pos.x * 2.0 - 1.0, 1.0 - pos.y * 2.0, 0.0, 1.0);
                TexCoord = aTexCoord;
            }
        "#;
        
        let fragment_shader_src = r#"
            #version 330 core
            in vec2 TexCoord;
            out vec4 FragColor;
            uniform sampler2D tex;
            void main() {
                FragColor = texture(tex, TexCoord);
            }
        "#;
        
        unsafe {
            // Compile vertex shader
            let vertex_shader = gl::CreateShader(gl::VERTEX_SHADER);
            let c_str = std::ffi::CString::new(vertex_shader_src).unwrap();
            gl::ShaderSource(vertex_shader, 1, &c_str.as_ptr(), std::ptr::null());
            gl::CompileShader(vertex_shader);
            check_shader_compile(vertex_shader, "vertex");
            
            // Compile fragment shader
            let fragment_shader = gl::CreateShader(gl::FRAGMENT_SHADER);
            let c_str = std::ffi::CString::new(fragment_shader_src).unwrap();
            gl::ShaderSource(fragment_shader, 1, &c_str.as_ptr(), std::ptr::null());
            gl::CompileShader(fragment_shader);
            check_shader_compile(fragment_shader, "fragment");
            
            // Link program
            let program = gl::CreateProgram();
            gl::AttachShader(program, vertex_shader);
            gl::AttachShader(program, fragment_shader);
            gl::LinkProgram(program);
            check_program_link(program);
            
            // Check if program is valid
            let mut link_status = 0;
            gl::GetProgramiv(program, gl::LINK_STATUS, &mut link_status);
            if link_status == 0 {
                panic!("Shader program failed to link!");
            }
            
            // Clean up shaders
            gl::DeleteShader(vertex_shader);
            gl::DeleteShader(fragment_shader);
            
            // Create VAO and VBO for a quad
            let mut vao = 0;
            let mut vbo = 0;
            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut vbo);
            
            gl::BindVertexArray(vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
            
            // Quad vertices: position (2) + texcoord (2)
            #[rustfmt::skip]
            let vertices: [f32; 24] = [
                // pos      // tex
                0.0, 0.0,   0.0, 0.0,
                1.0, 0.0,   1.0, 0.0,
                1.0, 1.0,   1.0, 1.0,
                0.0, 0.0,   0.0, 0.0,
                1.0, 1.0,   1.0, 1.0,
                0.0, 1.0,   0.0, 1.0,
            ];
            
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (vertices.len() * std::mem::size_of::<f32>()) as isize,
                vertices.as_ptr() as *const _,
                gl::STATIC_DRAW,
            );
            
            // Position attribute
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, 4 * std::mem::size_of::<f32>() as i32, std::ptr::null());
            gl::EnableVertexAttribArray(0);
            
            // TexCoord attribute
            gl::VertexAttribPointer(1, 2, gl::FLOAT, gl::FALSE, 4 * std::mem::size_of::<f32>() as i32, (2 * std::mem::size_of::<f32>()) as *const _);
            gl::EnableVertexAttribArray(1);
            
            gl::BindVertexArray(0);
            
            (program, vao, vbo)
        }
    }
}

unsafe fn check_shader_compile(shader: u32, shader_type: &str) {
    let mut success = 0;
    gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut success);
    if success == 0 {
        let mut log = vec![0u8; 512];
        gl::GetShaderInfoLog(shader, 512, std::ptr::null_mut(), log.as_mut_ptr() as *mut _);
        eprintln!("Shader ({}) compile error: {}", shader_type, String::from_utf8_lossy(&log));
    }
}

unsafe fn check_program_link(program: u32) {
    let mut success = 0;
    gl::GetProgramiv(program, gl::LINK_STATUS, &mut success);
    if success == 0 {
        let mut log = vec![0u8; 512];
        gl::GetProgramInfoLog(program, 512, std::ptr::null_mut(), log.as_mut_ptr() as *mut _);
        eprintln!("Program link error: {}", String::from_utf8_lossy(&log));
    }
}

impl VideoDevice for VideoSdl {
    fn gen_texture(&mut self, width: u32, height: u32) -> TextureInfo {
        let mut texture_id: u32 = 0;
        unsafe {
            gl::GenTextures(1, &mut texture_id);
            gl::BindTexture(gl::TEXTURE_2D, texture_id);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            
            // Allocate texture storage
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                width as i32,
                height as i32,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                std::ptr::null(),
            );
            gl::BindTexture(gl::TEXTURE_2D, 0);
        }
        
        let id = self.next_tex_id;
        let tex_info = TextureInfo { id, width, height };
        self.textures.insert(tex_info, GlTexture { id: texture_id, width, height });
        self.next_tex_id += 1;
        tex_info
    }

    fn set_title(&mut self, title: &str) {
        self.window.set_title(title).unwrap();
    }

    fn update_texture(&mut self, tex: TextureInfo, buffer: &[u8]) {
        if let Some(gl_tex) = self.textures.get(&tex) {
            unsafe {
                // Ensure GL context is current
                self.make_gl_context_current();
                
                gl::BindTexture(gl::TEXTURE_2D, gl_tex.id);
                
                // Verify buffer size matches expected
                let expected_size = (tex.width * tex.height * 4) as usize;
                if buffer.len() != expected_size {
                    eprintln!("Warning: Texture buffer size mismatch: expected {}, got {}", expected_size, buffer.len());
                }
                
                gl::TexSubImage2D(
                    gl::TEXTURE_2D,
                    0,
                    0,
                    0,
                    tex.width as i32,
                    tex.height as i32,
                    gl::RGBA,
                    gl::UNSIGNED_BYTE,
                    buffer.as_ptr() as *const _,
                );
                
                // Check for OpenGL errors
                let err = gl::GetError();
                if err != gl::NO_ERROR {
                    eprintln!("OpenGL error after TexSubImage2D: 0x{:x}", err);
                }
                
                gl::BindTexture(gl::TEXTURE_2D, 0);
            }
        }
    }

    fn begin(&mut self) {
        unsafe {
            // Ensure viewport is set correctly
            gl::Viewport(0, 0, self.screen_width as i32, self.screen_height as i32);
            
            // Clear to black
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
            
            // Enable blending for transparency
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
        }
    }

    fn draw_texture_2d(&mut self, tex: TextureInfo, rect: Option<Rect>) {
        if let Some(gl_tex) = self.textures.get(&tex) {
            let (x, y, w, h) = if let Some(r) = rect {
                (r.x as f32, r.y as f32, r.w as f32, r.h as f32)
            } else {
                (0.0, 0.0, self.screen_width as f32, self.screen_height as f32)
            };
            
            // Normalize to 0..1 range
            let sw = self.screen_width as f32;
            let sh = self.screen_height as f32;
            let dest_rect = [x / sw, y / sh, w / sw, h / sh];
            
            unsafe {
                // Set up OpenGL state
                gl::Disable(gl::DEPTH_TEST);
                gl::Enable(gl::BLEND);
                gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
                
                gl::UseProgram(self.shader_program);
                
                // Set destRect uniform
                let dest_rect_loc = gl::GetUniformLocation(self.shader_program, b"destRect\0".as_ptr() as *const _);
                if dest_rect_loc != -1 {
                    gl::Uniform4f(dest_rect_loc, dest_rect[0], dest_rect[1], dest_rect[2], dest_rect[3]);
                } else {
                    eprintln!("Warning: destRect uniform not found in shader");
                }
                
                // Set texture uniform (sampler2D should be at location 0 by default, but let's be explicit)
                let tex_loc = gl::GetUniformLocation(self.shader_program, b"tex\0".as_ptr() as *const _);
                if tex_loc != -1 {
                    gl::Uniform1i(tex_loc, 0);  // Texture unit 0
                } else {
                    eprintln!("Warning: tex uniform not found in shader");
                }
                
                // Bind texture to texture unit 0
                gl::ActiveTexture(gl::TEXTURE0);
                gl::BindTexture(gl::TEXTURE_2D, gl_tex.id);
                
                // Verify texture is bound
                let mut bound_texture = 0i32;
                gl::GetIntegerv(gl::TEXTURE_BINDING_2D, &mut bound_texture);
                if bound_texture != gl_tex.id as i32 {
                    eprintln!("Warning: Failed to bind texture {} (got {})", gl_tex.id, bound_texture);
                }
                
                // Draw the quad
                gl::BindVertexArray(self.vao);
                gl::DrawArrays(gl::TRIANGLES, 0, 6);
                
                // Check for OpenGL errors
                let err = gl::GetError();
                if err != gl::NO_ERROR {
                    eprintln!("OpenGL error after DrawArrays: 0x{:x}", err);
                }
                
                gl::BindVertexArray(0);
                
                // Cleanup
                gl::BindTexture(gl::TEXTURE_2D, 0);
                gl::UseProgram(0);
            }
        } else {
            eprintln!("Warning: Texture id={} not found", tex.id);
        }
    }

    fn end(&mut self) {
        // Swap buffers
        self.window.gl_swap_window();
    }

    fn window(&self) -> &sdl2::video::Window {
        &self.window
    }
    
    fn make_gl_context_current(&self) {
        self.window.gl_make_current(&self.gl_context)
            .expect("Failed to make OpenGL context current");
    }
}

impl Drop for VideoSdl {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteProgram(self.shader_program);
            gl::DeleteVertexArrays(1, &self.vao);
            gl::DeleteBuffers(1, &self.vbo);
        }
    }
}
