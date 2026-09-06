use crate::components::chain_detail::AzConfig;
use crate::hooks::signin;
use crate::services::azure::{self, AzLoginState, AzSubscription, LogicAppSite};
use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct WelcomeProps {
    pub on_connect: EventHandler<AzConfig>,
}

/// Signs in and updates this screen as it lands.
///
/// `busy` drives the "Checking…" panel for the whole wait, so the screen can
/// never look idle while a sign-in is in flight.
fn start_login(
    mut az_state: Signal<AzLoginState>,
    mut sub_id: Signal<String>,
    busy: Signal<bool>,
    tenant: &str,
) {
    az_state.set(AzLoginState::Checking);
    signin::sign_in_and_wait(tenant, busy, move |state| {
        if let AzLoginState::LoggedIn {
            ref subscription_id,
            ..
        } = state
        {
            sub_id.set(subscription_id.clone());
        }
        az_state.set(state);
    });
}

#[component]
pub fn Welcome(props: WelcomeProps) -> Element {
    let mut az_state = use_signal(|| AzLoginState::Checking);
    // True for the whole sign-in wait, so no sign-in button can look inert
    // while a browser flow is in progress.
    let signing_in = use_signal(|| false);
    let mut sub_id = use_signal(String::new);

    // Form fields (shared between browse and manual modes)
    let mut label_input = use_signal(String::new);
    let mut tenant_input = use_signal(String::new);
    let mut rg_input = use_signal(String::new);
    let mut app_input = use_signal(String::new);
    let mut sb_input = use_signal(String::new);
    let mut local_dir_input = use_signal(String::new);
    let mut app_config_store_input = use_signal(String::new);
    let mut devops_org_input = use_signal(String::new);
    let mut devops_project_input = use_signal(String::new);
    let mut error_msg = use_signal(|| Option::<String>::None);
    let mut show_form = use_signal(|| false);
    let mut editing_profile = use_signal(|| Option::<usize>::None);

    // Browse mode state
    let mut subscriptions = use_signal(Vec::<AzSubscription>::new);
    let mut selected_sub = use_signal(String::new);
    // Logic App sites loaded directly (no resource-group step)
    let mut logic_app_sites = use_signal(Vec::<LogicAppSite>::new);
    let mut sites_error = use_signal(|| Option::<String>::None);
    let mut sb_namespaces = use_signal(Vec::<String>::new);
    // "" | "subs" | "apps"
    let mut browse_loading = use_signal(String::new);
    // Error feedback when `az login` fails to spawn (Windows: az.cmd not on PATH, etc.)
    let mut login_error: Signal<Option<String>> = use_signal(|| None);
    // Profile index currently being validated before open — drives the
    // "Checking…" state and disables its button while the az call is in flight.
    let mut validating_profile: Signal<Option<usize>> = use_signal(|| None);
    // Validation failures per profile index, e.g. a resource group that no
    // longer exists in the profile's subscription. Surfaced inline so a stale
    // profile fails fast here instead of as a raw `az rest` error deep in
    // some panel after connecting.
    let mut open_errors: Signal<std::collections::HashMap<usize, String>> =
        use_signal(std::collections::HashMap::new);
    // Same idea as `validating_profile`/`open_errors`, but for the manual
    // new-profile form, where every field (including resource group and app
    // name) is hand-typed and so is exactly as typo-prone as a stale profile.
    let mut validating_form: Signal<bool> = use_signal(|| false);

    let mut profiles = use_signal(load_profiles);

    use_effect(move || {
        spawn(async move {
            let state = tokio::task::spawn_blocking(azure::check_login)
                .await
                .unwrap_or(AzLoginState::NotLoggedIn);
            if let AzLoginState::LoggedIn {
                ref subscription_id,
                ..
            } = state
            {
                sub_id.set(subscription_id.clone());
                // Auto-open profile creation when logged in with no saved profiles
                if profiles.read().is_empty() {
                    show_form.set(true);
                    browse_loading.set("subs".into());
                    let subs = tokio::task::spawn_blocking(azure::list_subscriptions)
                        .await
                        .unwrap_or(Ok(vec![]))
                        .unwrap_or_default();
                    if subs.len() == 1 {
                        let sid = subs[0].id.clone();
                        selected_sub.set(sid.clone());
                        subscriptions.set(subs);
                        browse_loading.set("apps".into());
                        match tokio::task::spawn_blocking(move || azure::list_logic_app_sites(&sid))
                            .await
                        {
                            Ok(Ok(sites)) => {
                                logic_app_sites.set(sites);
                                sites_error.set(None);
                            }
                            Ok(Err(e)) => {
                                logic_app_sites.set(vec![]);
                                sites_error.set(Some(e));
                            }
                            Err(e) => {
                                logic_app_sites.set(vec![]);
                                sites_error.set(Some(e.to_string()));
                            }
                        }
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
    let can_connect = !app_input.read().is_empty();

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
                                        button {
                                            style: "margin-left:10px; font-size:11px; background:none; border:1px solid currentColor; border-radius:4px; padding:1px 8px; cursor:pointer; opacity:0.6;",
                                            title: "Sign in with a different Azure account",
                                            onclick: move |_| {
                                                start_login(az_state, sub_id, signing_in, "");
                                            },
                                            "Switch account"
                                        }
                                    }
                                    if profiles.read().is_empty() {
                                        p { style: "margin:8px 0 0; font-size:12px; opacity:0.65;",
                                            "Select a Logic App below to start monitoring."
                                        }
                                    }
                                },
                                AzLoginState::AzNotFound => rsx! {
                                    div { class: "az-status",
                                        span { class: "dot error" }
                                        span { "Azure CLI not found" }
                                    }
                                    p { style: "margin:8px 0 0; font-size:12px; color:var(--red);",
                                        {
                                            #[cfg(target_os = "macos")]
                                            { rsx! {
                                                "The 'az' command was not found. Install Azure CLI with "
                                                code { "brew install azure-cli" }
                                                " (or from "
                                                a { href: "https://learn.microsoft.com/cli/azure/install-azure-cli-macos", target: "_blank",
                                                    "Microsoft Docs"
                                                }
                                                ") then restart the app."
                                            } }
                                            #[cfg(target_os = "linux")]
                                            { rsx! {
                                                "The 'az' command was not found. Install Azure CLI from "
                                                a { href: "https://aka.ms/installazurecli-linux", target: "_blank",
                                                    "aka.ms/installazurecli-linux"
                                                }
                                                " then restart the app."
                                            } }
                                            #[cfg(target_os = "windows")]
                                            { rsx! {
                                                "The 'az' command was not found. Install Azure CLI from "
                                                a { href: "https://aka.ms/installazurecliwindows", target: "_blank",
                                                    "aka.ms/installazurecliwindows"
                                                }
                                                " then restart the app."
                                            } }
                                        }
                                    }
                                    p { style: "margin:4px 0 0; font-size:11px; opacity:0.7;",
                                        {
                                            #[cfg(target_os = "macos")]
                                            { "If installed, the GUI app may not see your shell PATH — relaunch via Launchpad after install." }
                                            #[cfg(target_os = "linux")]
                                            { "If installed, restart this app so the new PATH is picked up." }
                                            #[cfg(target_os = "windows")]
                                            { "On Windows, close and reopen your terminal after installing to refresh the PATH." }
                                        }
                                    }
                                },
                                AzLoginState::Expired | AzLoginState::NotLoggedIn => {
                                    let msg = if matches!(state, AzLoginState::Expired) { "Session expired" } else { "Not logged in" };
                                    let dot = if matches!(state, AzLoginState::Expired) { "dot warn" } else { "dot error" };
                                    let err = login_error.read().clone();
                                    rsx! {
                                        div { class: "az-status",
                                            span { class: "{dot}" }
                                            span { "{msg}" }
                                            button {
                                                class: "btn-primary",
                                                onclick: move |_| {
                                                    let tenant = tenant_input.read().clone();
                                                    login_error.set(None);
                                                    start_login(az_state, sub_id, signing_in, &tenant);
                                                },
                                                "Connect to Azure"
                                            }
                                        }
                                        if let Some(e) = err {
                                            p { style: "margin:8px 0 0; font-size:12px; color:var(--red); word-break:break-word;",
                                                "{e}"
                                            }
                                            p { style: "margin:4px 0 0; font-size:11px; opacity:0.7;",
                                                "If the browser didn't open, run "
                                                code { style: "font-family:monospace; background:var(--bg2); padding:1px 5px; border-radius:3px;", "az login" }
                                                " from a terminal, then click here again."
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
                                            let on_connect = props.on_connect;
                                            rsx! {
                                                div { class: "profile-item",
                                                  div { class: "profile-row",
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
                                                            title: if is_logged_in { "Open this profile" } else { "Log in first" },
                                                            disabled: !is_logged_in || validating_profile.read().is_some(),
                                                            onclick: {
                                                                let p = p.clone();
                                                                move |_| {
                                                                    let mut config = p.clone();
                                                                    // Use the profile's own subscription — falling back to
                                                                    // whatever the CLI is currently active on only applies
                                                                    // to profiles saved before this field existed. Using
                                                                    // the active CLI subscription unconditionally broke
                                                                    // profiles when a different profile (or another tool)
                                                                    // had last switched `az` to another subscription.
                                                                    if config.subscription.is_empty() {
                                                                        config.subscription = sub_id.read().clone();
                                                                    }
                                                                    open_errors.write().remove(&idx);
                                                                    validating_profile.set(Some(idx));
                                                                        spawn(async move {
                                                                        // Check the tenant rather than switching blindly.
                                                                        // The old code fired `az login --tenant` and then
                                                                        // validated immediately, so a profile in another
                                                                        // tenant raced a browser it had just opened and
                                                                        // failed with "not found" — blaming the profile for
                                                                        // what was a timing problem. It also opened that
                                                                        // browser on every open, even when the session was
                                                                        // already right.
                                                                        let wanted = config.tenant.clone();
                                                                        let session = tokio::task::spawn_blocking(
                                                                            azure::current_tenant,
                                                                        )
                                                                        .await
                                                                        .unwrap_or(None);

                                                                        if azure::needs_tenant_switch(
                                                                            &wanted,
                                                                            session.as_deref(),
                                                                        ) {
                                                                            validating_profile.set(None);
                                                                            open_errors.write().insert(
                                                                                idx,
                                                                                format!(
                                                                                    "This profile is in tenant {wanted}, \
                                                                                     but you are signed in to {}. Sign in \
                                                                                     to that tenant above, then open it.",
                                                                                    session.unwrap_or_else(|| "another one".into())
                                                                                ),
                                                                            );
                                                                            return;
                                                                        }

                                                                        let sub = config.subscription.clone();
                                                                        let rg = config.resource_group.clone();
                                                                        let app = config.app_name.clone();
                                                                        // One cheap `az webapp show` validates subscription,
                                                                        // resource group and site name together — the exact
                                                                        // three fields a stale/mistyped profile gets wrong.
                                                                        let result = tokio::task::spawn_blocking(move || {
                                                                            azure::get_site_location(&sub, &rg, &app)
                                                                        })
                                                                        .await
                                                                        .unwrap_or_else(|e| Err(e.to_string()));
                                                                        validating_profile.set(None);
                                                                        match result {
                                                                            Ok(_) => on_connect.call(config),
                                                                            Err(e) => {
                                                                                open_errors.write().insert(idx, e);
                                                                            }
                                                                        }
                                                                    });
                                                                }
                                                            },
                                                            if *validating_profile.read() == Some(idx) { "Checking…" } else { "Open →" }
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
                                                                local_dir_input.set(p.local_dir.clone());
                                                                app_config_store_input.set(p.app_config_store.clone());
                                                                devops_org_input.set(p.devops_org.clone());
                                                                devops_project_input.set(p.devops_project.clone());
                                                                editing_profile.set(Some(idx));
                                                                show_form.set(true);
                                                            },
                                                            "Edit"
                                                        }
                                                        button {
                                                            class: "btn btn-small",
                                                            title: "Delete",
                                                            onclick: move |_| {
                                                                let Some(target) = profiles.read().get(idx).cloned() else {
                                                                    return;
                                                                };
                                                                profiles.set(delete_profile(&target));
                                                            },
                                                            "×"
                                                        }
                                                    }
                                                  }
                                                    if let Some(err) = open_errors.read().get(&idx).cloned() {
                                                        div { class: "az-error", style: "margin-top:6px",
                                                            "⚠ {err}"
                                                            button {
                                                                class: "btn btn-small",
                                                                style: "margin-left:10px",
                                                                onclick: {
                                                                    let p = p.clone();
                                                                        move |_| {
                                                                        let mut config = p.clone();
                                                                        if config.subscription.is_empty() {
                                                                            config.subscription = sub_id.read().clone();
                                                                        }
                                                                        open_errors.write().remove(&idx);
                                                                        on_connect.call(config);
                                                                    }
                                                                },
                                                                "Open anyway"
                                                            }
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
                                    local_dir_input.set(String::new());
                                    subscriptions.set(vec![]);
                                    selected_sub.set(String::new());
                                    logic_app_sites.set(vec![]);
                                    sb_namespaces.set(vec![]);
                                    browse_loading.set(String::new());
                                    editing_profile.set(None);
                                    show_form.set(true);
                                    if is_logged_in {
                                        browse_loading.set("subs".into());
                                        spawn(async move {
                                            let subs = tokio::task::spawn_blocking(azure::list_subscriptions)
                                                .await.unwrap_or(Ok(vec![])).unwrap_or_default();
                                            if subs.len() == 1 {
                                                let sid = subs[0].id.clone();
                                                selected_sub.set(sid.clone());
                                                subscriptions.set(subs);
                                                browse_loading.set("apps".into());
                                                match tokio::task::spawn_blocking(move || azure::list_logic_app_sites(&sid)).await {
                                                    Ok(Ok(s)) => { logic_app_sites.set(s); sites_error.set(None); }
                                                    Ok(Err(e)) => { logic_app_sites.set(vec![]); sites_error.set(Some(e)); }
                                                    Err(e)    => { logic_app_sites.set(vec![]); sites_error.set(Some(e.to_string())); }
                                                }
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
                            // Heading: simpler when auto-opened with no profiles
                            if profiles.read().is_empty() {
                                h3 { "Select a Logic App" }
                            } else {
                                h3 { "New Profile" }
                            }

                            div { class: "az-field",
                                label { "Profile Name (optional)" }
                                input {
                                    r#type: "text",
                                    placeholder: "e.g. Production, Acme Corp…",
                                    value: "{label_input.read()}",
                                    autocapitalize: "off",
                                    spellcheck: false,
                                    oninput: move |e| label_input.set(e.value().clone()),
                                }
                            }

                            // ── Subscription (with ↻ Refresh for post-PIM) ───────
                            div { class: "az-field",
                                div { style: "display:flex; align-items:center; justify-content:space-between; margin-bottom:4px;",
                                    label { style: "margin:0;", "Subscription" }
                                    button {
                                        style: "font-size:11px; background:none; border:none; cursor:pointer; opacity:0.6; padding:0; font-family:inherit;",
                                        title: "Refresh after PIM activation",
                                        disabled: browse_loading.read().as_str() == "subs",
                                        onclick: move |_| {
                                            selected_sub.set(String::new());
                                            logic_app_sites.set(vec![]);
                                            app_input.set(String::new());
                                            rg_input.set(String::new());
                                            sb_namespaces.set(vec![]);
                                            sb_input.set(String::new());
                                            browse_loading.set("subs".into());
                                            spawn(async move {
                                                let subs = tokio::task::spawn_blocking(azure::list_subscriptions)
                                                    .await.unwrap_or(Ok(vec![])).unwrap_or_default();
                                                if subs.len() == 1 {
                                                    let sid = subs[0].id.clone();
                                                    selected_sub.set(sid.clone());
                                                    subscriptions.set(subs);
                                                    browse_loading.set("apps".into());
                                                    match tokio::task::spawn_blocking(move || azure::list_logic_app_sites(&sid)).await {
                                                        Ok(Ok(s)) => { logic_app_sites.set(s); sites_error.set(None); }
                                                        Ok(Err(e)) => { logic_app_sites.set(vec![]); sites_error.set(Some(e)); }
                                                        Err(e)    => { logic_app_sites.set(vec![]); sites_error.set(Some(e.to_string())); }
                                                    }
                                                } else {
                                                    subscriptions.set(subs);
                                                }
                                                browse_loading.set(String::new());
                                            });
                                        },
                                        "↻ Refresh"
                                    }
                                }
                                if browse_loading.read().as_str() == "subs" {
                                    div { class: "az-loading", "Loading subscriptions..." }
                                } else {
                                    select {
                                        onchange: move |e| {
                                            let val = e.value();
                                            selected_sub.set(val.clone());
                                            logic_app_sites.set(vec![]);
                                            app_input.set(String::new());
                                            rg_input.set(String::new());
                                            sb_namespaces.set(vec![]);
                                            sb_input.set(String::new());
                                            browse_loading.set("apps".into());
                                            spawn(async move {
                                                match tokio::task::spawn_blocking(move || azure::list_logic_app_sites(&val)).await {
                                                    Ok(Ok(s)) => { logic_app_sites.set(s); sites_error.set(None); }
                                                    Ok(Err(e)) => { logic_app_sites.set(vec![]); sites_error.set(Some(e)); }
                                                    Err(e)    => { logic_app_sites.set(vec![]); sites_error.set(Some(e.to_string())); }
                                                }
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

                            // ── Logic App (directly, no resource group step) ──────
                            if !selected_sub.read().is_empty() {
                                div { class: "az-field",
                                    div { style: "display:flex; align-items:center; justify-content:space-between; margin-bottom:4px;",
                                        label { style: "margin:0;", "Logic App" }
                                        if logic_app_sites.read().is_empty() && browse_loading.read().as_str() != "apps" {
                                            button {
                                                style: "font-size:11px; background:none; border:none; cursor:pointer; opacity:0.6; padding:0; font-family:inherit;",
                                                title: "Retry — activate PIM then click Refresh above",
                                                onclick: {
                                                    let sub = selected_sub.read().clone();
                                                    move |_| {
                                                        browse_loading.set("apps".into());
                                                        let sid = sub.clone();
                                                        spawn(async move {
                                                            match tokio::task::spawn_blocking(move || azure::list_logic_app_sites(&sid)).await {
                                                                Ok(Ok(s)) => { logic_app_sites.set(s); sites_error.set(None); }
                                                                Ok(Err(e)) => { logic_app_sites.set(vec![]); sites_error.set(Some(e)); }
                                                                Err(e)    => { logic_app_sites.set(vec![]); sites_error.set(Some(e.to_string())); }
                                                            }
                                                            browse_loading.set(String::new());
                                                        });
                                                    }
                                                },
                                                "↻ Retry"
                                            }
                                        }
                                    }
                                    if browse_loading.read().as_str() == "apps" {
                                        div { class: "az-loading", "Loading Logic Apps..." }
                                    } else if let Some(err) = sites_error.read().clone() {
                                        // Distinguish auth errors from permission errors
                                        {
                                            let is_auth = err.contains("AADSTS")
                                                || err.contains("az login")
                                                || err.contains("refresh token")
                                                || err.contains("Please run");
                                            rsx! {
                                                div { class: "az-error",
                                                    if is_auth {
                                                        span { "Session expired — " }
                                                        button {
                                                            class: "btn-primary",
                                                            style: "display:inline; padding:2px 10px; font-size:12px;",
                                                            onclick: move |_| {
                                                                start_login(az_state, sub_id, signing_in, "");
                                                            },
                                                            "Log in again"
                                                        }
                                                    } else {
                                                        span { "No Logic Apps found — activate PIM then click ↻ Refresh" }
                                                    }
                                                }
                                            }
                                        }
                                    } else if logic_app_sites.read().is_empty() {
                                        div { class: "az-hint",
                                            "No Logic Apps (Standard) in this subscription"
                                        }
                                    } else {
                                        select {
                                            onchange: move |e| {
                                                let val = e.value();
                                                // val = "name||rg"
                                                let mut parts = val.splitn(2, "||");
                                                let name = parts.next().unwrap_or("").to_string();
                                                let rg   = parts.next().unwrap_or("").to_string();
                                                app_input.set(name);
                                                rg_input.set(rg);
                                            },
                                            if app_input.read().is_empty() {
                                                option { value: "", "Select Logic App..." }
                                            }
                                            for site in logic_app_sites.read().iter() {
                                                {
                                                    let val = format!("{}||{}", site.name, site.resource_group);
                                                    let selected = *app_input.read() == site.name;
                                                    let label = format!("{} ({})", site.name, site.resource_group);
                                                    rsx! {
                                                        option {
                                                            value: "{val}",
                                                            selected: selected,
                                                            "{label}"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            if !app_input.read().is_empty() && !sb_namespaces.read().is_empty() {
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

                            div { class: "az-field",
                                label { "App Configuration Store (optional — source of truth for app settings drift view)" }
                                input {
                                    r#type: "text",
                                    placeholder: "appcs-myapp-prd-001",
                                    value: "{app_config_store_input.read()}",
                                    oninput: move |e| app_config_store_input.set(e.value().clone()),
                                }
                            }
                            div { class: "az-field",
                                label { "Azure DevOps org URL (optional — for variable group cleanup)" }
                                input {
                                    r#type: "text",
                                    placeholder: "https://dev.azure.com/myorg",
                                    value: "{devops_org_input.read()}",
                                    oninput: move |e| devops_org_input.set(e.value().clone()),
                                }
                            }
                            if !devops_org_input.read().is_empty() {
                                div { class: "az-field",
                                    label { "Azure DevOps project" }
                                    input {
                                        r#type: "text",
                                        placeholder: "MyProject",
                                        value: "{devops_project_input.read()}",
                                        oninput: move |e| devops_project_input.set(e.value().clone()),
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
                                        let on_connect = props.on_connect;
                                        move |_| {
                                            let config = AzConfig {
                                                subscription: selected_sub.read().clone(),
                                                resource_group: rg_input.read().trim().to_string(),
                                                app_name: app_input.read().trim().to_string(),
                                                sb_namespace: sb_input.read().trim().to_string(),
                                                tenant: tenant_input.read().trim().to_string(),
                                                label: label_input.read().trim().to_string(),
                                                // No workspace picker on this quick-connect form — leaving
                                                // this empty means remote_chain falls back to the per-tenant
                                                // /remote/{sub}/{app} manual-links key instead of every such
                                                // profile sharing one file keyed by the home directory.
                                                local_dir: String::new(),
                                                app_config_store: app_config_store_input.read().trim().to_string(),
                                                devops_org: devops_org_input.read().trim().to_string(),
                                                devops_project: devops_project_input.read().trim().to_string(),
                                            };
                                            profiles.set(upsert_profile(&config));
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
                                    autocapitalize: "off",
                                    spellcheck: false,
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
                            div { class: "az-field",
                                label { "Local Workspace (optional — for chain links & payload suggestions)" }
                                div { style: "display:flex; gap:6px;",
                                    input {
                                        r#type: "text",
                                        placeholder: "/path/to/platform",
                                        value: "{local_dir_input.read()}",
                                        style: "flex:1;",
                                        oninput: move |e| local_dir_input.set(e.value().clone()),
                                    }
                                    button {
                                        class: "btn btn-small",
                                        r#type: "button",
                                        onclick: move |_| {
                                            spawn(async move {
                                                if let Some(path) = rfd::AsyncFileDialog::new()
                                                    .set_title("Select local workspace folder")
                                                    .pick_folder()
                                                    .await
                                                {
                                                    local_dir_input.set(path.path().to_string_lossy().to_string());
                                                }
                                            });
                                        },
                                        "Browse…"
                                    }
                                }
                            }
                            div { class: "az-field",
                                label { "App Configuration Store (optional — source of truth for app settings drift view)" }
                                input {
                                    r#type: "text",
                                    placeholder: "appcs-myapp-prd-001",
                                    value: "{app_config_store_input.read()}",
                                    oninput: move |e| app_config_store_input.set(e.value().clone()),
                                }
                            }
                            div { class: "az-field",
                                label { "Azure DevOps org URL (optional — for variable group cleanup)" }
                                input {
                                    r#type: "text",
                                    placeholder: "https://dev.azure.com/myorg",
                                    value: "{devops_org_input.read()}",
                                    oninput: move |e| devops_org_input.set(e.value().clone()),
                                }
                            }
                            if !devops_org_input.read().is_empty() {
                                div { class: "az-field",
                                    label { "Azure DevOps project" }
                                    input {
                                        r#type: "text",
                                        placeholder: "MyProject",
                                        value: "{devops_project_input.read()}",
                                        oninput: move |e| devops_project_input.set(e.value().clone()),
                                    }
                                }
                            }
                            {
                                let err = error_msg.read().clone();
                                if let Some(msg) = err {
                                    rsx! {
                                        div { class: "az-error",
                                            "⚠ {msg}"
                                            button {
                                                class: "btn btn-small",
                                                style: "margin-left:10px",
                                                onclick: {
                                                    let on_connect = props.on_connect;
                                                    move |_| {
                                                        let local_dir_val = local_dir_input.read().trim().to_string();
                                                        let config = AzConfig {
                                                            subscription: sub_id.read().clone(),
                                                            resource_group: rg_input.read().trim().to_string(),
                                                            app_name: app_input.read().trim().to_string(),
                                                            sb_namespace: sb_input.read().trim().to_string(),
                                                            tenant: tenant_input.read().trim().to_string(),
                                                            label: label_input.read().trim().to_string(),
                                                            local_dir: local_dir_val,
                                                            app_config_store: app_config_store_input.read().trim().to_string(),
                                                            devops_org: devops_org_input.read().trim().to_string(),
                                                            devops_project: devops_project_input.read().trim().to_string(),
                                                        };
                                                        // Address the profile being edited by identity, not by
                                                        // its index in this window's stale copy of the list.
                                                        let previous = editing_profile
                                                            .read()
                                                            .and_then(|idx| profiles.read().get(idx).cloned());
                                                        let updated = match previous {
                                                            Some(prev) => replace_profile(&prev, &config),
                                                            None => upsert_profile(&config),
                                                        };
                                                        profiles.set(updated);
                                                        show_form.set(false);
                                                        editing_profile.set(None);
                                                        error_msg.set(None);
                                                        on_connect.call(config);
                                                    }
                                                },
                                                "Connect anyway"
                                            }
                                        }
                                    }
                                } else {
                                    rsx! {}
                                }
                            }
                            div { class: "az-form-actions",
                                button {
                                    class: "btn-primary",
                                    disabled: !can_connect || *validating_form.read(),
                                    onclick: {
                                        let on_connect = props.on_connect;
                                        move |_| {
                                            // Left blank, this stays empty rather than falling back to the
                                            // home directory — remote_chain then keys manual links by
                                            // /remote/{sub}/{app}, which is unique per profile. Defaulting
                                            // to home directory made every profile without an explicit
                                            // workspace share one links file, leaking one tenant's manual
                                            // chain links (and its list_runs polling) into another's.
                                            let local_dir_val = local_dir_input.read().trim().to_string();
                                            let config = AzConfig {
                                                subscription: sub_id.read().clone(),
                                                resource_group: rg_input.read().trim().to_string(),
                                                app_name: app_input.read().trim().to_string(),
                                                sb_namespace: sb_input.read().trim().to_string(),
                                                tenant: tenant_input.read().trim().to_string(),
                                                label: label_input.read().trim().to_string(),
                                                local_dir: local_dir_val,
                                                app_config_store: app_config_store_input.read().trim().to_string(),
                                                devops_org: devops_org_input.read().trim().to_string(),
                                                devops_project: devops_project_input.read().trim().to_string(),
                                            };

                                            if !is_logged_in {
                                                // Not logged in: nothing to validate against yet — save for
                                                // later, matching the pre-existing "Save" (no connect) flow.
                                                // Address the profile being edited by identity, not by
                                                // its index in this window's stale copy of the list.
                                                let previous = editing_profile
                                                    .read()
                                                    .and_then(|idx| profiles.read().get(idx).cloned());
                                                let updated = match previous {
                                                    Some(prev) => replace_profile(&prev, &config),
                                                    None => upsert_profile(&config),
                                                };
                                                profiles.set(updated);
                                                show_form.set(false);
                                                editing_profile.set(None);
                                                error_msg.set(None);
                                                return;
                                            }

                                            error_msg.set(None);
                                            validating_form.set(true);
                                            let on_connect = on_connect;
                                            spawn(async move {
                                                let sub = config.subscription.clone();
                                                let rg = config.resource_group.clone();
                                                let app = config.app_name.clone();
                                                // Every field here is hand-typed, so validate the same
                                                // way as opening a saved profile: one `az webapp show`
                                                // checks subscription, resource group and site name
                                                // together before we save a profile that can't connect.
                                                let result = tokio::task::spawn_blocking(move || {
                                                    azure::get_site_location(&sub, &rg, &app)
                                                })
                                                .await
                                                .unwrap_or_else(|e| Err(e.to_string()));
                                                validating_form.set(false);
                                                match result {
                                                    Ok(_) => {
                                                        // Address the profile being edited by identity, not by
                                                        // its index in this window's stale copy of the list.
                                                        let previous = editing_profile
                                                            .read()
                                                            .and_then(|idx| profiles.read().get(idx).cloned());
                                                        let updated = match previous {
                                                            Some(prev) => replace_profile(&prev, &config),
                                                            None => upsert_profile(&config),
                                                        };
                                                        profiles.set(updated);
                                                        show_form.set(false);
                                                        editing_profile.set(None);
                                                        error_msg.set(None);
                                                        on_connect.call(config);
                                                    }
                                                    Err(e) => error_msg.set(Some(e)),
                                                }
                                            });
                                        }
                                    },
                                    if *validating_form.read() { "Checking…" } else if is_logged_in { "Save & Connect" } else { "Save" }
                                }
                                button {
                                    class: "btn",
                                    onclick: move |_| {
                                        show_form.set(false);
                                        editing_profile.set(None);
                                        error_msg.set(None);
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
    crate::services::store::with_lock(load_profiles_unlocked)
}

/// The read half of a read-modify-write; the caller already holds the lock.
fn load_profiles_unlocked() -> Vec<AzConfig> {
    let path = config_file();
    let content = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

/// Identity of a profile, independent of its position in the list.
///
/// Every mutation below used to address a profile by its index into *this*
/// window's copy of the list. With several windows open — which the app
/// encourages — that index means nothing once another window has inserted or
/// removed something, so an edit could land on the wrong profile and a delete
/// could remove one the user was still looking at.
fn profile_key(p: &AzConfig) -> (String, String, String, String) {
    (
        p.subscription.clone(),
        p.resource_group.clone(),
        p.app_name.clone(),
        p.label.clone(),
    )
}

/// Apply `f` to the profile list as it exists *on disk right now*, then write
/// the result back — all while holding the store lock.
///
/// Load-edit-save from a signal is a read-modify-write, and each window holds
/// its own copy of the list. Without this, two windows both save their own
/// stale snapshot and whichever writes last silently discards the other's
/// change. Returns the new list so the caller can update its signal.
fn mutate_profiles(f: impl FnOnce(&mut Vec<AzConfig>)) -> Vec<AzConfig> {
    crate::services::store::with_lock(|| {
        let mut profiles = load_profiles_unlocked();
        f(&mut profiles);
        if let Ok(json) = serde_json::to_string_pretty(&profiles) {
            let _ = crate::services::store::write_locked(&config_file(), &json);
        }
        profiles
    })
}

/// Insert `config`, or replace the existing profile with the same identity.
fn upsert_profile(config: &AzConfig) -> Vec<AzConfig> {
    let key = profile_key(config);
    mutate_profiles(
        |profiles| match profiles.iter().position(|p| profile_key(p) == key) {
            Some(idx) => profiles[idx] = config.clone(),
            None => profiles.insert(0, config.clone()),
        },
    )
}

/// Replace the profile previously saved as `previous` with `config` — the
/// edit case, where the identity itself may have changed.
fn replace_profile(previous: &AzConfig, config: &AzConfig) -> Vec<AzConfig> {
    let old_key = profile_key(previous);
    let new_key = profile_key(config);
    mutate_profiles(|profiles| {
        match profiles.iter().position(|p| profile_key(p) == old_key) {
            Some(idx) => profiles[idx] = config.clone(),
            // Already gone — another window deleted it while this form was
            // open. Re-adding it is the lesser surprise: the user just
            // pressed Save.
            None => match profiles.iter().position(|p| profile_key(p) == new_key) {
                Some(idx) => profiles[idx] = config.clone(),
                None => profiles.insert(0, config.clone()),
            },
        }
    })
}

fn delete_profile(target: &AzConfig) -> Vec<AzConfig> {
    let key = profile_key(target);
    mutate_profiles(|profiles| profiles.retain(|p| profile_key(p) != key))
}
