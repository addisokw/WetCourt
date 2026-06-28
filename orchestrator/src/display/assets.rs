use axum::{
    body::Body,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "frontend/dist/"]
struct Assets;

const FALLBACK_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Wet Court (dev)</title></head>
<body style="font-family:monospace;background:#111;color:#ccc;padding:2em">
<h1>Wet Court of Appeals — orchestrator</h1>
<p>The frontend bundle hasn't been built yet.</p>
<p>Run <code>cd frontend &amp;&amp; npm install &amp;&amp; npm run build</code>,
then restart the orchestrator. Or run <code>npm run dev</code> for HMR at
<code>http://localhost:5173</code>.</p>
<p>WebSocket is live at <code>ws://localhost:8080/ws</code>.</p>
<p>Operator endpoints: <code>POST /operator/start</code>, <code>POST /operator/estop</code>.</p>
</body></html>"#;

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(file) = Assets::get(path) {
        // mime_guess doesn't know `.webmanifest`; serve it as the spec type so
        // the browser accepts the PWA manifest.
        let mime = if path.ends_with(".webmanifest") {
            "application/manifest+json".to_string()
        } else {
            mime_guess::from_path(path)
                .first_or_octet_stream()
                .as_ref()
                .to_string()
        };
        return Response::builder()
            .header(header::CONTENT_TYPE, mime)
            .body(Body::from(file.data.into_owned()))
            .unwrap();
    }
    if let Some(file) = Assets::get("index.html") {
        return Response::builder()
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(file.data.into_owned()))
            .unwrap();
    }
    if path == "index.html" || !path.contains('.') {
        return Response::builder()
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(FALLBACK_HTML))
            .unwrap();
    }
    StatusCode::NOT_FOUND.into_response()
}
