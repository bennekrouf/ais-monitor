use dioxus::prelude::*;
use crate::services::azure::{self, AzLoginState, AzSubscription};
use crate::components::chain_detail::AzConfig;

#[derive(Props, Clone, PartialEq)]
pub struct WelcomeProps {
    pub on_connect: EventHandler<AzConfig>,
}

#[component]
pub fn Welcome(props: WelcomeProps) -> Element {
    let mut az_state = use_signal(|| AzLoginState::Checking);
    let mut sub_id = use_signal(|| String::new());

    // Form fields (shared between browse and manual modes)
    let mut label_input = use_signal(|| String::new());
    let mut tenant_input = use_signal(|| String::new());
    let mut rg_input = use_signal(|| String::new());
    let mut app_input = use_signal(|| String::new());
    let mut sb_input = use_signal(|| String::new());
    let mut error_msg = use_signal(|| Option::<String>::None);
    let mut show_form = use_signal(|| false);
    let mut editing_profile = use_signal(|| Option::<usize>::None);

    // Browse mode state
    let mut subscriptions = use_signal(|| Vec::<AzSubscription>::new());
    let mut selected_sub = use_signal(|| String::new());
    let mut resource_groups = use_signal(|| Vec::<String>::new());
    let mut logic_apps = use_signal(|| Vec::<String>::new());
    let mut sb_namespaces = use_signal(|| Vec::<String>::new());
    // "" | "subs" | "rgs" | "apps"
    let mut browse_loading = use_signal(|| String::new());

    let mut profiles = use_signal(|| load_profiles());

    use_effect(move || {
        spawn(async move {
            let state = tokio::task::spawn_blocking(azure::check_login)
                .await
                .unwrap_or(AzLoginState::NotLoggedIn);
            if let AzLoginState::LoggedIn { ref subscription_id, .. } = state {
                sub_id.set(subscription_id.clone());
                // Auto-open profile creation when logged in with no saved profiles
                if profiles.read().is_empty() {
                    show_form.set(true);
                    // Also kick off subscription loading for browse mode
                    browse_loading.set("subs".into());
                    let subs = tokio::task::spawn_blocking(azure::list_subscriptions)
                        .await
                        .unwrap_or(Ok(vec![]))
                        .unwrap_or_default();
                    if subs.len() == 1 {
                        let sid = subs[0].id.clone();
                        selected_sub.set(sid.clone());
                        subscriptions.set(subs);
                        browse_loading.set("rgs".into());
                        let rgs = tokio::task::spawn_blocking(move || azure::list_resource_groups(&sid))
                            .await.unwrap_or(Ok(vec![])).unwrap_or_default();
                        resource_groups.set(rgs);
                    } else {
                        subscriptions.set(subs);
                    }
                    browse_loading.set(String::new());
                }
            }
            az_state.set(state);
        });
    });

    let is_logged_in = matches!(*az_state.read(), AzLoginState::LoggedIn { .. });
    // Browse mode: new profile form while logged in
    let is_browse_mode = *show_form.read() && editing_profile.read().is_none() && is_logged_in;
    let can_connect = !rg_input.read().is_empty() && !app_input.read().is_empty();

    rsx! {
        div { class: "welcome",
            div { class: "welcome-card",
                h1 { "AIS Monitor" }
                p { class: "subtitle", "Azure Logic Apps — Production Chain Monitoring" }

                div { class: "welcome-box",
                    // Azure login status
                    div { class: "welcome-pick",
                        {
                            let state = az_state.read().clone();
                            match state {
                                AzLoginState::Checking => rsx! {
                                    div { class: "az-status",
                                        span { class: "dot pulse" }
                                        span { "Checking Azure login..." }
                                    }
                                },
                                AzLoginState::LoggedIn { ref account, .. } => rsx! {
                                    div { class: "az-status",
                                        span { class: "dot ok" }
                                        span { "Connected: {account}" }
                                    }
                                    if profiles.read().is_empty() {
                                        p { style: "margin:8px 0 0; font-size:12px; opacity:0.65;",
                                            "Select a Logic App below to start monitoring."
                                        }
                                    }
                                },
                                AzLoginState::Expired | AzLoginState::NotLoggedIn => {
                                    let msg = if matches!(state, AzLoginState::Expired) { "Session expired" } else { "Not logged in" };
                                    let dot = if matches!(state, AzLoginState::Expired) { "dot warn" } else { "dot error" };
                                    rsx! {
                                        div { class: "az-status",
                                            span { class: "{dot}" }
                                            span { "{msg}" }
                                            button {
                                                class: "btn-primary",
                                                onclick: move |_| {
                                                    let tenant = tenant_input.read().clone();
                                                    let t = if tenant.is_empty() { None } else { Some(tenant.clone()) };
                                                    azure::open_login(t.as_deref());
                                                    az_state.set(AzLoginState::Checking);
                                                    spawn(async move {
                                                        for _ in 0..24 {
                                                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                                            let state = tokio::task::spawn_blocking(azure::check_login)
                                                                .await
                                                                .unwrap_or(AzLoginState::NotLoggedIn);
                                                            if let AzLoginState::LoggedIn { ref subscription_id, .. } = state {
                                                                sub_id.set(subscription_id.clone());
                                                            }
                                                            let done = matches!(state, AzLoginState::LoggedIn { .. });
                                                            az_state.set(state);
                                                            if done { break; }
                                                        }
                                                    });
                                                },
                                                "Connect to Azure"
                                            }
                                        }
                                    }
                                },
                            }
                        }
                    }

                    // Saved profiles
                    {
                        let saved = profiles.read().clone();
                        if !saved.is_empty() {
                            rsx! {
                                div { class: "profile-section",
                                    h3 { "Profiles" }
                                    for (idx, profile) in saved.iter().enumerate() {
                                        {
                                            let p = profile.clone();
                                            let display_label = if profile.label.is_empty() {
                                                format!("{} / {}", profile.resource_group, profile.app_name)
                                            } else {
                                                profile.label.clone()
                                            };
                                            let sub_line = if !profile.label.is_empty() {
                                                format!("{} / {}", profile.resource_group, profile.app_name)
                                            } else {
                                                String::new()
                                            };
                                            let tenant_tag = if profile.tenant.is_empty() { String::new() }
                                                else { profile.tenant.clone() };
                                            let on_connect = props.on_connect.clone();
                                            rsx! {
                                                div { class: "profile-item",
                                                    div { class: "profile-main",
                                                        div { class: "profile-label", "{display_label}" }
                                                        if !sub_line.is_empty() {
                                                            div { class: "profile-sub", "{sub_line}" }
                                                        }
                                                        if !tenant_tag.is_empty() {
                                                            span { class: "profile-tenant", "{tenant_tag}" }
                                                        }
                                                    }
                                                    div { class: "profile-actions",
                                                        button {
                                                            class: "btn btn-open btn-small",
                                                            title: "Open this profile",
                                                            onclick: {
                                                                let p = p.clone();
                                                                let on_connect = on_connect.clone();
                                                                move |_| {
                                                                    let mut config = p.clone();
                                                                    if !config.tenant.is_empty() {
                                                                        azure::open_login(Some(&config.tenant));
                                                                    }
                                                                    config.subscription = sub_id.read().clone();
                                                                    on_connect.call(config);
                                                                }
                                                            },
                                                            "Open →"
                                                        }
                                                        button {
                                                            class: "btn btn-small",
                                                            title: "Edit",
                                                            onclick: move |_| {
                                                                let p = profiles.read()[idx].clone();
                                                                label_input.set(p.label.clone());
                                                                tenant_input.set(p.tenant.clone());
                                                                rg_input.set(p.resource_group.clone());
                                                                app_input.set(p.app_name.clone());
                                                                sb_input.set(p.sb_namespace.clone());
                                                                editing_profile.set(Some(idx));
                                                                show_form.set(true);
                                                            },
                                                            "Edit"
                                                        }
                                                        button {
                                                            class: "btn btn-small",
                                                            title: "Delete",
                                                            onclick: move |_| {
                                                                let mut saved = profiles.read().clone();
                                                                saved.remove(idx);
                                                                save_profiles(&saved);
                                                                profiles.set(saved);
                                                            },
                                                            "×"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            rsx! {}
                        }
                    }

                    // "+ New Profile" button
                    if !*show_form.read() {
                        div { class: "az-form",
                            button {
                                class: "btn",
                                onclick: move |_| {
                                    label_input.set(String::new());
                                    tenant_input.set(String::new());
                                    rg_input.set(String::new());
                                    app_input.set(String::new());
                                    sb_input.set(String::new());
                                    subscriptions.set(vec![]);
                                    selected_sub.set(String::new());
                                    resource_groups.set(vec![]);
                                    logic_apps.set(vec![]);
                                    sb_namespaces.set(vec![]);
                                    browse_loading.set(String::new());
                                    editing_profile.set(None);
                                    show_form.set(true);
                                    if is_logged_in {
                                        browse_loading.set("subs".into());
                                        spawn(async move {
                                            let subs = tokio::task::spawn_blocking(azure::list_subscriptions)
                                                .await
                                                .unwrap_or(Ok(vec![]))
                                                .unwrap_or_default();
                                            // Auto-select single subscription and immediately load RGs
                                            if subs.len() == 1 {
                                                let sid = subs[0].id.clone();
                                                selected_sub.set(sid.clone());
                                                subscriptions.set(subs);
                                                browse_loading.set("rgs".into());
                                                let rgs = tokio::task::spawn_blocking(move || azure::list_resource_groups(&sid))
                                                    .await.unwrap_or(Ok(vec![])).unwrap_or_default();
                                                resource_groups.set(rgs);
                                            } else {
                                                subscriptions.set(subs);
                                            }
                                            browse_loading.set(String::new());
                                        });
                                    }
                                },
                                "+ New Profile"
                            }
                        }
                    }

                    // Browse form — new profile while logged in
                    if is_browse_mode {
                        div { class: "az-form",
                            h3 { "New Profile" }

                            div { class: "az-field",
                                label { "Profile Name (optional)" }
                                input {
                                    r#type: "text",
                                    placeholder: "Acme Corp — PRD",
                                    value: "{label_input.read()}",
                                    oninput: move |e| label_input.set(e.value().clone()),
                                }
                            }

                            div { class: "az-field",
                                label { "Subscription" }
                                if browse_loading.read().as_str() == "subs" {
                                    div { class: "az-loading", "Loading subscriptions..." }
                                } else {
                                    select {
                                        onchange: move |e| {
                                            let val = e.value();
                                            selected_sub.set(val.clone());
                                            resource_groups.set(vec![]);
                                            rg_input.set(String::new());
                                            logic_apps.set(vec![]);
                                            app_input.set(String::new());
                                            sb_namespaces.set(vec![]);
                                            sb_input.set(String::new());
                                            browse_loading.set("rgs".into());
                                            spawn(async move {
                                                let rgs = tokio::task::spawn_blocking(move || azure::list_resource_groups(&val))
                                                    .await.unwrap_or(Ok(vec![])).unwrap_or_default();
                                                resource_groups.set(rgs);
                                                browse_loading.set(String::new());
                                            });
                                        },
                                        if selected_sub.read().is_empty() {
                                            option { value: "", "Select subscription..." }
                                        }
                                        for sub in subscriptions.read().iter() {
                                            {
                                                let is_selected = sub.id == *selected_sub.read();
                                                rsx! {
                                                    option {
                                                        value: "{sub.id}",
                                                        selected: is_selected,
                                                        "{sub.name}"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            if !selected_sub.read().is_empty() {
                                div { class: "az-field",
                                    label { "Resource Group" }
                                    if browse_loading.read().as_str() == "rgs" {
                                        div { class: "az-loading", "Loading resource groups..." }
                                    } else {
                                        select {
                                            onchange: move |e| {
                                                let val = e.value();
                                                rg_input.set(val.clone());
                                                logic_apps.set(vec![]);
                                                app_input.set(String::new());
                                                sb_namespaces.set(vec![]);
                                                sb_input.set(String::new());
                                                browse_loading.set("apps".into());
                                                let sub = selected_sub.read().clone();
                                                spawn(async move {
                                                    let sub_a = sub.clone();
                                                    let rg_a = val.clone();
                                                    let sub_b = sub.clone();
                                                    let rg_b = val.clone();
                                                    let (apps_res, sbs_res) = tokio::join!(
                                                        tokio::task::spawn_blocking(move || azure::list_logic_apps(&sub_a, &rg_a)),
                                                        tokio::task::spawn_blocking(move || azure::list_service_bus_namespaces(&sub_b, &rg_b)),
                                                    );
                                                    logic_apps.set(apps_res.unwrap_or(Ok(vec![])).unwrap_or_default());
                                                    sb_namespaces.set(sbs_res.unwrap_or(Ok(vec![])).unwrap_or_default());
                                                    browse_loading.set(String::new());
                                                });
                                            },
                                            if rg_input.read().is_empty() {
                                                option { value: "", "Select resource group..." }
                                            }
                                            for rg in resource_groups.read().iter() {
                                                {
                                                    let is_selected = rg == &*rg_input.read();
                                                    rsx! {
                                                        option {
                                                            value: "{rg}",
                                                            selected: is_selected,
                                                            "{rg}"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            if !rg_input.read().is_empty() {
                                div { class: "az-field",
                                    label { "Logic App" }
                                    if browse_loading.read().as_str() == "apps" {
                                        div { class: "az-loading", "Loading Logic Apps..." }
                                    } else if logic_apps.read().is_empty() {
                                        div { class: "az-hint", "No Logic Apps (Standard) found in this resource group" }
                                    } else {
                                        select {
                                            onchange: move |e| app_input.set(e.value()),
                                            if app_input.read().is_empty() {
                                                option { value: "", "Select Logic App..." }
                                            }
                                            for app in logic_apps.read().iter() {
                                                {
                                                    let is_selected = app == &*app_input.read();
                                                    rsx! {
                                                        option {
                                                            value: "{app}",
                                                            selected: is_selected,
                                                            "{app}"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            if !rg_input.read().is_empty() && !sb_namespaces.read().is_empty() {
                                div { class: "az-field",
                                    label { "Service Bus Namespace (optional)" }
                                    select {
                                        onchange: move |e| sb_input.set(e.value()),
                                        option { value: "", "None" }
                                        for ns in sb_namespaces.read().iter() {
                                            {
                                                let is_selected = ns == &*sb_input.read();
                                                rsx! {
                                                    option {
                                                        value: "{ns}",
                                                        selected: is_selected,
                                                        "{ns}"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            {
                                let err = error_msg.read().clone();
                                if let Some(msg) = err {
                                    rsx! { div { class: "az-error", "{msg}" } }
                                } else {
                                    rsx! {}
                                }
                            }

                            div { class: "az-form-actions",
                                button {
                                    class: "btn-primary",
                                    disabled: !can_connect,
                                    onclick: {
                                        let on_connect = props.on_connect.clone();
                                        move |_| {
                                            let config = AzConfig {
                                                subscription: selected_sub.read().clone(),
                                                resource_group: rg_input.read().trim().to_string(),
                                                app_name: app_input.read().trim().to_string(),
                                                sb_namespace: sb_input.read().trim().to_string(),
                                                tenant: tenant_input.read().trim().to_string(),
                                                label: label_input.read().trim().to_string(),
                                            };
                                            let mut saved = profiles.read().clone();
                                            saved.insert(0, config.clone());
                                            save_profiles(&saved);
                                            profiles.set(saved);
                                            show_form.set(false);
                                            error_msg.set(None);
                                            on_connect.call(config);
                                        }
                                    },
                                    "Save & Connect"
                                }
                                button {
                                    class: "btn",
                                    onclick: move |_| {
                                        show_form.set(false);
                                        editing_profile.set(None);
                                    },
                                    "Cancel"
                                }
                            }
                        }
                    }
                    // Manual form — edit profile or not yet logged in
                    else if *show_form.read() {
                        div { class: "az-form",
                            h3 {
                                if editing_profile.read().is_some() { "Edit Profile" } else { "New Profile" }
                            }
                            div { class: "az-field",
                                label { "Profile Name" }
                                input {
                                    r#type: "text",
                                    placeholder: "Acme Corp — PRD",
                                    value: "{label_input.read()}",
                                    oninput: move |e| label_input.set(e.value().clone()),
                                }
                            }
                            div { class: "az-field",
                                label { "Tenant ID (optional — for multi-tenant)" }
                                input {
                                    r#type: "text",
                                    placeholder: "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
                                    value: "{tenant_input.read()}",
                                    oninput: move |e| tenant_input.set(e.value().clone()),
                                }
                            }
                            div { class: "az-field",
                                label { "Resource Group" }
                                input {
                                    r#type: "text",
                                    placeholder: "rg-myapp-prd-001",
                                    value: "{rg_input.read()}",
                                    oninput: move |e| rg_input.set(e.value().clone()),
                                }
                            }
                            div { class: "az-field",
                                label { "Logic App Name" }
                                input {
                                    r#type: "text",
                                    placeholder: "logic-myapp-prd-001",
                                    value: "{app_input.read()}",
                                    oninput: move |e| app_input.set(e.value().clone()),
                                }
                            }
                            div { class: "az-field",
                                label { "Service Bus Namespace (optional)" }
                                input {
                                    r#type: "text",
                                    placeholder: "sbns-myapp-prd-001",
                                    value: "{sb_input.read()}",
                                    oninput: move |e| sb_input.set(e.value().clone()),
                                }
                            }
                            {
                                let err = error_msg.read().clone();
                                if let Some(msg) = err {
                                    rsx! { div { class: "az-error", "{msg}" } }
                                } else {
                                    rsx! {}
                                }
                            }
                            div { class: "az-form-actions",
                                button {
                                    class: "btn-primary",
                                    disabled: !can_connect,
                                    onclick: {
                                        let on_connect = props.on_connect.clone();
                                        move |_| {
                                            let config = AzConfig {
                                                subscription: sub_id.read().clone(),
                                                resource_group: rg_input.read().trim().to_string(),
                                                app_name: app_input.read().trim().to_string(),
                                                sb_namespace: sb_input.read().trim().to_string(),
                                                tenant: tenant_input.read().trim().to_string(),
                                                label: label_input.read().trim().to_string(),
                                            };
                                            let mut saved = profiles.read().clone();
                                            if let Some(idx) = *editing_profile.read() {
                                                saved[idx] = config.clone();
                                            } else {
                                                saved.insert(0, config.clone());
                                            }
                                            save_profiles(&saved);
                                            profiles.set(saved);
                                            show_form.set(false);
                                            editing_profile.set(None);
                                            error_msg.set(None);
                                            if is_logged_in {
                                                on_connect.call(config);
                                            }
                                        }
                                    },
                                    if is_logged_in { "Save & Connect" } else { "Save" }
                                }
                                button {
                                    class: "btn",
                                    onclick: move |_| {
                                        show_form.set(false);
                                        editing_profile.set(None);
                                    },
                                    "Cancel"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn config_file() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ais-monitor")
        .join("profiles.json")
}

fn load_profiles() -> Vec<AzConfig> {
    let path = config_file();
    let content = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

fn save_profiles(profiles: &[AzConfig]) {
    let path = config_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(profiles) {
        let _ = std::fs::write(path, json);
    }
}
