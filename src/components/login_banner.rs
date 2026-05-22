use dioxus::prelude::*;
use crate::services::azure::AzLoginState;

#[derive(Props, Clone, PartialEq)]
pub struct LoginBannerProps {
    pub state: AzLoginState,
    pub on_login: EventHandler<()>,
    pub on_refresh: EventHandler<()>,
}

#[component]
pub fn LoginBanner(props: LoginBannerProps) -> Element {
    match &props.state {
        AzLoginState::Unknown | AzLoginState::Checking => rsx! {
            div { class: "login-banner checking",
                span { class: "dot pulse" }
                span { "Checking Azure login…" }
            }
        },
        AzLoginState::LoggedIn { account, .. } => rsx! {
            div { class: "login-banner ok",
                span { class: "dot ok" }
                span { class: "account-name", "{account}" }
                button {
                    class: "btn btn-small",
                    onclick: move |_| props.on_refresh.call(()),
                    "⟳"
                }
            }
        },
        AzLoginState::Expired => rsx! {
            div { class: "login-banner warn",
                span { class: "dot warn" }
                span { "Azure token expired" }
                button {
                    class: "btn btn-primary btn-small",
                    onclick: move |_| props.on_login.call(()),
                    "Re-login"
                }
            }
        },
        AzLoginState::NotLoggedIn => rsx! {
            div { class: "login-banner error",
                span { class: "dot error" }
                span { "Not signed in to Azure" }
                button {
                    class: "btn btn-primary btn-small",
                    onclick: move |_| props.on_login.call(()),
                    "Sign in"
                }
            }
        },
    }
}
