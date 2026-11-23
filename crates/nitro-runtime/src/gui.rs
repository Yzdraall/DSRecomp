use std::time::Instant;

use eframe::egui::{self, Color32, ColorImage, Key, RichText, TextureHandle, TextureOptions, Vec2};

use crate::ppu::{save_dual_screen_png, NDS_HEIGHT, NDS_WIDTH};
use crate::NitroMachine;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScreenLayout {
    Vertical,
    SideBySide,
}

pub fn run_native_window(machine: NitroMachine, title: String) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title.clone())
            .with_inner_size([860.0, 860.0])
            .with_min_inner_size([600.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        &title,
        options,
        Box::new(move |_cc| Ok(Box::new(DsNativeApp::new(machine)))),
    )
    .map_err(|e| e.to_string())
}

struct DsNativeApp {
    machine: NitroMachine,
    top_tex: Option<TextureHandle>,
    bot_tex: Option<TextureHandle>,
    paused: bool,
    layout: ScreenLayout,
    scale: f32,
    show_inspector: bool,
    last_fps_instant: Instant,
    frames_since_sample: u32,
    measured_fps: f32,
    status_toast: Option<String>,
}

impl DsNativeApp {
    fn new(machine: NitroMachine) -> Self {
        Self {
            machine,
            top_tex: None,
            bot_tex: None,
            paused: false,
            layout: ScreenLayout::Vertical,
            scale: 2.0,
            show_inspector: true,
            last_fps_instant: Instant::now(),
            frames_since_sample: 0,
            measured_fps: 60.0,
            status_toast: None,
        }
    }

    fn poll_keyboard(&mut self, ctx: &egui::Context) {
        let mut keyinput: u16 = 0x03FF;
        let mut extkeyin: u16 = 0x007F;

        ctx.input(|i| {
            // A = X / Space, B = Z / Backspace, Start = Enter, Select = RShift / Tab
            if i.key_down(Key::X) || i.key_down(Key::Space) {
                keyinput &= !(1 << 0);
            }
            if i.key_down(Key::Z) {
                keyinput &= !(1 << 1);
            }
            if i.key_down(Key::Tab) {
                keyinput &= !(1 << 2);
            }
            if i.key_down(Key::Enter) {
                keyinput &= !(1 << 3);
            }
            if i.key_down(Key::ArrowRight) || i.key_down(Key::D) {
                keyinput &= !(1 << 4);
            }
            if i.key_down(Key::ArrowLeft) || i.key_down(Key::A) {
                keyinput &= !(1 << 5);
            }
            if i.key_down(Key::ArrowUp) || i.key_down(Key::W) {
                keyinput &= !(1 << 6);
            }
            if i.key_down(Key::ArrowDown) || i.key_down(Key::S) {
                keyinput &= !(1 << 7);
            }
            if i.key_down(Key::E) {
                keyinput &= !(1 << 8); // R
            }
            if i.key_down(Key::Q) {
                keyinput &= !(1 << 9); // L
            }
            if i.key_down(Key::C) {
                extkeyin &= !(1 << 0); // X
            }
            if i.key_down(Key::V) {
                extkeyin &= !(1 << 1); // Y
            }
        });

        self.machine.bus.input.keyinput = keyinput;
        self.machine.bus.input.extkeyin = extkeyin;
    }
}

