use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct WelcomeProps {
    pub on_select: EventHandler<String>,
}

#[component]
pub fn Welcome(props: WelcomeProps) -> Element {
    let mut recent_dirs = use_signal(|| load_recent_dirs());

    rsx! {
        div { class: "welcome",
            div { class: "welcome-card",
                h1 { "AIS Monitor" }
                p { class: "subtitle", "Azure Logic Apps — Chain Discovery & Monitoring" }

                button {
                    class: "btn btn-primary",
                    onclick: {
                        let on_select = props.on_select.clone();
                        move |_| {
                            if let Some(dir) = rfd::FileDialog::new()
                                .set_title("Select Logic Apps project folder")
                                .pick_folder()
                            {
                                let path = dir.to_string_lossy().to_string();
                                save_recent_dir(&path);
                                recent_dirs.set(load_recent_dirs());
                                on_select.call(path);
                            }
                        }
                    },
                    "Open Folder…"
                }

                if !recent_dirs.read().is_empty() {
                    div { class: "recent-section",
                        h3 { "Recent" }
                        for dir in recent_dirs.read().iter() {
                            {
                                let d = dir.clone();
                                let d2 = dir.clone();
                                let on_select = props.on_select.clone();
                                rsx! {
                                    button {
                                        class: "btn btn-recent",
                                        title: "{d}",
                                        onclick: move |_| on_select.call(d.clone()),
                                        "{d2}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn recent_file() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ais-monitor")
        .join("recent.txt")
}

fn load_recent_dirs() -> Vec<String> {
    let path = recent_file();
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(String::from)
        .collect()
}

fn save_recent_dir(dir: &str) {
    let path = recent_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut dirs = load_recent_dirs();
    dirs.retain(|d| d != dir);
    dirs.insert(0, dir.to_string());
    dirs.truncate(5);
    let _ = std::fs::write(path, dirs.join("\n"));
}
