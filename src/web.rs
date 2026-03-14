use crate::db::{Db, Priority, Status};
use axum::{
    extract::{Path, State},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Json, Response},
    routing::{get, patch},
    Router,
};
use rust_embed::Embed;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

type AppState = Arc<Mutex<Db>>;

#[derive(Embed)]
#[folder = "static/"]
struct Assets;

// ── Request types ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CreateTask {
    title: String,
    priority: Option<Priority>,
    parent_id: Option<i64>,
    tags: Option<Vec<String>>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct UpdateTask {
    title: Option<String>,
    status: Option<Status>,
    priority: Option<Priority>,
    tags: Option<Vec<String>>,
    description: Option<String>,
    parent_id: Option<Value>, // null = make root, number = reparent, absent = no change
}

// ── Handlers ────────────────────────────────────────────────────────────────

async fn list_tasks(State(db): State<AppState>) -> impl IntoResponse {
    let db = db.lock().unwrap();
    match db.all_tasks() {
        Ok(tasks) => Json(json!(tasks)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e.to_string()}).to_string()).into_response(),
    }
}

async fn create_task(State(db): State<AppState>, Json(body): Json<CreateTask>) -> impl IntoResponse {
    let db = db.lock().unwrap();
    let priority = body.priority.unwrap_or(Priority::Medium);
    let tags = body.tags.unwrap_or_default();
    let description = body.description.as_deref().unwrap_or("");
    match db.add_task(&body.title, priority, &tags, description, body.parent_id) {
        Ok(id) => (StatusCode::CREATED, Json(json!({"id": id}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e.to_string()}).to_string()).into_response(),
    }
}

async fn update_task(
    State(db): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateTask>,
) -> impl IntoResponse {
    let db = db.lock().unwrap();
    let tags_vec = body.tags;
    match db.update_task(
        id,
        body.title.as_deref(),
        body.status,
        body.priority,
        tags_vec.as_deref(),
        body.description.as_deref(),
    ) {
        Ok(true) => {
            // Handle parent_id reparenting if the field was present
            if let Some(ref parent_val) = body.parent_id {
                let new_parent = parent_val.as_i64(); // null → None, number → Some
                if let Err(e) = db.reparent_task(id, new_parent) {
                    return (StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e.to_string()}).to_string()).into_response();
                }
            }
            Json(json!({"updated": true})).into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, json!({"error": "task not found"}).to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e.to_string()}).to_string()).into_response(),
    }
}

async fn delete_task(State(db): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let db = db.lock().unwrap();
    match db.delete_task(id) {
        Ok(()) => Json(json!({"deleted": true})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e.to_string()}).to_string()).into_response(),
    }
}

// ── Static file serving ─────────────────────────────────────────────────────

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
        }
        None => {
            // SPA fallback: serve index.html for unknown routes
            match Assets::get("index.html") {
                Some(file) => ([(header::CONTENT_TYPE, "text/html")], file.data).into_response(),
                None => (StatusCode::NOT_FOUND, "not found").into_response(),
            }
        }
    }
}

// ── Entry point ─────────────────────────────────────────────────────────────

pub fn run() -> std::io::Result<()> {
    let db = Db::open().expect("failed to open database");
    let state: AppState = Arc::new(Mutex::new(db));

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let app = Router::new()
        .route("/api/tasks", get(list_tasks).post(create_task))
        .route("/api/tasks/{id}", patch(update_task).delete(delete_task))
        .fallback(static_handler)
        .with_state(state);

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
            .await
            .expect("failed to bind");
        eprintln!("Web UI running at http://localhost:{port}");
        axum::serve(listener, app).await.expect("server error");
    });

    Ok(())
}