impl eframe::App for DsNativeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_keyboard(ctx);

        if !self.paused {
            self.machine.step_frame();
            self.frames_since_sample += 1;
            let elapsed = self.last_fps_instant.elapsed().as_secs_f32();
            if elapsed >= 0.5 {
                self.measured_fps = (self.frames_since_sample as f32) / elapsed;
                self.frames_since_sample = 0;
                self.last_fps_instant = Instant::now();
            }
        }

        let top_img = ColorImage::from_rgba_unmultiplied(
            [NDS_WIDTH, NDS_HEIGHT],
            &self.machine.frame.top_rgba,
        );
        let bot_img = ColorImage::from_rgba_unmultiplied(
            [NDS_WIDTH, NDS_HEIGHT],
            &self.machine.frame.bottom_rgba,
        );

        match &mut self.top_tex {
            Some(t) => t.set(top_img, TextureOptions::NEAREST),
            None => {
                self.top_tex = Some(ctx.load_texture("nds_top", top_img, TextureOptions::NEAREST));
            }
        }
        match &mut self.bot_tex {
            Some(t) => t.set(bot_img, TextureOptions::NEAREST),
            None => {
                self.bot_tex = Some(ctx.load_texture("nds_bot", bot_img, TextureOptions::NEAREST));
            }
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("DSRecomp")
                        .strong()
                        .color(Color32::from_rgb(86, 211, 100)),
                );
                ui.separator();
                if ui
                    .button(if self.paused {
                        "▶ Resume"
                    } else {
                        "⏸ Pause"
                    })
                    .clicked()
                {
                    self.paused = !self.paused;
                }
                if self.paused && ui.button("⏭ +1 Frame").clicked() {
                    self.machine.step_frame();
                }
                ui.separator();
                ui.selectable_value(&mut self.layout, ScreenLayout::Vertical, "Vertical DS");
                ui.selectable_value(&mut self.layout, ScreenLayout::SideBySide, "Side-by-Side");
                ui.separator();
                ui.selectable_value(&mut self.scale, 1.5, "1.5×");
                ui.selectable_value(&mut self.scale, 2.0, "2×");
                ui.selectable_value(&mut self.scale, 2.5, "2.5×");
                ui.separator();
                if ui.button("📸 Save PNG").clicked() {
                    let out = std::path::PathBuf::from(format!(
                        "screenshot_frame_{:05}.png",
                        self.machine.bus.frame_counter
                    ));
                    if save_dual_screen_png(&self.machine.frame, &out).is_ok() {
                        self.status_toast = Some(format!("Saved {}", out.display()));
                    }
                }
                ui.checkbox(&mut self.show_inspector, "AOT Inspector");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!(
                        "{:.1} FPS • Frame {}",
                        self.measured_fps, self.machine.bus.frame_counter
                    ));
                });
            });
        });

        if self.show_inspector {
            egui::SidePanel::right("aot_inspector")
                .default_width(250.0)
                .show(ctx, |ui| {
                    ui.heading("ARM9 AOT Telemetry");
                    ui.label(format!(
                        "ROM: {} ({})",
                        self.machine.bus.rom.header.game_title,
                        self.machine.bus.rom.header.game_code
                    ));
                    ui.label(format!(
                        "Active Overlay: {}",
                        match self.machine.bus.active_arm9_overlay() {
                            Some(id) => format!("ov_{id}"),
                            None => "None (static_arm9)".to_string(),
                        }
                    ));
                    let aot = self.machine.cpu.aot_blocks_executed;
                    let fb = self.machine.cpu.fallback_steps_executed;
                    ui.label(format!("AOT Blocks Run: {aot}"));
                    ui.label(format!("Dynamic Steps:  {fb}"));
                    ui.label(format!(
                        "Stylus: ({}, {}) {}",
                        self.machine.bus.input.stylus_x,
                        self.machine.bus.input.stylus_y,
                        if self.machine.bus.input.stylus_down {
                            "[TOUCH]"
                        } else {
                            ""
                        }
                    ));
                    ui.separator();
                    ui.label(RichText::new("ARM9 Registers").strong());
                    egui::Grid::new("reg_grid").striped(true).show(ui, |ui| {
                        for r in (0..16usize).step_by(2) {
                            ui.monospace(format!("r{r:<2}: {:08X}", self.machine.cpu.regs[r]));
                            ui.monospace(format!(
                                "r{:<2}: {:08X}",
                                r + 1,
                                self.machine.cpu.regs[r + 1]
                            ));
                            ui.end_row();
                        }
                    });
                    ui.separator();
                    ui.monospace(format!(
                        "Flags: {}{}{}{} | {}",
                        if self.machine.cpu.n { 'N' } else { '-' },
                        if self.machine.cpu.z { 'Z' } else { '-' },
                        if self.machine.cpu.c { 'C' } else { '-' },
                        if self.machine.cpu.v { 'V' } else { '-' },
                        if self.machine.cpu.thumb {
                            "THUMB"
                        } else {
                            "ARM32"
                        }
                    ));
                    if let Some(ref toast) = self.status_toast {
                        ui.separator();
                        ui.colored_label(Color32::LIGHT_GREEN, toast);
                    }
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let screen_size = Vec2::new((NDS_WIDTH as f32) * self.scale, (NDS_HEIGHT as f32) * self.scale);
            let top_id = self.top_tex.as_ref().unwrap().id();
            let bot_id = self.bot_tex.as_ref().unwrap().id();

            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                match self.layout {
                    ScreenLayout::Vertical => {
                        ui.image((top_id, screen_size));
                        ui.add_space(10.0);
                        let resp = ui.add(
                            egui::Image::new((bot_id, screen_size))
                                .sense(egui::Sense::click_and_drag()),
                        );
                        self.update_stylus_from_response(&resp);
                    }
                    ScreenLayout::SideBySide => {
                        ui.horizontal(|ui| {
                            ui.image((top_id, screen_size));
                            ui.add_space(12.0);
                            let resp = ui.add(
                                egui::Image::new((bot_id, screen_size))
                                    .sense(egui::Sense::click_and_drag()),
                            );
                            self.update_stylus_from_response(&resp);
                        });
                    }
                }
                ui.add_space(6.0);
                ui.small("Controls: Arrows/WASD = D-Pad • X/Space = A • Z = B • Enter = Start • Mouse on Bottom Screen = Touch Stylus");
            });
        });

        ctx.request_repaint();
    }
}

impl DsNativeApp {
    fn update_stylus_from_response(&mut self, resp: &egui::Response) {
        if resp.is_pointer_button_down_on() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let rect = resp.rect;
                let nx = ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 0.999);
                let ny = ((pos.y - rect.min.y) / rect.height()).clamp(0.0, 0.999);
                self.machine.bus.input.stylus_x = (nx * 256.0) as u8;
                self.machine.bus.input.stylus_y = (ny * 192.0) as u8;
                self.machine.bus.input.stylus_down = true;
                return;
            }
        }
        self.machine.bus.input.stylus_down = false;
    }
}
