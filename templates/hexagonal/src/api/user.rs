use super::{to_http_err, ErrorRes, Services};
use crate::application::UserService;
use crate::domain::User;
use axum::{
    extract::{FromRef, Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

pub fn routes() -> Router<Services> {
    Router::new()
        .route("/users", post(create_user))
        .route("/users/{id}", get(get_user))
}

#[derive(Clone)]
pub struct UserSvc(pub Arc<dyn UserService>);

impl FromRef<Services> for UserSvc {
    fn from_ref(s: &Services) -> Self {
        UserSvc(s.user.clone())
    }
}

#[derive(Deserialize)]
pub struct CreateUserReq {
    pub id: String,
    pub name: String,
}

pub async fn create_user(
    State(user_svc): State<UserSvc>,
    Json(req): Json<CreateUserReq>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<ErrorRes>)> {
    user_svc
        .0
        .create_user(req.id, req.name)
        .await
        .map_err(to_http_err)?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "ok": true }))))
}

pub async fn get_user(
    State(user_svc): State<UserSvc>,
    Path(id): Path<String>,
) -> Result<Json<User>, (StatusCode, Json<ErrorRes>)> {
    let user = user_svc.0.get_user(id).await.map_err(to_http_err)?;
    Ok(Json(user))
}
