#[derive(Clone, Debug, PartialEq)]
pub struct ChainDetail {
    pub label: String,
    pub steps: Vec<StepDetail>,
    pub queues: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepDetail {
    pub workflow: String,
    pub link_type: String,
    pub trigger_info: String,
}
