mod components;
mod hooks;
mod kit;
mod mcp;
mod notice;
mod screens;
mod services;
mod telemetry;

use components::chain_detail::AzConfig;
use desktop_kit::self_update::{self, Stage};
use desktop_kit::update_check::UpdateInfo;
use dioxus::desktop::LogicalSize;
use dioxus::prelude::*;
use screens::{main_screen::MainScreen, welcome::Welcome};

const MAIN_CSS: &str = include_str!("../assets/main.css");

fn webview_data_dir() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("AIS Monitor")
}

fn window_config(title: &str) -> dioxus::desktop::Config {
    dioxus::desktop::Config::new()
        .with_data_directory(webview_data_dir())
        .with_window(
            dioxus::desktop::WindowBuilder::new()
                .with_title(title)
                .with_inner_size(LogicalSize::new(1280.0, 820.0))
                .with_always_on_top(false)
                .with_window_icon(make_icon()),
        )
}

/// Opens another window connected to `config`, in this same process.
///
/// One process, many windows — not many processes. Two copies of the binary
/// would race each other on on-disk profiles/caches. Windows inside one
/// process share none of that: each gets its own VirtualDom and its own
/// signals, and the OS sees a single app.
pub fn open_in_new_window(config: AzConfig) {
    let tag = if !config.label.is_empty() {
        config.label.clone()
    } else {
        config.app_name.clone()
    };
    let dom = VirtualDom::new_with_props(
        WindowRoot,
        WindowRootProps {
            initial: Some(config),
        },
    );
    dioxus::desktop::window().new_window(
        dom,
        window_config(&format!(
            "AIS Monitor {} — {}",
            env!("CARGO_PKG_VERSION"),
            tag
        )),
    );
}

fn main() {
    // Before anything can shell out: an app launched from Finder or a .dmg,
    // or by an MCP client, does not inherit the terminal's PATH, so `az`
    // reads as "not found".
    services::env::adopt_login_path();

    // Names the app to desktop-kit before its update check can run.
    kit::init();
    // `ais-monitor mcp`: the tools over stdio for an MCP client, and no window.
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        return mcp::serve();
    }

    // Suppress noisy debug logs from hyper/reqwest unless the user overrides RUST_LOG
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info,hyper_util=warn,hyper=warn,reqwest=warn");
    }

    let cfg = window_config(concat!("AIS Monitor ", env!("CARGO_PKG_VERSION")));
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
            let (r, g, b) = if in_shape {
                (255u8, 255u8, 255u8)
            } else {
                (r, g, b)
            };

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
    if inside {
        return true;
    }
    // Shield top arc
    if fy >= top - s * 0.02 && fy <= top + s * 0.02 && (fx - scx).abs() < w {
        return true;
    }

    // Heartbeat / pulse line across the shield center
    let pulse_y = s * 0.48;
    let thick = s * 0.025;
    if fy > pulse_y - s * 0.18
        && fy < pulse_y + s * 0.18
        && fx > scx - w + s * 0.06
        && fx < scx + w - s * 0.06
    {
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

        if (fy - py).abs() < thick {
            return true;
        }
    }

    false
}

#[component]
fn App() -> Element {
    rsx! { WindowRoot { initial: Option::<AzConfig>::None } }
}

