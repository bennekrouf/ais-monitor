mod screens;
mod components;
mod services;

use dioxus::prelude::*;
use dioxus::desktop::LogicalSize;
use screens::{welcome::Welcome, main_screen::MainScreen};

const MAIN_CSS: &str = include_str!("../assets/main.css");

fn main() {
    let webview_data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("AIS Monitor");

    let cfg = dioxus::desktop::Config::new()
        .with_data_directory(webview_data_dir)
        .with_window(
            dioxus::desktop::WindowBuilder::new()
                .with_title("AIS Monitor")
                .with_inner_size(LogicalSize::new(1280.0, 820.0))
                .with_always_on_top(false)
                .with_window_icon(make_icon()),
        );
    LaunchBuilder::desktop().with_cfg(cfg).launch(App);
}

fn make_icon() -> Option<dioxus::desktop::tao::window::Icon> {
    const SIZE: u32 = 64;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let cx = x as f32 - SIZE as f32 / 2.0 + 0.5;
            let cy = y as f32 - SIZE as f32 / 2.0 + 0.5;
            let r_sq = cx * cx + cy * cy;
            let radius = SIZE as f32 / 2.0;
            let alpha = if r_sq > (radius * radius) { 0u8 } else { 255u8 };

            // Teal/green gradient for monitor (distinct from ais-runner blue)
            let t = (x + y) as f32 / (SIZE * 2) as f32;
            let r = (0.0_f32 + t * 15.0) as u8;
            let g = (160.0 - t * 30.0) as u8;
            let b = (120.0 - t * 20.0) as u8;

            // White "eye" / monitor shape
            let in_shape = is_monitor_shape(x, y, SIZE);
            let (r, g, b) = if in_shape { (255u8, 255u8, 255u8) } else { (r, g, b) };

            rgba.extend_from_slice(&[r, g, b, alpha]);
        }
    }
    dioxus::desktop::tao::window::Icon::from_rgba(rgba, SIZE, SIZE).ok()
}

fn is_monitor_shape(x: u32, y: u32, size: u32) -> bool {
    let s = size as f32;
    let fx = x as f32;
    let fy = y as f32;

    // Central "eye" circle (monitoring)
    let cx = s * 0.50;
    let cy = s * 0.45;
    if (fx - cx).hypot(fy - cy) < s * 0.15 { return true; }

    // Outer ring
    let dist = (fx - cx).hypot(fy - cy);
    if dist > s * 0.25 && dist < s * 0.30 { return true; }

    // Three small dots at bottom (chain links)
    for i in 0..3 {
        let dx = s * (0.30 + i as f32 * 0.20);
        let dy = s * 0.78;
        if (fx - dx).hypot(fy - dy) < s * 0.06 { return true; }
    }

    false
}

#[component]
fn App() -> Element {
    let mut selected_dir = use_signal(|| Option::<String>::None);
    let dir_val = selected_dir.read().clone();

    match dir_val {
        None => rsx! {
            document::Style { "{MAIN_CSS}" }
            Welcome {
                on_select: move |dir: String| selected_dir.set(Some(dir)),
            }
        },
        Some(dir) => rsx! {
            document::Style { "{MAIN_CSS}" }
            MainScreen {
                logic_apps_dir: dir.clone(),
                on_back: move |_| selected_dir.set(None),
            }
        },
    }
}
