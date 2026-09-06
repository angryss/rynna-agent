use super::*;
use axum::extract::Query;
use rynna_core::{
    workflow_runs::{Control, Run, Start},
    workflows::{Workflow, WorkflowMetadata},
};
use uuid::Uuid;
// HTTP snapshots preserve progress metadata without publishing captured policies.
#[derive(Serialize)]
#[serde(transparent)]
pub(super) struct RunResponse(Run);
impl From<Run> for RunResponse {
    fn from(mut run: Run) -> Self {
        for step in &mut run.workflow.steps {
            step.instructions.clear();
        }
        for helper in &mut run.helpers {
            helper.instructions.clear();
        }
        Self(run)
    }
}
#[derive(Deserialize)]
pub(super) struct Scope {
    profile: String,
    session_id: Uuid,
}
pub(super) fn error(message: String) -> ApiError {
    ApiError {
        status: StatusCode::CONFLICT,
        code: "workflow_error",
        message,
    }
}
pub(super) async fn list(
    State(state): State<AppState>,
    AxumPath(profile): AxumPath<String>,
) -> Result<Json<Vec<WorkflowMetadata>>, ApiError> {
    Ok(Json(
        state
            .workflows
            .definitions(&profile)
            .await
            .map_err(error)?
            .iter()
            .map(Workflow::metadata)
            .collect(),
    ))
}
pub(super) async fn read(
    State(state): State<AppState>,
    AxumPath((profile, id)): AxumPath<(String, String)>,
    peer: LoopbackClient,
) -> Result<Json<Workflow>, ApiError> {
    ensure_loopback_admin(peer)?;
    Ok(Json(
        state
            .workflows
            .definitions(&profile)
            .await
            .map_err(error)?
            .into_iter()
            .find(|w| w.id == id)
            .ok_or_else(|| error("workflow unavailable".into()))?,
    ))
}
pub(super) async fn save(
    State(state): State<AppState>,
    AxumPath(profile): AxumPath<String>,
    peer: LoopbackClient,
    Json(workflow): Json<Workflow>,
) -> Result<Json<Workflow>, ApiError> {
    ensure_loopback_admin(peer)?;
    Ok(Json(
        state
            .workflows
            .save(&profile, workflow)
            .await
            .map_err(error)?,
    ))
}
pub(super) async fn delete(
    State(state): State<AppState>,
    AxumPath((profile, id)): AxumPath<(String, String)>,
    peer: LoopbackClient,
) -> Result<StatusCode, ApiError> {
    ensure_loopback_admin(peer)?;
    state.workflows.delete(&profile, &id).await.map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn start(
    State(state): State<AppState>,
    Json(request): Json<Start>,
) -> Result<Json<RunResponse>, ApiError> {
    Ok(Json(
        state.workflows.start(request).await.map_err(error)?.into(),
    ))
}
pub(super) async fn runs(
    State(state): State<AppState>,
    Query(scope): Query<Scope>,
) -> Result<Json<Vec<RunResponse>>, ApiError> {
    Ok(Json(
        state
            .workflows
            .list(&scope.profile, scope.session_id)
            .await
            .map_err(error)?
            .into_iter()
            .map(RunResponse::from)
            .collect(),
    ))
}
pub(super) async fn run(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<Uuid>,
    Query(scope): Query<Scope>,
) -> Result<Json<RunResponse>, ApiError> {
    Ok(Json(
        state
            .workflows
            .read(id, &scope.profile, scope.session_id)
            .await
            .map_err(error)?
            .into(),
    ))
}
pub(super) async fn control(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<Uuid>,
    Json(request): Json<Control>,
) -> Result<Json<RunResponse>, ApiError> {
    Ok(Json(
        state
            .workflows
            .control(id, request)
            .await
            .map_err(error)?
            .into(),
    ))
}
