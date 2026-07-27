use super::{to_http_err, ErrorRes, Services};
use crate::domain::Greeter;
use axum::{
    extract::{FromRef, Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use std::sync::Arc;

pub fn routes() -> Router<Services> {
    Router::new().route("/hello/{name}", get(say_hello))
}

#[derive(Clone)]
pub struct GreeterSvc(pub Arc<dyn Greeter>);

impl FromRef<Services> for GreeterSvc {
    fn from_ref(s: &Services) -> Self {
        GreeterSvc(s.greeter.clone())
    }
}

pub async fn say_hello(
    State(greeter): State<GreeterSvc>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorRes>)> {
    let message = greeter.0.say_hello(name).await.map_err(to_http_err)?;
    Ok(Json(serde_json::json!({ "message": message })))
}
