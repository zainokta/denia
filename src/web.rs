use axum::{
    body::Body,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::{EmbeddedFile, RustEmbed};

/// The built SPA client output, embedded into the binary in release builds and
/// read from disk in debug builds. Produced by `pnpm build` (TanStack Start in
/// SPA mode) before `cargo build`.
#[derive(RustEmbed)]
#[folder = "web/dist/client"]
struct WebAssets;

/// Prerendered SPA shell document. Served for `/` and as the fallback for
/// client-side routes.
const SHELL: &str = "_shell.html";

/// Serves the embedded web console for any route not handled by the API.
pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { SHELL } else { path };

    if let Some(file) = WebAssets::get(path) {
        return serve(file);
    }

    // A path that looks like a static asset (has a file extension in its last
    // segment) and was not found is a genuine 404. Everything else is treated
    // as a client-side route and resolves to the SPA shell.
    if last_segment_has_extension(path) {
        return StatusCode::NOT_FOUND.into_response();
    }

    match WebAssets::get(SHELL) {
        Some(file) => serve(file),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn serve(file: EmbeddedFile) -> Response {
    let mime = file.metadata.mimetype().to_string();
    (
        [(header::CONTENT_TYPE, mime)],
        Body::from(file.data.into_owned()),
    )
        .into_response()
}

fn last_segment_has_extension(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|segment| segment.contains('.'))
}
