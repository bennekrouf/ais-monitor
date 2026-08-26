use crate::components::chain_detail::AzConfig;
use crate::services::azure::{self, FunctionApp, RoleAssignment};
use dioxus::prelude::*;

const ARM_ROLE_SUGGESTIONS: &[&str] = &[
    "Storage Blob Data Contributor",
    "Storage Queue Data Contributor",
    "Key Vault Secrets User",
    "Key Vault Crypto User",
    "SQL DB Contributor",
    "Azure Service Bus Data Owner",
    "Azure Service Bus Data Sender",
    "Azure Service Bus Data Receiver",
    "Cosmos DB Account Reader Role",
    "Contributor",
    "Reader",
];

#[derive(Clone, Debug, PartialEq)]
enum RoleKind {
    Arm,
    CosmosData,
}

#[derive(Clone, Debug, PartialEq)]
struct PendingAssign {
    app_name: String,
    principal_id: String,
    command_display: String,
    kind: RoleKind,
    // ARM fields
    arm_role: String,
    arm_scope: String,
    // Cosmos fields
    cosmos_account: String,
    cosmos_role_id: String,
    cosmos_role_label: String,
    cosmos_scope: String,
}

#[derive(Props, Clone, PartialEq)]
pub struct RbacPanelProps {
    pub az_config: AzConfig,
}

