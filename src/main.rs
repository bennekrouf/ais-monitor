mod screens;
mod components;
mod services;
mod update_check;

use dioxus::prelude::*;
use dioxus::desktop::LogicalSize;
use components::chain_detail::AzConfig;
use screens::{welcome::Welcome, main_screen::MainScreen};

const MAIN_CSS: &str = include_str!("../assets/main.css");

fn main() {
    // Suppress noisy debug logs from hyper/reqwest unless the user overrides RUST_LOG
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info,hyper_util=warn,hyper=warn,reqwest=warn");
    }

    let webview_data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("AIS Monitor");

    let cfg = dioxus::desktop::Config::new()
        .with_data_directory(webview_data_dir)
        .with_window(
            dioxus::desktop::WindowBuilder::new()
                .with_title(concat!("AIS Monitor ", env!("CARGO_PKG_VERSION")))
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

            let t = (x + y) as f32 / (SIZE * 2) as f32;
            let r = (0.0_f32 + t * 20.0) as u8;
            let g = (100.0 + t * 20.0) as u8;
            let b = (200.0 - t * 30.0) as u8;

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

    // Shield outline: rounded top, pointed bottom
    let scx = s * 0.50;
    let top = s * 0.18;
    let bot = s * 0.82;
    let w = s * 0.32;
    let mid_y = s * 0.55;
    let inside = if fy < top || fy > bot {
        false
    } else if fy <= mid_y {
        (fx - scx).abs() < w && (fx - scx).abs() > w - s * 0.04
    } else {
        let frac = (fy - mid_y) / (bot - mid_y);
        let cur_w = w * (1.0 - frac);
        cur_w > s * 0.02 && (fx - scx).abs() < cur_w && (fx - scx).abs() > cur_w - s * 0.04
    };
    if inside { return true; }
    // Shield top arc
    if fy >= top - s * 0.02 && fy <= top + s * 0.02 && (fx - scx).abs() < w {
        return true;
    }

    // Heartbeat / pulse line across the shield center
    let pulse_y = s * 0.48;
    let thick = s * 0.025;
    if fy > pulse_y - s * 0.18 && fy < pulse_y + s * 0.18 && fx > scx - w + s * 0.06 && fx < scx + w - s * 0.06 {
        let left = scx - w + s * 0.06;
        let right = scx + w - s * 0.06;
        let span = right - left;
        let t = (fx - left) / span;

        let py = if t < 0.30 {
            pulse_y
        } else if t < 0.40 {
            let u = (t - 0.30) / 0.10;
            pulse_y - s * 0.15 * u
        } else if t < 0.50 {
            let u = (t - 0.40) / 0.10;
            pulse_y - s * 0.15 * (1.0 - u) + s * 0.12 * u
        } else if t < 0.58 {
            let u = (t - 0.50) / 0.08;
            pulse_y + s * 0.12 * (1.0 - u)
        } else {
            pulse_y
        };

        if (fy - py).abs() < thick { return true; }
    }

    false
}

#[component]
fn App() -> Element {
    let mut az_config = use_signal(|| Option::<AzConfig>::None);

    // ── System theme — applies to both Welcome and MainScreen ────────────
    // Start with the OS preference; stop syncing once the user toggles manually.
    let system_light = dark_light::detect() != dark_light::Mode::Dark;
    let mut is_light = use_signal(|| system_light);
    let theme_overridden = use_signal(|| false);

    // Inject CSS once at startup
    use_effect(move || {
        let css = MAIN_CSS.replace('`', "\\`").replace("${", "\\${");
        document::eval(&format!(
            "if(!document.getElementById('ais-css')){{var s=document.createElement('style');s.id='ais-css';s.textContent=`{}`;document.head.appendChild(s);}}",
            css
        ));
    });

    // Apply CSS class whenever theme changes
    use_effect(move || {
        let cls = if *is_light.read() { "light" } else { "" };
        document::eval(&format!("document.body.className = '{}';", cls));
    });

    // Poll system preference every 2 s — skip when user has overridden
    use_coroutine(move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
            if *theme_overridden.read() { continue; }
            let system_light = tokio::task::spawn_blocking(|| {
                dark_light::detect() != dark_light::Mode::Dark
            }).await.unwrap_or(*is_light.read());
            if system_light != *is_light.read() {
                is_light.set(system_light);
            }
        }
    });

    let config_val = az_config.read().clone();

    // ── Auto-update check ──────────────────────────────────────────────────
    let mut update_info      = use_signal(|| Option::<update_check::UpdateInfo>::None);
    let mut update_dismissed = use_signal(|| false);
    use_coroutine(move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if let Some(info) = update_check::check().await {
            update_info.set(Some(info));
        }
    });

    rsx! {
        if let (Some(info), false) = (update_info.read().clone(), *update_dismissed.read()) {
            div { class: "update-banner",
                span { class: "update-banner-text",
                    "ais-monitor "
                    strong { "{info.latest_version}" }
                    " is available (you have {env!(\"CARGO_PKG_VERSION\")})."
                }
                a {
                    class: "update-banner-link",
                    href: "{info.release_url}",
                    target: "_blank",
                    "Download"
                }
                button {
                    class: "update-banner-dismiss",
                    onclick: move |_| update_dismissed.set(true),
                    "×"
                }
            }
        }
        match config_val {
            None => rsx! {
                Welcome {
                    on_connect: move |config: AzConfig| {
                        // Reflect the active customer profile in the window title.
                        let tag = if !config.label.is_empty() {
                            config.label.clone()
                        } else {
                            config.app_name.clone()
                        };
                        dioxus::desktop::window().set_title(&format!(
                            "AIS Monitor {} — {}",
                            env!("CARGO_PKG_VERSION"),
                            tag,
                        ));
                        az_config.set(Some(config));
                    },
                }
            },
            Some(config) => rsx! {
                MainScreen {
                    az_config: config,
                    is_light,
                    theme_overridden,
                    on_back: move |_| {
                        dioxus::desktop::window().set_title(
                            concat!("AIS Monitor ", env!("CARGO_PKG_VERSION"))
                        );
                        az_config.set(None);
                    },
                }
            },
        }
    }
}
