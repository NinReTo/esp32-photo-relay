//! esp32-photo-relay — оконное приложение для ПК.
//!
//! Показывает, подключена ли плата, настраивает её Wi-Fi по USB,
//! принимает фото и показывает телефоны, которые ими пользовались.

// В готовой сборке для Windows не открывать рядом с окном чёрную консоль.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod board;
mod filename;
mod netinfo;
mod protocol;
mod receiver;
mod ui;

use eframe::egui;

const WINDOW_TITLE: &str = "ESP32 Photo Relay";
const WINDOW_SIZE: [f32; 2] = [940.0, 660.0];
const WINDOW_MIN_SIZE: [f32; 2] = [720.0, 480.0];

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size(WINDOW_MIN_SIZE),
        ..Default::default()
    };

    eframe::run_native(
        WINDOW_TITLE,
        options,
        Box::new(|creation| Ok(Box::new(app::App::new(&creation.egui_ctx)))),
    )
}
