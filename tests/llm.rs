//! Exercises the URL importer end to end against a local fake store and fake LLM APIs,
//! checking both the requests we send and how replies are merged.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use wishful_thinking::importer::llm::{LlmConfig, LlmMode, ProviderKind};
use wishful_thinking::importer::Importer;

type Captured = Arc<Mutex<Vec<(axum::http::HeaderMap, Value)>>>;

const SPARSE_PAGE: &str = r#"<html><head><title>Shop | Item 4471</title></head><body>
  <nav>Home Toys Sale</nav>
  <div class="pdp"><h2>Wooden Rainbow Stacker, 7 pieces</h2>
  <img src="/images/rainbow-large.jpg" alt="Rainbow stacker"><img src="/images/logo.png" alt="logo">
  <p>Our price: <strong>$38.50</strong></p><p>Hand-finished beech wood, ages 1+.</p></div>
</body></html>"#;

const STRUCTURED_PAGE: &str = r#"<html><head>
  <script type="application/ld+json">{"@type":"Product","name":"Kite","image":"/kite.jpg","offers":{"price":"12.00","priceCurrency":"USD"}}</script>
</head><body>Kite</body></html>"#;

async fn anthropic(
    State(c): State<Captured>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    c.lock().unwrap().push((headers, body));
    Json(json!({
        "id": "msg_test", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
        "content": [
            {"type": "thinking", "thinking": "", "signature": "x"},
            {"type": "text", "text": "{\"title\":\"Wooden Rainbow Stacker (7 pieces)\",\"price\":\"38.50\",\"currency\":\"USD\",\"image_url\":\"/images/rainbow-large.jpg\",\"store\":\"Tiny Toy Shop\",\"description\":\"A beech-wood rainbow stacking toy.\"}"}
        ],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 100, "output_tokens": 50}
    }))
}

async fn openai(
    State(c): State<Captured>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    c.lock().unwrap().push((headers, body));
    Json(json!({
        "choices": [{"message": {"role": "assistant", "content": "```json\n{\"title\":\"Wooden Rainbow Stacker\",\"price\":\"38.5\",\"currency\":\"USD\",\"image_url\":\"http://ignored.example/x.jpg\",\"store\":\"\",\"description\":\"\"}\n```"}}]
    }))
}

async fn start() -> (String, Captured) {
    let captured: Captured = Arc::default();
    let app = Router::new()
        .route("/sparse", get(|| async { Html(SPARSE_PAGE) }))
        .route("/structured", get(|| async { Html(STRUCTURED_PAGE) }))
        .route("/v1/messages", post(anthropic))
        .route("/openai/chat/completions", post(openai))
        .with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), captured)
}

fn importer(kind: ProviderKind, base: &str, model: &str, mode: LlmMode) -> Importer {
    let base_url = match kind {
        ProviderKind::Anthropic => base.to_string(),
        ProviderKind::OpenAiCompatible => format!("{base}/openai"),
    };
    let cfg = LlmConfig {
        kind,
        api_key: Some("test-key".into()),
        model: model.into(),
        base_url,
    };
    Importer::new(Some(cfg.build()), mode, true)
}

#[tokio::test]
async fn anthropic_fills_gaps_on_sparse_page() {
    let (base, captured) = start().await;
    let imp = importer(
        ProviderKind::Anthropic,
        &base,
        "claude-opus-5-5",
        LlmMode::Fallback,
    );
    let out = imp.import(&format!("{base}/sparse")).await.unwrap();

    assert_eq!(
        out.info.title.as_deref(),
        Some("Wooden Rainbow Stacker (7 pieces)")
    );
    assert_eq!(out.info.price_cents, Some(3850));
    assert_eq!(out.info.currency.as_deref(), Some("USD"));
    assert_eq!(
        out.info.image_url.as_deref(),
        Some(format!("{base}/images/rainbow-large.jpg").as_str())
    );
    assert_eq!(out.info.store.as_deref(), Some("Tiny Toy Shop"));
    assert_eq!(out.llm_used.as_deref(), Some("anthropic/claude-opus-5-5"));

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let (headers, body) = &reqs[0];
    assert_eq!(headers["x-api-key"], "test-key");
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(body["output_config"]["effort"], "low");
    assert!(body.get("thinking").is_none() && body.get("temperature").is_none());
    // Fallbacks are only sent to the first-party API.
    assert!(body.get("fallbacks").is_none());
    let prompt = body["messages"][0]["content"].as_str().unwrap();
    assert!(prompt.contains("$38.50") && prompt.contains("rainbow-large.jpg"));
}

#[tokio::test]
async fn structured_data_skips_llm_in_fallback_mode() {
    let (base, captured) = start().await;
    let imp = importer(
        ProviderKind::Anthropic,
        &base,
        "claude-opus-5-5",
        LlmMode::Fallback,
    );
    let out = imp.import(&format!("{base}/structured")).await.unwrap();
    assert_eq!(out.info.title.as_deref(), Some("Kite"));
    assert_eq!(out.info.price_cents, Some(1200));
    assert!(out.llm_used.is_none());
    assert!(captured.lock().unwrap().is_empty());
}

#[tokio::test]
async fn openai_compatible_provider() {
    let (base, captured) = start().await;
    let imp = importer(
        ProviderKind::OpenAiCompatible,
        &base,
        "llama3.2",
        LlmMode::Always,
    );
    let out = imp.import(&format!("{base}/sparse")).await.unwrap();
    assert_eq!(out.info.title.as_deref(), Some("Wooden Rainbow Stacker"));
    assert_eq!(out.info.price_cents, Some(3850));
    assert_eq!(out.llm_used.as_deref(), Some("openai-compatible/llama3.2"));
    // The model's image isn't on the page, so it is ignored.
    assert_ne!(
        out.info.image_url.as_deref(),
        Some("http://ignored.example/x.jpg")
    );

    let reqs = captured.lock().unwrap();
    let (headers, body) = &reqs[0];
    assert_eq!(headers["authorization"], "Bearer test-key");
    assert_eq!(body["model"], "llama3.2");
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["messages"][0]["role"], "system");
}

#[tokio::test]
async fn llm_failure_is_a_warning_not_an_error() {
    let (base, _) = start().await;
    // Point the provider at a path that doesn't exist.
    let cfg = LlmConfig {
        kind: ProviderKind::Anthropic,
        api_key: Some("k".into()),
        model: "claude-opus-5-5".into(),
        base_url: format!("{base}/nope"),
    };
    let imp = Importer::new(Some(cfg.build()), LlmMode::Always, true);
    let out = imp.import(&format!("{base}/sparse")).await.unwrap();
    assert!(out.llm_used.is_none());
    assert!(out.warnings.iter().any(|w| w.contains("AI assistant")));
    // Heuristics still produced something useful.
    assert!(out.info.title.is_some());
}
