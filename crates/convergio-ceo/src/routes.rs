//! HTTP API routes for convergio-ceo.

use axum::Router;

/// Returns the router for this crate's API endpoints.
pub fn routes() -> Router {
    Router::new()
    // .route("/api/ceo/health", get(health))
}
