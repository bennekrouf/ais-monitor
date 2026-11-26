//! How to use AIS Monitor from Claude or Codex: what to paste, and where.
//!
//! The server is this executable started as `ais-monitor mcp`. desktop-kit
//! works out the command from where the app runs, so the snippets are right
//! wherever it was installed. Its own connect window needs a newer Dioxus than
//! this app runs; the strings are the same.

use desktop_kit::mcp::Connect;
use dioxus::prelude::*;

/// The MCP button, and the window it opens.
#[component]
pub fn McpButton(class: String) -> Element {
    let mut open = use_signal(|| false);
    rsx! {
        button {
            class: "{class}",
            title: "Use AIS Monitor from Claude Desktop, Claude Code or Codex",
            onclick: move |_| open.set(true),
            "MCP"
        }
        if open() {
            McpDialog { on_close: move |_| open.set(false) }
        }
    }
}

#[component]
fn McpDialog(on_close: EventHandler<()>) -> Element {
    let connect = use_hook(|| Connect::new("ais-monitor"));
    rsx! {
        div { class: "mcp-backdrop", onclick: move |_| on_close.call(()) }
        div { class: "mcp-dialog",
            div { class: "mcp-dialog-header",
                div {
                    h3 { "Use AIS Monitor from Claude or Codex" }
                    p { class: "mcp-hint",
                        "Ask Claude about your Logic Apps in chat: which chain a workflow belongs to, why its \
                         last runs failed, what is sitting in a dead-letter queue, which settings drifted. It \
                         reads only: it never starts or stops an app, triggers a workflow, touches a message \
                         or changes a setting."
                    }
                }
                button { class: "btn btn-small", title: "Close", onclick: move |_| on_close.call(()), "✕" }
            }
            div { class: "mcp-dialog-body",
                p { class: "mcp-label", "1 · Claude Desktop" }
                p { class: "mcp-hint",
                    "Settings → Developer → Edit Config, paste this in and save, then quit Claude and open it \
                     again. If the file already has an mcpServers section, add just the ais-monitor entry to it."
                }
                Snippet { text: connect.desktop.clone() }
                p { class: "mcp-hint mcp-path", "The file: {connect.config_path}" }
                p { class: "mcp-label", "2 · Claude Code" }
                p { class: "mcp-hint", "Run this once in a terminal:" }
                Snippet { text: connect.code.clone() }
                p { class: "mcp-label", "3 · Codex" }
                p { class: "mcp-hint", "Run this once in a terminal:" }
                Snippet { text: connect.codex.clone() }
                p { class: "mcp-label", "4 · Try it" }
                p { class: "mcp-hint",
                    "Sign in with az login as usual, then ask: “Which workflows of my dev Logic App failed \
                     today, and why?” Claude works on the connections saved here."
                }
                p { class: "mcp-hint mcp-note",
                    "Claude reads Azure as your signed-in az account, and what it reads goes to the AI model \
                     you use with it. Claude in the browser (claude.ai) can’t use this: it only connects to \
                     servers on the internet, and AIS Monitor runs on your computer."
                }
            }
        }
    }
}

/// A block of text to paste somewhere else, with its own Copy button.
#[component]
fn Snippet(text: String) -> Element {
    let mut copied = use_signal(|| false);
    let to_copy = text.clone();
    rsx! {
        div { class: "mcp-snippet-wrap",
            pre { class: "mcp-snippet", "{text}" }
            button {
                class: "btn btn-small mcp-copy",
                onclick: move |_| {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        copied.set(cb.set_text(to_copy.clone()).is_ok());
                    }
                },
                if copied() { "Copied ✓" } else { "Copy" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_snippets_start_this_app_as_an_mcp_server() {
        let c = Connect::at(
            "ais-monitor",
            "/Applications/AIS Monitor.app/Contents/MacOS/ais-monitor",
        );
        assert!(c.desktop.contains("\"mcp\""));
        assert_eq!(
            c.code,
            "claude mcp add ais-monitor --scope user -- \"/Applications/AIS Monitor.app/Contents/MacOS/ais-monitor\" mcp"
        );
    }
}
