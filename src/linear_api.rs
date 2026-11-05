use crate::model::{IssueId, LinearIssue, Priority, StateType};
use crate::session::Session;
use crate::settings::QueryLookup;
use reqwest::{header::AUTHORIZATION, Client, StatusCode};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

pub const GRAPHQL_URL: &str = "https://api.linear.app/graphql";
/// Linear's documented default page size.
const PAGE_SIZE: usize = 50;
const MAX_PAGES: usize = 100;
/// Keeps every task description well inside one 64 KiB RPC frame.
const MAX_DESCRIPTION_BYTES: usize = 24 * 1024;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ApiError {
    #[error("Linear is not connected")]
    NotConnected,
    #[error("Linear rejected the credentials; reconnect to Linear")]
    Unauthorized,
    #[error("{0}")]
    Reconnect(String),
    #[error("Linear rate limit reached; sync will retry later")]
    RateLimited,
    #[error("Linear request failed: {0}")]
    Request(String),
    #[error("Linear GraphQL error: {0}")]
    GraphQl(String),
    #[error("invalid Linear response: {0}")]
    Decode(String),
    #[error("Linear query exceeded the 5,000-issue synchronization limit")]
    TooManyIssues,
}

/// Sends one GraphQL operation and parses `data` into `T`.
pub async fn graphql<T: DeserializeOwned>(
    http: &Client,
    url: &str,
    authorization: &str,
    query: &str,
    variables: Value,
) -> Result<T, ApiError> {
    let response = http
        .post(url)
        .header(AUTHORIZATION, authorization)
        .json(&json!({ "query": query, "variables": variables }))
        .send()
        .await
        .map_err(|error| ApiError::Request(error.without_url().to_string()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| ApiError::Request(error.without_url().to_string()))?;
    let envelope: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let errors = envelope
        .get("errors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let has_code = |code: &str| {
        errors
            .iter()
            .any(|error| error["extensions"]["code"].as_str() == Some(code))
    };
    if status == StatusCode::UNAUTHORIZED || has_code("AUTHENTICATION_ERROR") {
        return Err(ApiError::Unauthorized);
    }
    if status == StatusCode::TOO_MANY_REQUESTS || has_code("RATELIMITED") {
        return Err(ApiError::RateLimited);
    }
    if !errors.is_empty() {
        let messages: Vec<_> = errors
            .iter()
            .map(|error| error["message"].as_str().unwrap_or("unknown error"))
            .collect();
        return Err(ApiError::GraphQl(messages.join("; ")));
    }
    if !status.is_success() {
        return Err(ApiError::Request(format!("HTTP {status}")));
    }
    serde_json::from_value(envelope.get("data").cloned().unwrap_or(Value::Null))
        .map_err(|error| ApiError::Decode(error.to_string()))
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Organization {
    pub id: String,
    pub name: String,
    pub url_key: String,
}

const VIEWER: &str = "query Viewer { viewer { organization { id name urlKey } } }";

/// Validates an authorization by asking which workspace it belongs to.
pub async fn viewer_organization(
    http: &Client,
    url: &str,
    authorization: &str,
) -> Result<Organization, ApiError> {
    #[derive(Deserialize)]
    struct Data {
        viewer: Viewer,
    }
    #[derive(Deserialize)]
    struct Viewer {
        organization: Organization,
    }
    graphql::<Data>(http, url, authorization, VIEWER, json!({}))
        .await
        .map(|data| data.viewer.organization)
}

const ISSUE_FIELDS: &str = "id identifier title description priority url state { name type } labels { nodes { name } } team { id }";

fn filter_query() -> String {
    format!("query FilteredIssues($filter: IssueFilter, $first: Int, $after: String) {{ issues(filter: $filter, first: $first, after: $after) {{ nodes {{ {ISSUE_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }}")
}

fn view_query() -> String {
    format!("query ViewIssues($id: String!, $first: Int, $after: String) {{ customView(id: $id) {{ issues(first: $first, after: $after) {{ nodes {{ {ISSUE_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }} }}")
}

#[derive(Deserialize)]
struct Connection<T> {
    nodes: Vec<T>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Deserialize)]
struct PageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
struct RawIssue {
    id: String,
    identifier: String,
    title: String,
    description: Option<String>,
    priority: f64,
    url: String,
    state: RawState,
    labels: Nodes<RawLabel>,
    team: RawTeam,
}

#[derive(Deserialize)]
struct RawState {
    name: String,
    #[serde(rename = "type")]
    kind: StateType,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
}

#[derive(Deserialize)]
struct RawTeam {
    id: String,
}

impl RawIssue {
    fn parse(self) -> Result<LinearIssue, ApiError> {
        let priority = Priority::from_linear(self.priority).ok_or_else(|| {
            ApiError::Decode(format!(
                "issue {} has unknown priority {}",
                self.identifier, self.priority
            ))
        })?;
        let mut labels: Vec<_> = self
            .labels
            .nodes
            .into_iter()
            .map(|label| label.name)
            .collect();
        labels.sort();
        Ok(LinearIssue {
            id: IssueId(self.id),
            identifier: self.identifier,
            title: self.title,
            description: truncate(self.description.unwrap_or_default()),
            url: self.url,
            state_name: self.state.name,
            state_type: self.state.kind,
            priority,
            labels,
            team_id: self.team.id,
        })
    }
}

fn truncate(mut text: String) -> String {
    if text.len() <= MAX_DESCRIPTION_BYTES {
        return text;
    }
    let mut end = MAX_DESCRIPTION_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("\n\n[Truncated by PlaneAI; open the issue in Linear for the full description.]");
    text
}

/// Every issue a source returns, in pages of 50, refusing more than 5,000.
pub async fn fetch_issues(
    session: &mut Session<'_>,
    lookup: &QueryLookup,
) -> Result<Vec<LinearIssue>, ApiError> {
    #[derive(Deserialize)]
    struct FilterData {
        issues: Connection<RawIssue>,
    }
    #[derive(Deserialize)]
    struct ViewData {
        #[serde(rename = "customView")]
        custom_view: ViewIssues,
    }
    #[derive(Deserialize)]
    struct ViewIssues {
        issues: Connection<RawIssue>,
    }
    let mut issues = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let page = match lookup {
            QueryLookup::Filter { filter } => {
                session
                    .execute::<FilterData>(
                        &filter_query(),
                        json!({ "filter": filter, "first": PAGE_SIZE, "after": after }),
                    )
                    .await?
                    .issues
            }
            QueryLookup::View { id } => {
                session
                    .execute::<ViewData>(
                        &view_query(),
                        json!({ "id": id, "first": PAGE_SIZE, "after": after }),
                    )
                    .await?
                    .custom_view
                    .issues
            }
        };
        for issue in page.nodes {
            issues.push(issue.parse()?);
        }
        match (page.page_info.has_next_page, page.page_info.end_cursor) {
            (true, Some(cursor)) => after = Some(cursor),
            (true, None) => return Err(ApiError::Decode("next page without a cursor".into())),
            (false, _) => return Ok(issues),
        }
    }
    Err(ApiError::TooManyIssues)
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
}

