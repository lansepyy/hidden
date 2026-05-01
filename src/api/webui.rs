use axum::response::Html;

static INDEX_HTML: &str = include_str!("../../static/index.html");

/// GET /  —  返回嵌入的 WebUI 单页面
pub async fn serve_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}