#[component]
pub fn RbacPanel(props: RbacPanelProps) -> Element {
    let az = props.az_config.clone();

    let mut func_apps: Signal<Vec<FunctionApp>> = use_signal(Vec::new);
    let mut roles: Signal<Vec<(String, Vec<RoleAssignment>)>> = use_signal(Vec::new);
    let mut loading: Signal<bool> = use_signal(|| true);
    let mut error_msg: Signal<Option<String>> = use_signal(|| None);

    // Which app's "Assign role" form is open, plus its in-progress field values.
    let mut open_form_for: Signal<Option<String>> = use_signal(|| None);
    let mut form_kind: Signal<RoleKind> = use_signal(|| RoleKind::Arm);
    let mut form_arm_role: Signal<String> = use_signal(String::new);
    let mut form_arm_scope: Signal<String> = use_signal(String::new);
    let mut form_cosmos_account: Signal<String> = use_signal(String::new);
    let mut form_cosmos_role: Signal<String> = use_signal(|| "Data Contributor".to_string());
    let mut form_cosmos_scope: Signal<String> = use_signal(|| "/".to_string());

    let mut pending: Signal<Option<PendingAssign>> = use_signal(|| None);
    let mut assign_running: Signal<bool> = use_signal(|| false);
    let mut assign_error: Signal<Option<String>> = use_signal(|| None);

    let mut load = {
        let az = az.clone();
        move || {
            let az = az.clone();
            loading.set(true);
            error_msg.set(None);
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                let sub2 = sub.clone();
                let rg2 = rg.clone();
                let apps = match tokio::task::spawn_blocking(move || {
                    azure::list_function_apps(&sub2, &rg2)
                })
                .await
                {
                    Ok(Ok(apps)) => apps,
                    Ok(Err(e)) => {
                        error_msg.set(Some(e));
                        loading.set(false);
                        return;
                    }
                    Err(e) => {
                        error_msg.set(Some(format!("{e}")));
                        loading.set(false);
                        return;
                    }
                };
                func_apps.set(apps.clone());

                let mut all_roles = Vec::new();
                for app in &apps {
                    let pid = app.principal_id.clone();
                    let pid2 = pid.clone();
                    let assignments =
                        tokio::task::spawn_blocking(move || azure::list_role_assignments(&pid2))
                            .await
                            .unwrap_or(Ok(Vec::new()))
                            .unwrap_or_default();
                    all_roles.push((app.name.clone(), assignments));
                }
                roles.set(all_roles);
                loading.set(false);
            });
        }
    };

    use_effect({
        let mut load = load.clone();
        move || load()
    });

    let do_assign = {
        let az = az.clone();
        let load = load.clone();
        move |p: PendingAssign| {
            let az = az.clone();
            let mut load = load.clone();
            assign_running.set(true);
            assign_error.set(None);
            spawn(async move {
                let rg = az.resource_group.clone();
                let result: Result<(), String> =
                    tokio::task::spawn_blocking(move || match p.kind {
                        RoleKind::Arm => {
                            azure::assign_role_arm(&p.principal_id, &p.arm_role, &p.arm_scope)
                        }
                        RoleKind::CosmosData => azure::assign_cosmos_data_role(
                            &rg,
                            &p.cosmos_account,
                            &p.principal_id,
                            &p.cosmos_role_id,
                            &p.cosmos_scope,
                        ),
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("{e}")));

                match &result {
                    Ok(()) => {
                        crate::services::activity::info("Role assigned", "RBAC");
                        pending.set(None);
                        open_form_for.set(None);
                        load();
                    }
                    Err(e) => {
                        crate::services::activity::error(
                            "Role assignment failed",
                            "RBAC",
                            e.clone(),
                        );
                        assign_error.set(Some(e.clone()));
                    }
                }
                assign_running.set(false);
            });
        }
    };

    let is_loading = *loading.read();
    let err = error_msg.read().clone();
    let apps = func_apps.read().clone();
    let all_roles = roles.read().clone();
    let rg_scope = format!(
        "/subscriptions/{}/resourceGroups/{}",
        az.subscription, az.resource_group
    );

    rsx! {
        div { class: "func-panel",
            div { class: "func-header",
                h2 { "Managed Identity RBAC" }
                button {
                    class: "icon-refresh-btn",
                    title: "Refresh",
                    disabled: is_loading,
                    onclick: move |_| load(),
                    span { class: if is_loading { "icon-spin" } else { "" }, "⟳" }
                }
            }
            div { class: "func-note",
                "Reads live role assignments for each Function App's system-assigned identity. Assigning a role runs the exact az command shown before you confirm."
            }

            if is_loading {
                div { class: "func-loading", "Loading role assignments…" }
            } else if let Some(e) = err {
                div { class: "az-error", "{e}" }
            } else if apps.is_empty() {
                div { class: "func-empty", "No function apps found in this resource group." }
            } else {
                for app in &apps {
                    {
                        let app_name = app.name.clone();
                        let principal_id = app.principal_id.clone();
                        let assignments = all_roles.iter()
                            .find(|(n, _)| n == &app_name)
                            .map(|(_, r)| r.as_slice())
                            .unwrap_or(&[]);
                        let has_identity = !principal_id.is_empty();
                        let form_open = open_form_for.read().as_deref() == Some(app_name.as_str());
                        let an_open = app_name.clone();
                        let an_submit = app_name.clone();
                        let pid_submit = principal_id.clone();
                        let rg_scope_default = rg_scope.clone();
                        let rg_for_cosmos = az.resource_group.clone();
                        rsx! {
                            div { class: "func-app-card",
                                div { class: "func-app-header",
                                    h3 { "{app_name}" }
                                    if has_identity {
                                        span { class: "func-app-count", title: "{principal_id}", "identity: {principal_id}" }
                                    } else {
                                        span { class: "func-badge-disabled", "No system-assigned identity" }
                                    }
                                    if has_identity {
                                        button {
                                            class: "btn btn-small",
                                            style: "margin-left:auto;",
                                            onclick: move |_| {
                                                if form_open {
                                                    open_form_for.set(None);
                                                } else {
                                                    form_arm_scope.set(rg_scope_default.clone());
                                                    open_form_for.set(Some(an_open.clone()));
                                                }
                                            },
                                            if form_open { "Cancel" } else { "Assign role" }
                                        }
                                    }
                                }

                                if assignments.is_empty() {
                                    div { class: "func-empty-small",
                                        if has_identity { "No RBAC role assignments found." } else { "No identity to assign roles to." }
                                    }
                                } else {
                                    table { class: "func-table",
                                        thead { tr { th { "Role" } th { "Scope" } } }
                                        tbody {
                                            for a in assignments {
                                                tr { class: "func-row",
                                                    td { class: "func-name", "{a.role_name}" }
                                                    td { style: "font-size:11px; color:var(--text2);", title: "{a.scope}", "{a.scope}" }
                                                }
                                            }
                                        }
                                    }
                                }

                                if form_open {
                                    div { class: "sb-send-panel",
                                        div { class: "sb-send-header", "Assign a role to this identity" }
                                        div { class: "az-field",
                                            label { "Role type" }
                                            select {
                                                onchange: move |e| {
                                                    form_kind.set(if e.value() == "cosmos" { RoleKind::CosmosData } else { RoleKind::Arm });
                                                },
                                                option { value: "arm", "ARM role (Storage / Key Vault / SQL / Service Bus / etc.)" }
                                                option { value: "cosmos", "Cosmos DB data-plane role" }
                                            }
                                        }
                                        if *form_kind.read() == RoleKind::Arm {
                                            div { class: "az-field",
                                                label { "Role name" }
                                                input {
                                                    r#type: "text",
                                                    list: "arm-role-suggestions",
                                                    value: "{form_arm_role.read()}",
                                                    oninput: move |e| form_arm_role.set(e.value().clone()),
                                                }
                                                datalist { id: "arm-role-suggestions",
                                                    for r in ARM_ROLE_SUGGESTIONS {
                                                        option { value: "{r}" }
                                                    }
                                                }
                                            }
                                            div { class: "az-field",
                                                label { "Scope" }
                                                input {
                                                    r#type: "text",
                                                    value: "{form_arm_scope.read()}",
                                                    oninput: move |e| form_arm_scope.set(e.value().clone()),
                                                }
                                            }
                                        } else {
                                            div { class: "az-field",
                                                label { "Cosmos account name" }
                                                input {
                                                    r#type: "text",
                                                    value: "{form_cosmos_account.read()}",
                                                    oninput: move |e| form_cosmos_account.set(e.value().clone()),
                                                }
                                            }
                                            div { class: "az-field",
                                                label { "Data role" }
                                                select {
                                                    onchange: move |e| form_cosmos_role.set(e.value().clone()),
                                                    option { value: "Data Reader", "Data Reader" }
                                                    option { value: "Data Contributor", selected: true, "Data Contributor" }
                                                }
                                            }
                                            div { class: "az-field",
                                                label { "Scope (Cosmos resource path)" }
                                                input {
                                                    r#type: "text",
                                                    value: "{form_cosmos_scope.read()}",
                                                    oninput: move |e| form_cosmos_scope.set(e.value().clone()),
                                                }
                                            }
                                        }
                                        button {
                                            class: "btn btn-small btn-primary",
                                            onclick: move |_| {
                                                let kind = form_kind.read().clone();
                                                let p = match kind {
                                                    RoleKind::Arm => {
                                                        let role = form_arm_role.read().trim().to_string();
                                                        let scope = form_arm_scope.read().trim().to_string();
                                                        PendingAssign {
                                                            app_name: an_submit.clone(),
                                                            principal_id: pid_submit.clone(),
                                                            command_display: format!("az role assignment create --assignee {} --role \"{}\" --scope {}", pid_submit, role, scope),
                                                            kind: RoleKind::Arm,
                                                            arm_role: role,
                                                            arm_scope: scope,
                                                            cosmos_account: String::new(),
                                                            cosmos_role_id: String::new(),
                                                            cosmos_role_label: String::new(),
                                                            cosmos_scope: String::new(),
                                                        }
                                                    }
                                                    RoleKind::CosmosData => {
                                                        let account = form_cosmos_account.read().trim().to_string();
                                                        let role_label = form_cosmos_role.read().clone();
                                                        let role_id = if role_label == "Data Reader" {
                                                            "00000000-0000-0000-0000-000000000001"
                                                        } else {
                                                            "00000000-0000-0000-0000-000000000002"
                                                        }.to_string();
                                                        let scope = form_cosmos_scope.read().trim().to_string();
                                                        PendingAssign {
                                                            app_name: an_submit.clone(),
                                                            principal_id: pid_submit.clone(),
                                                            command_display: format!(
                                                                "az cosmosdb sql role assignment create --account-name {} --resource-group {} --principal-id {} --role-definition-id {} --scope {}",
                                                                account, rg_for_cosmos, pid_submit, role_id, scope
                                                            ),
                                                            kind: RoleKind::CosmosData,
                                                            arm_role: String::new(),
                                                            arm_scope: String::new(),
                                                            cosmos_account: account,
                                                            cosmos_role_id: role_id,
                                                            cosmos_role_label: role_label,
                                                            cosmos_scope: scope,
                                                        }
                                                    }
                                                };
                                                pending.set(Some(p));
                                            },
                                            "Review & assign…"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(p) = pending.read().clone() {
                {
                    let running = *assign_running.read();
                    let err = assign_error.read().clone();
                    let command = p.command_display.clone();
                    let app_name = p.app_name.clone();
                    rsx! {
                        div { class: "modal-backdrop",
                            onclick: move |_| if !running { pending.set(None); },
                            div { class: "modal-card",
                                onclick: move |e: Event<MouseData>| e.stop_propagation(),
                                h3 { class: "modal-title", "Assign role to {app_name}?" }
                                p { class: "modal-body",
                                    "This runs:"
                                    br {}
                                    code { "{command}" }
                                }
                                if let Some(e) = err {
                                    div { class: "az-error", "{e}" }
                                }
                                div { class: "modal-actions",
                                    button {
                                        class: "btn btn-small",
                                        disabled: running,
                                        onclick: move |_| pending.set(None),
                                        "Cancel"
                                    }
                                    button {
                                        class: "btn btn-small btn-primary",
                                        disabled: running,
                                        onclick: {
                                            let p = p.clone();
                                            let mut do_assign = do_assign.clone();
                                            move |_| do_assign(p.clone())
                                        },
                                        if running { "Assigning…" } else { "Assign" }
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