/// One window. Each has its own VirtualDom, so its own signals, its own theme
/// state and its own connected profile — and its own welcome screen to go
/// back to.
#[component]
fn WindowRoot(initial: Option<AzConfig>) -> Element {
    let mut az_config = use_signal(|| initial);

    // ── System theme — applies to both Welcome and MainScreen ────────────
    // Start with the OS preference; stop syncing once the user toggles
    // manually. Inside the initialiser so the platform call happens once per
    // window rather than on every render — the polling coroutine below is
    // what keeps it current.
    let mut is_light = use_signal(|| dark_light::detect() != dark_light::Mode::Dark);
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
    use_coroutine(
        move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
                if *theme_overridden.read() {
                    continue;
                }
                let system_light =
                    tokio::task::spawn_blocking(|| dark_light::detect() != dark_light::Mode::Dark)
                        .await
                        .unwrap_or(*is_light.read());
                if system_light != *is_light.read() {
                    is_light.set(system_light);
                }
            }
        },
    );

    let config_val = az_config.read().clone();

    // ── Auto-update check ──────────────────────────────────────────────────
    // ── Notice from mayorana.ch ────────────────────────────────────────────
    // A message to the people running this build (see notice.rs). Same
    // posture as the update check: delayed, best-effort, silent on failure.
    let mut mayorana_notice = use_signal(|| Option::<notice::Notice>::None);
    use_coroutine(
        move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            if let Some(n) = notice::fetch().await {
                mayorana_notice.set(Some(n));
            }
        },
    );

    let mut update_info = use_signal(|| Option::<UpdateInfo>::None);
    let mut update_dismissed = use_signal(|| false);
    // Whether this copy can put the new build in place itself (an installed
    // app, not a `cargo` build), and how far an install has got. A failure
    // keeps the banner with the reason and the button to try again. Starts
    // with what the last attempt left, so an app that quit to update and came
    // back on the old version says why.
    let mut update_installable = use_signal(|| false);
    let mut update_stage = use_signal(|| Option::<Stage>::None);
    let mut update_failed = use_signal(self_update::last_failure);
    use_coroutine(
        move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if let Some(info) = desktop_kit::update_check::check().await {
                telemetry::record(telemetry::Event::UpdateOffered {
                    to: info.latest_version.clone(),
                });
                update_installable.set(self_update::installable(&info));
                update_info.set(Some(info));
            }
        },
    );
    // Downloads, checks the digest and signature, and prepares the swap off
    // the UI thread, showing how far it got, then quits: what desktop-kit
    // started beside the app puts the new version in place and opens it.
    let mut install_update = move |info: UpdateInfo| {
        telemetry::record(telemetry::Event::UpdateClicked {
            to: info.latest_version.clone(),
        });
        update_failed.set(None);
        update_stage.set(Some(Stage::Downloading(0)));
        spawn(async move {
            let (tx, rx) = tokio::sync::watch::channel(Stage::Downloading(0));
            let job = tokio::task::spawn_blocking(move || {
                self_update::install(&info, &|stage| {
                    let _ = tx.send(stage);
                })
            });
            while !job.is_finished() {
                let stage = rx.borrow().clone();
                update_stage.set(Some(stage));
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            match job.await {
                Ok(Ok(())) => std::process::exit(0),
                Ok(Err(e)) => update_failed.set(Some(e)),
                Err(e) => update_failed.set(Some(e.to_string())),
            }
            update_stage.set(None);
        });
    };

    // ── Anonymous usage statistics ─────────────────────────────────────────
    // On by default, but only once the person has been told: `start` reads the
    // opt-outs (including a shell profile's), records the launch, and says
    // whether the notice is still owed. See telemetry.rs.
    let mut ask_consent = use_signal(|| false);
    use_coroutine(
        move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
            if telemetry::start().await {
                ask_consent.set(true);
            }
            telemetry::flush_forever().await;
        },
    );
    // Recorded while the notice is on screen, so collection starts from the
    // next event — never from one the person had no chance to read about.
    use_effect(move || {
        if *ask_consent.read() {
            telemetry::mark_informed();
        }
    });

    rsx! {
        if let (Some(info), false) = (update_info.read().clone(), *update_dismissed.read()) {
            div { class: "update-banner",
                span { class: "update-banner-text",
                    match (update_stage(), update_failed()) {
                        (Some(Stage::Downloading(bytes)), _) => rsx! {
                            "Downloading AIS Monitor "
                            strong { "{info.latest_version}" }
                            "… {bytes as f64 / 1_000_000.0:.1} MB"
                        },
                        (Some(Stage::Checking), _) => rsx! {
                            "Checking the download of AIS Monitor "
                            strong { "{info.latest_version}" }
                            "…"
                        },
                        (Some(Stage::Installing), _) => rsx! {
                            "Installing AIS Monitor "
                            strong { "{info.latest_version}" }
                            ": it restarts in a moment."
                        },
                        (None, Some(e)) => rsx! {
                            "Couldn't install AIS Monitor "
                            strong { "{info.latest_version}" }
                            ": {e}."
                        },
                        (None, None) => rsx! {
                            "AIS Monitor "
                            strong { "{info.latest_version}" }
                            " is available (you have {env!(\"CARGO_PKG_VERSION\")})."
                        },
                    }
                }
                if update_stage().is_none() {
                    if desktop_kit::policy::get().disable_self_update {
                        span { class: "update-banner-sha", "Your organization installs updates." }
                    } else if update_installable() {
                        button {
                            class: "update-banner-link",
                            title: "Download {info.latest_version}, check it, install it in place of this version and restart AIS Monitor",
                            onclick: {
                                let info = info.clone();
                                move |_| install_update(info.clone())
                            },
                            "Install & restart"
                        }
                    } else {
                        {
                            // Deliberately a button, not an `<a href>`. This
                            // URL comes from a remote `latest.json`, and a
                            // bare anchor in a desktop webview is one
                            // interception rule away from navigating the
                            // *app's own* webview to it. Routing through
                            // `open_in_browser` also runs its scheme check.
                            let url = info.download_url.clone();
                            let to = info.latest_version.clone();
                            rsx! {
                                button {
                                    class: "update-banner-link",
                                    onclick: move |_| {
                                        telemetry::record(telemetry::Event::UpdateClicked { to: to.clone() });
                                        services::portal_links::open_in_browser(&url)
                                    },
                                    "Download"
                                }
                            }
                        }
                        if !info.sha256.is_empty() {
                            span {
                                class: "update-banner-sha",
                                title: "SHA-256 of the published build — compare it against the file you downloaded",
                                "sha256 {services::text::head(&info.sha256, 12)}"
                            }
                        }
                    }
                    button {
                        class: "update-banner-dismiss",
                        onclick: move |_| update_dismissed.set(true),
                        "×"
                    }
                }
            }
        }

        // Usage-statistics notice: once, at the bottom so it never sits under the
        // update or notice banners. Either button is remembered.
        if *ask_consent.read() {
            div { class: "consent-banner",
                span { class: "update-banner-text",
                    strong { "AIS Monitor shares anonymous usage statistics. " }
                    "Whether it is installed and opened, and its version and operating system \
                     — never your files, data, accounts or anything you type."
                }
                button {
                    class: "update-banner-link",
                    onclick: move |_| {
                        telemetry::set_consent(true);
                        ask_consent.set(false);
                    },
                    "OK"
                }
                button {
                    class: "update-banner-link",
                    onclick: move |_| {
                        telemetry::set_consent(false);
                        ask_consent.set(false);
                    },
                    "Turn off"
                }
            }
        }

        // Notice from mayorana.ch — shown until dismissed, then remembered.
        if let Some(n) = mayorana_notice.read().clone() {
            {
                let id = n.id.clone();
                let link_text = n.link_text.clone().unwrap_or_else(|| "Open".to_string());
                rsx! {
                    div { class: "update-banner notice-banner",
                        span { class: "update-banner-text", "{n.text}" }
                        if let Some(url) = n.url.clone() {
                            a {
                                class: "update-banner-link",
                                href: "{url}",
                                target: "_blank",
                                "{link_text}"
                            }
                        }
                        button {
                            class: "update-banner-dismiss",
                            onclick: move |_| {
                                notice::dismiss(&id);
                                mayorana_notice.set(None);
                            },
                            "×"
                        }
                    }
                }
            }
        }
        match config_val {
            None => rsx! {
                Welcome {
                    // Connecting always opens a new window and leaves this one
                    // on the welcome screen — it's a launcher, not a profile
                    // you leave.
                    on_connect: move |config: AzConfig| {
                        crate::open_in_new_window(config);
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
