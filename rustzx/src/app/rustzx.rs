//! Main application class module
//! Handles all platform-related, hardware-related stuff
//! and command-line interface

use crate::{
    app::{
        events::{Event, EventDevice, EventsSdl},
        settings::{machine_from_str, Settings, SoundBackend},
        sound::{SoundDevice, DEFAULT_SAMPLE_RATE},
        video::{Rect, TextureInfo, VideoDevice, VideoSdl},
    },
    host::{self, AppHost, AppHostContext, DetectedFileKind},
};
#[cfg(feature = "gui")]
use crate::app::gui::GuiContext;
use anyhow::{anyhow, Context};
use rustzx_core::{
    host::SnapshotRecorder,
    zx::{
        constants::{
            CANVAS_HEIGHT, CANVAS_WIDTH, CANVAS_X, CANVAS_Y, FPS, SCREEN_HEIGHT, SCREEN_WIDTH,
        },
        machine::ZXMachine,
    },
    Emulator,
};
use rustzx_utils::io::FileAsset;
use std::{
    collections::VecDeque,
    fs::{self, File},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

/// max 100 ms interval in `max frames` speed mode
const MAX_FRAME_TIME: Duration = Duration::from_millis(100);

/// returns frame length from given `fps`
fn frame_length(fps: usize) -> Duration {
    Duration::from_millis((1000_f64 / fps as f64) as u64)
}

pub enum AppState {
    Running,
    Paused,
    Exit,
}

/// Application instance type
pub struct RustzxApp {
    /// main emulator object
    emulator: Emulator<AppHost>,
    /// Sound rendering in a separate thread
    snd: Option<Box<dyn SoundDevice>>,
    video: Box<dyn VideoDevice>,
    events: EventsSdl,
    tex_border: TextureInfo,
    tex_canvas: TextureInfo,
    scale: u32,
    settings: Settings,

    enable_frame_trace: bool,
    enable_joy_keyaboard_layer: bool,
    #[cfg(feature = "gui")]
    gui: Option<GuiContext>,
    app_events: VecDeque<Event>,
    app_state: AppState,
}

impl RustzxApp {
    /// Starts application itself
    pub fn from_config(settings: Settings) -> anyhow::Result<RustzxApp> {
        let snd = if !settings.disable_sound {
            let backend = create_sound_backend(&settings).context(
                "Failed to initialize sound subsystem, try other sound backend or --nosound option",
            )?;

            Some(backend)
        } else {
            None
        };
        let mut video = Box::new(VideoSdl::new(&settings));
        let tex_border = video.gen_texture(SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        let tex_canvas = video.gen_texture(CANVAS_WIDTH as u32, CANVAS_HEIGHT as u32);
        let scale = settings.scale as u32;
        let events = EventsSdl::new(&settings);
        let sample_rate = snd
            .as_ref()
            .map(|s| s.sample_rate())
            .unwrap_or(DEFAULT_SAMPLE_RATE);

        let mut emulator = Emulator::new(settings.to_rustzx_settings(sample_rate), AppHostContext)
            .map_err(|e| anyhow!("Failed to construct emulator: {}", e))?;

        if let Some(rom) = settings.rom.as_ref() {
            emulator
                .load_rom(host::load_rom(rom, settings.machine)?)
                .map_err(|e| anyhow!("Emulator failed to load rom: {}", e))?;
        }
        if let Some(snapshot) = settings.snap.as_ref() {
            emulator
                .load_snapshot(host::load_snapshot(snapshot)?)
                .map_err(|e| anyhow!("Emulator failed to load snapshot: {}", e))?;
        }
        if let Some(tape) = settings.tape.as_ref() {
            emulator
                .load_tape(host::load_tape(tape)?)
                .map_err(|e| anyhow!("Emulator failed to load tape: {}", e))?;
        }
        if let Some(screen) = settings.screen.as_ref() {
            emulator
                .load_screen(host::load_screen(screen)?)
                .map_err(|e| anyhow!("Emulator failed to load screen: {}", e))?;
        }

        let file_autodetect = settings.file_autodetect.clone();

        #[cfg(feature = "gui")]
        let gui = if !settings.disable_gui {
            video.make_gl_context_current();
            Some(GuiContext::new(video.window()))
        } else {
            None
        };

        let mut app = RustzxApp {
            emulator,
            snd,
            video,
            events,
            tex_border,
            tex_canvas,
            scale,
            settings,
            enable_frame_trace: false, /*cfg!(debug_assertions)*/
            enable_joy_keyaboard_layer: false,
            #[cfg(feature = "gui")]
            gui,
            app_events: VecDeque::new(),
            app_state: AppState::Running,
        };

        if let Some(file) = file_autodetect.as_ref() {
            app.load_file_autodetect(file)?;
        }

        app.update_window_title();

        Ok(app)
    }

    fn update_window_title(&mut self) {
        let mut title = format!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

        if self.enable_joy_keyaboard_layer {
            title.push_str(" [JOY]");
        }

        if self.enable_frame_trace {
            title.push_str(" [FRAME_TRACE]");
        }

        self.video.set_title(&title);
    }

    fn handle_events(&mut self, event: crate::app::events::Event) -> anyhow::Result<AppState> {
        let mut app_state = AppState::Running;
        log::debug!("Handling event: {:?}", event);
        match event {
            Event::Exit => {
                app_state = AppState::Exit;
            }
            Event::ZXKey(key, state) => {
                self.emulator.send_key(key, state);
            }
            Event::SwitchFrameTrace => {
                self.enable_frame_trace = !self.enable_frame_trace;
                self.update_window_title();
            }
            Event::ChangeJoyKeyboardLayer(value) => {
                self.enable_joy_keyaboard_layer = value;
                self.update_window_title();
            }
            Event::ChangeSpeed(speed) => {
                self.emulator.set_speed(speed);
            }
            Event::Kempston(key, state) => {
                self.emulator.send_kempston_key(key, state);
            }
            Event::Sinclair(num, key, state) => {
                self.emulator.send_sinclair_key(num, key, state);
            }
            Event::CompoundKey(key, state) => {
                self.emulator.send_compound_key(key, state);
            }
            Event::MouseMove { x, y } => {
                self.emulator.send_mouse_pos_diff(x, y);
            }
            Event::MouseButton(button, pressed) => {
                self.emulator.send_mouse_button(button, pressed);
            }
            Event::MouseWheel(direction) => {
                self.emulator.send_mouse_wheel(direction);
            }
            Event::InsertTape => self.emulator.play_tape(),
            Event::StopTape => self.emulator.stop_tape(),
            Event::OpenFile(path) => self.load_file_autodetect(&path)?,
            Event::QuickSave => self.quick_save()?,
            Event::QuickLoad => self.quick_load()?,
            Event::Reset => self.reset_emulation(),
            Event::ChangeMachine(machine) => self.change_machine(machine.as_str()),
        }
        Ok(app_state)
    }

    // TBD: Remove this!
    pub fn change_machine(&mut self, machine: &str) {
        self.settings.machine = machine_from_str(machine).unwrap();
        self.emulator = Emulator::new(
            self.settings
                .to_rustzx_settings(self.settings.sound_sample_rate.unwrap()),
            AppHostContext,
        )
        .map_err(|e| anyhow!("Failed to construct emulator: {}", e))
        .unwrap();

        self.emulator.reset();
    }

    pub fn reset_emulation(&mut self) {
        self.emulator.reset();
    }

    pub fn start(&mut self) -> anyhow::Result<()> {
        let scale = self.scale;
        let mut frame_count: u64 = 0;
        'emulator: loop {
            frame_count = frame_count.wrapping_add(1);
            let frame_target_dt = frame_length(FPS);
            // absolute start time
            let frame_start = Instant::now();
            // Emulate all requested frames
            let emulator_dt = self
                .emulator
                .emulate_frames(MAX_FRAME_TIME)
                .map_err(|e| anyhow!("Emulation step failed: {:#?}", e))?
                .duration;
            // Match others: only move samples to sound when sound is enabled (normal speed)
            if let Some(ref mut snd) = self.snd {
                if self.emulator.have_sound() {
                    while let Some(sample) = self.emulator.next_audio_sample() {
                        snd.send_sample(sample);
                    }
                }
            }

            self.video
                .update_texture(self.tex_border, self.emulator.border_buffer().rgba_data());
            self.video
                .update_texture(self.tex_canvas, self.emulator.screen_buffer().rgba_data());

            // Ensure OpenGL context is current before rendering
            self.video.make_gl_context_current();
            self.video.begin();
            self.video.draw_texture_2d(
                self.tex_border,
                Some(Rect::new(
                    0,
                    0,
                    SCREEN_WIDTH as u32 * scale,
                    SCREEN_HEIGHT as u32 * scale,
                )),
            );
            self.video.draw_texture_2d(
                self.tex_canvas,
                Some(Rect::new(
                    CANVAS_X as i32 * scale as i32,
                    CANVAS_Y as i32 * scale as i32,
                    CANVAS_WIDTH as u32 * scale,
                    CANVAS_HEIGHT as u32 * scale,
                )),
            );

            #[cfg(feature = "gui")]
            let run_gui_block = self.gui.is_some();

            #[cfg(feature = "gui")]
            if run_gui_block {
                // DON'T call video.end() yet - render egui first, then swap buffers
                let mut sdl_events = Vec::new();
                let mut f1_pressed = false;
                let mut f2_pressed = false;

                while let Some(sdl_event) = self.events.poll_raw_event() {
                    if let sdl2::event::Event::KeyDown {
                        keycode: Some(k), ..
                    } = &sdl_event
                    {
                        if *k == sdl2::keyboard::Keycode::F1 {
                            f1_pressed = true;
                        } else if *k == sdl2::keyboard::Keycode::F2 {
                            f2_pressed = true;
                        }
                    }
                    sdl_events.push(sdl_event);
                }

                let run_egui = !self.emulator.have_sound()
                    || std::env::var("RUSTZX_EGUI_WITH_SOUND").as_deref() == Ok("1");

                if run_egui {
                    if let Some(ref mut gui) = self.gui {
                        let window = self.video.window();
                        self.video.make_gl_context_current();
                        if f1_pressed {
                            gui.show_settings = !gui.show_settings;
                        }
                        if f2_pressed {
                            gui.show_demo = !gui.show_demo;
                        }
                        for sdl_event in &sdl_events {
                            gui.egui_integration.handle_event(window, sdl_event);
                        }
                        gui.egui_integration.begin_frame(window);
                        let ctx = gui.egui_integration.context();
                        gui.render(&ctx);
                        let full_output = gui.egui_integration.end_frame(window);
                        if let Some(path) = gui.picked_file.take() {
                            self.app_events
                                .push_back(Event::OpenFile(path.to_path_buf()));
                        }
                        gui.egui_integration.paint(window, full_output);
                    }
                }

                self.video.end();

                for sdl_event in &sdl_events {
                    if let Some(event) = self.events.sdl_event_to_emulator_event(sdl_event) {
                        let app_state = self.handle_events(event)?;
                        if let AppState::Exit = app_state {
                            break 'emulator;
                        }
                        self.app_state = app_state;
                    }
                }
            }

            #[cfg(not(feature = "gui"))]
            {
                self.video.end();
                while let Some(event) = self.events.pop_event() {
                    let app_state = self.handle_events(event)?;
                    if let AppState::Exit = app_state {
                        break 'emulator;
                    }
                    self.app_state = app_state;
                }
            }

            #[cfg(feature = "gui")]
            if !run_gui_block {
                self.video.end();
                while let Some(event) = self.events.pop_event() {
                    let app_state = self.handle_events(event)?;
                    if let AppState::Exit = app_state {
                        break 'emulator;
                    }
                    self.app_state = app_state;
                }
            }

            while let Some(event) = self.app_events.pop_front() {
                let app_state = self.handle_events(event)?;
                if let AppState::Exit = app_state {
                    break 'emulator;
                }
                self.app_state = app_state;
            }
            // how long emulation iteration was
            let emulation_dt = frame_start.elapsed();
            if emulation_dt < frame_target_dt {
                let wait_koef = if self.emulator.have_sound() { 9 } else { 10 };
                thread::sleep((frame_target_dt - emulation_dt) * wait_koef / 10);
            };
            // get exceed clocks and use them on next iteration
            let frame_dt = frame_start.elapsed();
            // change window header
            if self.enable_frame_trace {
                log::trace!(
                    "EMUALTOR: {:7.3}ms; FRAME:{:7.3}ms",
                    emulator_dt.as_millis(),
                    frame_dt.as_millis()
                );
            }
        }
        Ok(())
    }

    fn load_file_autodetect(&mut self, path: &Path) -> anyhow::Result<()> {
        match host::detect_file_type(path)? {
            DetectedFileKind::Snapshot => {
                self.emulator
                    .load_snapshot(host::load_snapshot(path)?)
                    .map_err(|e| {
                        anyhow!("Emulator failed to load auto-detected snapshot: {}", e)
                    })?;
            }
            DetectedFileKind::Tape => {
                self.emulator
                    .load_tape(host::load_tape(path)?)
                    .map_err(|e| anyhow!("Emulator failed to load auto-detected tape: {}", e))?;
            }
            DetectedFileKind::Screen => self
                .emulator
                .load_screen(host::load_screen(path)?)
                .map_err(|e| anyhow!("Emulator failed load screen via auto-detect: {}", e))?,
        }
        Ok(())
    }

    fn quick_save(&mut self) -> anyhow::Result<()> {
        let new_path = self.last_quick_snapshot_path();
        let prev_path = self.prev_quick_snapshot_path();

        if new_path.exists() {
            if prev_path.exists() {
                fs::remove_file(&prev_path)?;
            }
            fs::rename(&new_path, &prev_path)?;
        }

        let recorder = SnapshotRecorder::Sna(FileAsset::from(File::create(new_path)?));
        self.emulator
            .save_snapshot(recorder)
            .map_err(|e| anyhow!("Failed to save qick snapshot: {}", e))?;
        Ok(())
    }

    fn quick_load(&mut self) -> anyhow::Result<()> {
        let last_snapshot_path = self.last_quick_snapshot_path();
        if !last_snapshot_path.exists() {
            log::warn!("Quick snapshot was not found");
            return Ok(());
        }
        self.emulator
            .load_snapshot(host::load_snapshot(&last_snapshot_path)?)
            .map_err(|e| anyhow!("Emulator failed to load quick snapshot: {}", e))?;
        Ok(())
    }

    fn last_quick_snapshot_path(&self) -> PathBuf {
        if let Some(path) = self.settings.file_autodetect.as_ref() {
            return path.with_extension(".rustzx.last.sna");
        }
        Path::new("default.rustzx.last.sna").to_owned()
    }

    fn prev_quick_snapshot_path(&self) -> PathBuf {
        if let Some(path) = self.settings.file_autodetect.as_ref() {
            return path.with_extension(".rustzx.prev.sna");
        }
        Path::new("default.rustzx.prev.sna").to_owned()
    }
}

fn create_sound_backend(settings: &Settings) -> anyhow::Result<Box<dyn SoundDevice>> {
    use crate::app::sound;

    let backend: Box<dyn SoundDevice> = match settings.sound_backend {
        SoundBackend::Sdl => Box::new(sound::SoundSdl::new(settings)?),
        #[cfg(feature = "sound-cpal")]
        SoundBackend::Cpal => Box::new(sound::SoundCpal::new(settings)?),
    };
    Ok(backend)
}