const TEAM_STATES: &str =
    "query IssueTeamStates($id: String!) { issue(id: $id) { team { states { nodes { id name } } } } }";

/// The workflow states of the issue's current team.
pub async fn team_states(
    session: &mut Session<'_>,
    issue_id: &IssueId,
) -> Result<Vec<WorkflowState>, ApiError> {
    #[derive(Deserialize)]
    struct Data {
        issue: Issue,
    }
    #[derive(Deserialize)]
    struct Issue {
        team: Team,
    }
    #[derive(Deserialize)]
    struct Team {
        states: Nodes<WorkflowState>,
    }
    session
        .execute::<Data>(TEAM_STATES, json!({ "id": issue_id }))
        .await
        .map(|data| data.issue.team.states.nodes)
}

const ISSUE_UPDATE: &str = "mutation MoveIssue($id: String!, $stateId: String!) { issueUpdate(id: $id, input: { stateId: $stateId }) { success } }";
const COMMENT_CREATE: &str = "mutation CommentOnIssue($issueId: String!, $body: String!) { commentCreate(input: { issueId: $issueId, body: $body }) { success } }";

pub async fn set_issue_state(
    session: &mut Session<'_>,
    issue_id: &IssueId,
    state_id: &str,
) -> Result<(), ApiError> {
    let data: Value = session
        .execute(ISSUE_UPDATE, json!({ "id": issue_id, "stateId": state_id }))
        .await?;
    succeeded(&data, "issueUpdate")
}

pub async fn create_comment(
    session: &mut Session<'_>,
    issue_id: &IssueId,
    body: &str,
) -> Result<(), ApiError> {
    let data: Value = session
        .execute(COMMENT_CREATE, json!({ "issueId": issue_id, "body": body }))
        .await?;
    succeeded(&data, "commentCreate")
}

fn succeeded(data: &Value, mutation: &str) -> Result<(), ApiError> {
    match data[mutation]["success"].as_bool() {
        Some(true) => Ok(()),
        _ => Err(ApiError::GraphQl(format!("{mutation} did not succeed"))),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn raw_issue(id: &str, identifier: &str, state: (&str, &str), priority: f64) -> Value {
        json!({
            "id": id, "identifier": identifier, "title": format!("Title {identifier}"),
            "description": format!("Description {identifier}"), "priority": priority,
            "url": format!("https://linear.app/acme/issue/{identifier}"),
            "state": { "name": state.0, "type": state.1 },
            "labels": { "nodes": [{ "name": "zeta" }, { "name": "alpha" }] },
            "team": { "id": "team-1" }
        })
    }

    #[test]
    fn issues_parse_into_typed_fields() {
        let raw: RawIssue =
            serde_json::from_value(raw_issue("uuid-1", "ENG-1", ("In QA", "started"), 1.0))
                .unwrap();
        assert_eq!(
            raw.parse().unwrap(),
            LinearIssue {
                id: IssueId("uuid-1".into()),
                identifier: "ENG-1".into(),
                title: "Title ENG-1".into(),
                description: "Description ENG-1".into(),
                url: "https://linear.app/acme/issue/ENG-1".into(),
                state_name: "In QA".into(),
                state_type: StateType::Started,
                priority: Priority::Urgent,
                labels: vec!["alpha".into(), "zeta".into()],
                team_id: "team-1".into(),
            }
        );
    }

    #[test]
    fn a_null_description_is_empty_and_a_huge_one_is_truncated() {
        let mut raw = raw_issue("u", "ENG-2", ("Todo", "unstarted"), 0.0);
        raw["description"] = Value::Null;
        let parsed = serde_json::from_value::<RawIssue>(raw)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(parsed.description, "");

        let long = "é".repeat(MAX_DESCRIPTION_BYTES);
        let truncated = truncate(long);
        assert!(truncated.starts_with(&"é".repeat(MAX_DESCRIPTION_BYTES / 2)));
        assert!(truncated.ends_with("open the issue in Linear for the full description.]"));
        assert!(truncated.len() < MAX_DESCRIPTION_BYTES + 100);
    }
}
