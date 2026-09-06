#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChainDetail {
    pub label: String,
    pub steps: Vec<StepDetail>,
    pub queues: Vec<String>,
    pub parallel_entries: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StepDetail {
    pub workflow: String,
    pub link_type: String,
    pub trigger_info: String,
}
