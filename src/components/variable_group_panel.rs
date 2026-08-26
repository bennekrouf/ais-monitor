use crate::components::chain_detail::AzConfig;
use crate::services::azure::{self, VariableGroup};
use crate::services::pipeline_scan;
use dioxus::prelude::*;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq)]
struct VarRow {
    group_id: u64,
    group_name: String,
    name: String,
    value: Option<String>,
    is_secret: bool,
    in_app_config: bool,
    values_match: bool,
    referenced: bool,
}

impl VarRow {
    fn safe_to_delete(&self) -> bool {
        !self.is_secret && self.in_app_config && self.values_match && !self.referenced
    }
}

#[derive(Props, Clone, PartialEq)]
pub struct VariableGroupPanelProps {
    pub az_config: AzConfig,
}

#[component]
pub fn VariableGroupPanel(props: VariableGroupPanelProps) -> Element {
    let az = props.az_config.clone();
    let configured = !az.devops_org.trim().is_empty() && !az.devops_project.trim().is_empty();

    let mut loading: Signal<bool> = use_signal(|| true);
    let mut error_msg: Signal<Option<String>> = use_signal(|| None);
    let mut rows: Signal<Vec<VarRow>> = use_signal(Vec::new);
    let mut selected: Signal<HashSet<(u64, String)>> = use_signal(HashSet::new);

    let mut deleting: Signal<bool> = use_signal(|| false);
    let mut delete_error: Signal<Option<String>> = use_signal(|| None);
    let mut confirm_delete: Signal<bool> = use_signal(|| false);

    let mut load = {
        let az = az.clone();
        move || {
            let az = az.clone();
            if az.devops_org.trim().is_empty() || az.devops_project.trim().is_empty() {
                loading.set(false);
                return;
            }
            loading.set(true);
            error_msg.set(None);
            spawn(async move {
                let org = az.devops_org.clone();
                let project = az.devops_project.clone();
                let sub = az.subscription.clone();
                let store = az.app_config_store.trim().to_string();
                let local_dir = az.local_dir.clone();

                let org2 = org.clone();
                let project2 = project.clone();
                let groups: Vec<VariableGroup> = match tokio::task::spawn_blocking(move || {
                    azure::list_variable_groups(&org2, &project2)
                })
                .await
                {
                    Ok(Ok(g)) => g,
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

                let app_config: HashMap<String, String> = if store.is_empty() {
                    HashMap::new()
                } else {
                    let sub2 = sub.clone();
                    let store2 = store.clone();
                    tokio::task::spawn_blocking(move || azure::appconfig_list_kv(&sub2, &store2))
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                        .unwrap_or_default()
                };

                let referenced: HashSet<String> = tokio::task::spawn_blocking(move || {
                    pipeline_scan::scan_variable_references(std::path::Path::new(&local_dir))
                })
                .await
                .unwrap_or_default();

                let mut out = Vec::new();
                for g in groups {
                    for v in g.variables {
                        let in_app_config = app_config.contains_key(&v.name);
                        let values_match = v
                            .value
                            .as_ref()
                            .zip(app_config.get(&v.name))
                            .map(|(a, b)| a == b)
                            .unwrap_or(false);
                        let is_referenced = referenced.contains(&v.name);
                        out.push(VarRow {
                            group_id: g.id,
                            group_name: g.name.clone(),
                            name: v.name,
                            value: v.value,
                            is_secret: v.is_secret,
                            in_app_config,
                            values_match,
                            referenced: is_referenced,
                        });
                    }
                }
                out.sort_by(|a, b| {
                    (a.group_name.clone(), a.name.clone())
                        .cmp(&(b.group_name.clone(), b.name.clone()))
                });
                rows.set(out);
                loading.set(false);
            });
        }
    };

    use_effect({
        let mut load = load.clone();
        move || load()
    });

    let do_delete = {
        let az = az.clone();
        let load = load.clone();
        move |_| {
            let az = az.clone();
            let mut load = load.clone();
            let targets: Vec<(u64, String)> = selected.read().iter().cloned().collect();
            deleting.set(true);
            delete_error.set(None);
            spawn(async move {
                let org = az.devops_org.clone();
                let project = az.devops_project.clone();
                for (group_id, name) in &targets {
                    let org2 = org.clone();
                    let project2 = project.clone();
                    let name2 = name.clone();
                    let group_id2 = *group_id;
                    let result = tokio::task::spawn_blocking(move || {
                        azure::delete_variable_group_variable(&org2, &project2, group_id2, &name2)
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("{e}")));
                    if let Err(e) = result {
                        crate::services::activity::error(
                            "Delete variable failed",
                            name.clone(),
                            e.clone(),
                        );
                        delete_error.set(Some(format!("{name}: {e}")));
                        deleting.set(false);
                        confirm_delete.set(false);
                        load();
                        return;
                    }
                    crate::services::activity::info(
                        "Deleted variable-group variable",
                        name.clone(),
                    );
                }
                selected.write().clear();
                deleting.set(false);
                confirm_delete.set(false);
                load();
            });
        }
    };

    let is_loading = *loading.read();
    let err = error_msg.read().clone();
    let all_rows = rows.read().clone();
    let selected_count = selected.read().len();
    let safe_count = all_rows.iter().filter(|r| r.safe_to_delete()).count();

    rsx! {
        div { class: "func-panel",
            div { class: "func-header",
                h2 { "Variable Group Cleanup" }
                if configured {
                    button {
                        class: "icon-refresh-btn",
                        title: "Refresh",
                        disabled: is_loading,
                        onclick: move |_| load(),
                        span { class: if is_loading { "icon-spin" } else { "" }, "⟳" }
                    }
                }
            }

            if !configured {
                div { class: "func-note",
                    "No Azure DevOps org/project set on this profile — edit the profile to add them. Requires the `azure-devops` CLI extension (`az extension add --name azure-devops`)."
                }
            } else {
                div { class: "func-note",
                    "\"Safe to delete\" means: present in App Configuration with a matching value, and not referenced as $(VarName) anywhere under the profile's local workspace path. Secrets are never auto-flagged since their values can't be compared."
                }

                if is_loading {
                    div { class: "func-loading", "Loading variable groups…" }
                } else if let Some(e) = err {
                    div { class: "az-error", "{e}" }
                } else if all_rows.is_empty() {
                    div { class: "func-empty", "No variable groups found in this project." }
                } else {
                    div { class: "func-summary",
                        span { class: "func-summary-item", "{all_rows.len()} variables across groups" }
                        span { class: "func-summary-item func-success", "{safe_count} safe to delete" }
                        if selected_count > 0 {
                            span { class: "func-summary-item", "{selected_count} selected" }
                            button {
                                class: "btn btn-small btn-primary",
                                onclick: move |_| confirm_delete.set(true),
                                "Delete selected…"
                            }
                        }
                    }

                    table { class: "func-table",
                        thead {
                            tr {
                                th { "" }
                                th { "Group" }
                                th { "Variable" }
                                th { "In App Config?" }
                                th { "Values match?" }
                                th { "Referenced in pipeline?" }
                            }
                        }
                        tbody {
                            for row in &all_rows {
                                {
                                    let key = (row.group_id, row.name.clone());
                                    let key2 = key.clone();
                                    let is_checked = selected.read().contains(&key);
                                    let can_select = row.safe_to_delete();
                                    rsx! {
                                        tr { class: "func-row",
                                            td {
                                                if can_select {
                                                    input {
                                                        r#type: "checkbox",
                                                        checked: is_checked,
                                                        onchange: move |e| {
                                                            let mut s = selected.write();
                                                            if e.checked() { s.insert(key2.clone()); } else { s.remove(&key2); }
                                                        },
                                                    }
                                                }
                                            }
                                            td { "{row.group_name}" }
                                            td { class: "func-name", "{row.name}" }
                                            td {
                                                if row.is_secret {
                                                    span { class: "func-no-data", "secret" }
                                                } else if row.in_app_config {
                                                    span { class: "func-badge-active", "yes" }
                                                } else {
                                                    span { class: "func-no-data", "no" }
                                                }
                                            }
                                            td {
                                                if row.is_secret {
                                                    span { class: "func-no-data", "—" }
                                                } else if row.values_match {
                                                    span { class: "func-badge-active", "yes" }
                                                } else {
                                                    span { class: "func-no-data", "no" }
                                                }
                                            }
                                            td {
                                                if row.referenced {
                                                    span { class: "func-errors has-errors", "yes" }
                                                } else {
                                                    span { class: "func-no-data", "no" }
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

            if *confirm_delete.read() {
                {
                    let targets: Vec<(u64, String)> = selected.read().iter().cloned().collect();
                    let running = *deleting.read();
                    let err = delete_error.read().clone();
                    let org = az.devops_org.clone();
                    let project = az.devops_project.clone();
                    rsx! {
                        div { class: "modal-backdrop",
                            onclick: move |_| if !running { confirm_delete.set(false); },
                            div { class: "modal-card",
                                onclick: move |e: Event<MouseData>| e.stop_propagation(),
                                h3 { class: "modal-title", "Delete {targets.len()} variable(s)?" }
                                p { class: "modal-body",
                                    "This runs, once per variable:"
                                    br {}
                                    code { "az pipelines variable-group variable delete --group-id <id> --name <name> --organization {org} --project {project} --yes" }
                                }
                                if let Some(e) = err {
                                    div { class: "az-error", "{e}" }
                                }
                                div { class: "modal-actions",
                                    button {
                                        class: "btn btn-small",
                                        disabled: running,
                                        onclick: move |_| confirm_delete.set(false),
                                        "Cancel"
                                    }
                                    button {
                                        class: "btn btn-small btn-primary",
                                        disabled: running,
                                        onclick: do_delete,
                                        if running { "Deleting…" } else { "Delete" }
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
