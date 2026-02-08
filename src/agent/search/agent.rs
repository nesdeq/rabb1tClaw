use crate::agent::stream::collect_stream;
use crate::agent::runner::{resolve_agent_model, ResolvedAgentModel};
use crate::agent::tracker::truncate;
use crate::config::native::{
    AgentKind, SEARCH_ANALYZE_PROMPT, SEARCH_EVALUATE_PROMPT, SEARCH_SYNTHESIZE_PROMPT,
};
use crate::provider::{get_shared_client, ChatMessage};
use crate::state::GatewayState;
use std::fmt::Write;
use std::sync::Arc;
use tracing::{info, warn};

use super::tracker::{SearchQueryStatus, SearchQueryTracker};

// ============================================================================
// Serper Types
// ============================================================================

/// Serper.dev organic/news search result.
#[derive(serde::Deserialize, Debug)]
struct SerperOrganic {
    title: Option<String>,
    link: Option<String>,
    snippet: Option<String>,
    date: Option<String>,
}

/// Serper.dev knowledge graph result.
#[derive(serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SerperKnowledgeGraph {
    title: Option<String>,
    description: Option<String>,
}

/// Serper.dev "People Also Ask" entry.
#[derive(serde::Deserialize, Debug)]
struct SerperPeopleAlsoAsk {
    question: Option<String>,
    snippet: Option<String>,
}

/// Serper.dev search response.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct SerperResponse {
    #[serde(default)]
    organic: Vec<SerperOrganic>,
    #[serde(default)]
    news: Vec<SerperOrganic>,
    knowledge_graph: Option<SerperKnowledgeGraph>,
    #[serde(default)]
    people_also_ask: Vec<SerperPeopleAlsoAsk>,
}

/// Phase 1 output: refined query with Serper parameters.
#[derive(serde::Deserialize, Debug)]
struct AnalyzedQuery {
    q: String,
    #[serde(default = "default_search_type")]
    r#type: String,
    #[serde(default)]
    gl: Option<String>,
    #[serde(default)]
    hl: Option<String>,
    #[serde(default)]
    tbs: Option<String>,
    #[serde(default)]
    location: Option<String>,
}

fn default_search_type() -> String {
    "search".into()
}

#[derive(serde::Deserialize, Debug)]
struct AnalyzeOutput {
    queries: Vec<AnalyzedQuery>,
}

/// Phase 2 output: evaluation verdict.
#[derive(serde::Deserialize, Debug)]
struct EvaluateOutput {
    verdict: String,
    #[serde(default)]
    results: Vec<EvalResult>,
    #[serde(default)]
    urls: Vec<String>,
    #[serde(default)]
    partial_results: Vec<EvalResult>,
}

#[derive(serde::Deserialize, Debug)]
struct EvalResult {
    title: Option<String>,
    url: Option<String>,
    snippet: Option<String>,
    date: Option<String>,
}

// ============================================================================
// Public API
// ============================================================================

/// Bundled search operational limits (replaces positional params).
#[derive(Debug, Clone, Copy)]
pub struct SearchLimits {
    pub max_results: usize,
    pub max_news: usize,
    pub max_people_also_ask: usize,
    pub max_total_tokens: usize,
    pub max_deep_read_urls: usize,
    pub max_page_tokens: usize,
    pub fetch_timeout_secs: u64,
}

/// Run a web search with 3-phase LLM pipeline.
pub async fn run_search(
    state: Arc<GatewayState>,
    tracker: Arc<SearchQueryTracker>,
    prefix: String,
    query_id: String,
    query: String,
    limits: SearchLimits,
) {
    info!("Search agent started: [{}] {}", query_id, query);

    let status = match run_inner(&state, &query, &limits).await {
        Ok(context) => {
            info!("Search agent [{}] completed ({} chars)", query_id, context.len());
            SearchQueryStatus::Completed { context }
        }
        Err(e) => {
            warn!("Search agent [{}] failed: {}", query_id, e);
            SearchQueryStatus::Failed { error: e.to_string() }
        }
    };

    tracker.complete(&prefix, &query_id, status).await;
}

// ============================================================================
// Internal Pipeline
// ============================================================================

async fn run_inner(
    state: &GatewayState,
    query: &str,
    limits: &SearchLimits,
) -> Result<String, anyhow::Error> {
    let api_key = std::env::var("SERP_API_KEY")
        .map_err(|_| anyhow::anyhow!("SERP_API_KEY not set"))?;

    // Resolve search agent model (if configured — LLM phases are optional)
    let resolved = resolve_agent_model(state, AgentKind::Search).await;

    // ── Phase 1: Query Analysis ──
    let analyzed_queries = match &resolved {
        Some(r) => phase1_analyze(r, query).await.unwrap_or_else(|e| {
            warn!("Phase 1 (analyze) failed, using raw query: {}", e);
            vec![AnalyzedQuery {
                q: query.to_string(),
                r#type: "search".into(),
                gl: None, hl: None, tbs: None, location: None,
            }]
        }),
        None => vec![AnalyzedQuery {
            q: query.to_string(),
            r#type: "search".into(),
            gl: None, hl: None, tbs: None, location: None,
        }],
    };

    // ── Phase 2a: Fetch from Serper ──
    let client = get_shared_client();
    let mut all_responses = Vec::new();

    // Fire all Serper requests in parallel
    let mut futs = Vec::new();
    for aq in &analyzed_queries {
        let num = if aq.r#type == "news" { limits.max_news } else { limits.max_results };
        futs.push(fetch_serper(client, &api_key, aq, num));
    }
    let results = futures::future::join_all(futs).await;
    for res in results {
        match res {
            Ok(resp) => all_responses.push(resp),
            Err(e) => warn!("Serper request failed: {}", e),
        }
    }

    if all_responses.is_empty() {
        return Err(anyhow::anyhow!("all Serper requests failed"));
    }

    let results_text = format_serper_results(&all_responses, limits.max_results, limits.max_news, limits.max_people_also_ask);

    if results_text.trim().is_empty() {
        return Err(anyhow::anyhow!("no search results found"));
    }

    // ── Phase 2b: Evaluate results with LLM ──
    let final_text = match &resolved {
        Some(r) => {
            let eval_input = format!(
                "## Original Query\n\n{}\n\n## Max URLs for deep reading: {}\n\n## Search Results\n\n{}",
                query, limits.max_deep_read_urls, results_text
            );
            match phase2_evaluate(r, &eval_input).await {
                Ok(eval) => {
                    if eval.verdict == "need_deep_read" && !eval.urls.is_empty() {
                        // ── Phase 3: Deep Read + Synthesize ──
                        let urls: Vec<&str> = eval.urls.iter()
                            .take(limits.max_deep_read_urls)
                            .map(|s| s.as_str())
                            .collect();
                        match phase3_synthesize(r, query, &results_text, &urls, limits).await {
                            Ok(text) => text,
                            Err(e) => {
                                warn!("Phase 3 (synthesize) failed, using evaluation results: {}", e);
                                format_eval_results(&eval.partial_results)
                            }
                        }
                    } else {
                        format_eval_results(&eval.results)
                    }
                }
                Err(e) => {
                    warn!("Phase 2 (evaluate) failed, using raw results: {}", e);
                    results_text
                }
            }
        }
        None => results_text,
    };

    Ok(truncate(&final_text, limits.max_total_tokens))
}

// ============================================================================
// Phase 1: Query Analysis
// ============================================================================

async fn phase1_analyze(
    resolved: &ResolvedAgentModel,
    query: &str,
) -> Result<Vec<AnalyzedQuery>, anyhow::Error> {
    let response = call_llm(resolved, SEARCH_ANALYZE_PROMPT, query).await?;
    let json_str = extract_json(&response);
    let output: AnalyzeOutput = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Phase 1 JSON parse error: {} — raw: {}", e, response))?;

    if output.queries.is_empty() {
        return Err(anyhow::anyhow!("Phase 1 returned empty queries"));
    }

    info!("Phase 1: {} refined queries", output.queries.len());
    Ok(output.queries)
}

// ============================================================================
// Phase 2: Evaluate
// ============================================================================

async fn phase2_evaluate(
    resolved: &ResolvedAgentModel,
    eval_input: &str,
) -> Result<EvaluateOutput, anyhow::Error> {
    let response = call_llm(resolved, SEARCH_EVALUATE_PROMPT, eval_input).await?;
    let json_str = extract_json(&response);
    let output: EvaluateOutput = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Phase 2 JSON parse error: {} — raw: {}", e, response))?;

    info!("Phase 2 verdict: {}", output.verdict);
    Ok(output)
}

// ============================================================================
// Phase 3: Deep Read + Synthesize
// ============================================================================

async fn phase3_synthesize(
    resolved: &ResolvedAgentModel,
    query: &str,
    serper_text: &str,
    urls: &[&str],
    limits: &SearchLimits,
) -> Result<String, anyhow::Error> {
    // Fetch and extract page content in parallel
    let client = get_shared_client();
    let timeout_secs = limits.fetch_timeout_secs;
    let mut futs = Vec::new();
    for &url in urls {
        futs.push(fetch_and_extract(client, url, timeout_secs));
    }
    let results = futures::future::join_all(futs).await;

    let mut pages_text = String::new();
    for (i, res) in results.into_iter().enumerate() {
        let url = urls[i];
        match res {
            Ok(content) => {
                let trimmed = truncate(&content, limits.max_page_tokens);
                let _ = writeln!(pages_text, "--- Page: {} ---\n{}\n", url, trimmed);
            }
            Err(e) => {
                warn!("Failed to fetch {}: {}", url, e);
                let _ = writeln!(pages_text, "--- Page: {} ---\n(fetch failed: {})\n", url, e);
            }
        }
    }

    let synth_input = format!(
        "## Original Query\n\n{}\n\n## Search Snippets\n\n{}\n\n## Full Page Content\n\n{}",
        query, serper_text, pages_text
    );

    let response = call_llm(resolved, SEARCH_SYNTHESIZE_PROMPT, &synth_input).await?;
    info!("Phase 3: synthesized {} chars", response.len());
    Ok(response)
}

// ============================================================================
// Helpers
// ============================================================================

/// One-shot LLM call: system prompt + single user message → collected response.
async fn call_llm(
    resolved: &ResolvedAgentModel,
    system_prompt: &str,
    user_content: &str,
) -> Result<String, anyhow::Error> {
    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: user_content.to_string(),
    }];
    let request = resolved.chat_request(messages, Some(system_prompt.to_string()));
    let rx = resolved.provider.chat_stream(request).await?;
    collect_stream(rx).await
}

/// Call Serper API with full params (gl, hl, tbs, type).
async fn fetch_serper(
    client: &reqwest::Client,
    api_key: &str,
    query: &AnalyzedQuery,
    num: usize,
) -> Result<SerperResponse, anyhow::Error> {
    let endpoint = match query.r#type.as_str() {
        "news" => "https://google.serper.dev/news",
        _ => "https://google.serper.dev/search",
    };

    let mut body = serde_json::json!({ "q": query.q, "num": num });
    let obj = body.as_object_mut().unwrap();
    if let Some(ref gl) = query.gl {
        obj.insert("gl".into(), serde_json::Value::String(gl.clone()));
    }
    if let Some(ref hl) = query.hl {
        obj.insert("hl".into(), serde_json::Value::String(hl.clone()));
    }
    if let Some(ref tbs) = query.tbs {
        obj.insert("tbs".into(), serde_json::Value::String(tbs.clone()));
    }
    if let Some(ref location) = query.location {
        obj.insert("location".into(), serde_json::Value::String(location.clone()));
    }

    let resp = client
        .post(endpoint)
        .header("X-API-KEY", api_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json::<SerperResponse>()
        .await?;

    Ok(resp)
}

/// Fetch a URL and extract main content text via trafilatura.
async fn fetch_and_extract(
    client: &reqwest::Client,
    url: &str,
    timeout_secs: u64,
) -> Result<String, anyhow::Error> {
    let resp = client
        .get(url)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36")
        .send()
        .await?
        .error_for_status()?;

    let html = resp.text().await?;

    // Extract content using trafilatura (blocking — run on spawn_blocking)
    let result = tokio::task::spawn_blocking(move || {
        rs_trafilatura::extract(&html)
    }).await??;

    let text = result.content_text;
    if text.trim().is_empty() {
        return Err(anyhow::anyhow!("trafilatura extracted no content"));
    }
    Ok(text)
}

/// Write a single search result line with optional date.
fn format_result_line(out: &mut String, title: &str, url: &str, snippet: &str, date: &str) {
    if date.is_empty() {
        let _ = writeln!(out, "- **{}** ({})\n  {}", title, url, snippet);
    } else {
        let _ = writeln!(out, "- **{}** ({}) [{}]\n  {}", title, url, date, snippet);
    }
}

/// Format all Serper responses into readable text for LLM evaluation.
fn format_serper_results(
    responses: &[SerperResponse],
    max_results: usize,
    max_news: usize,
    max_people_also_ask: usize,
) -> String {
    let mut out = String::new();

    // Knowledge graph (first response that has one)
    for resp in responses {
        if let Some(kg) = &resp.knowledge_graph {
            if let Some(ref title) = kg.title {
                let _ = writeln!(out, "**Knowledge Graph:** {}", title);
                if let Some(ref desc) = kg.description {
                    let _ = writeln!(out, "{}", desc);
                }
                let _ = writeln!(out);
                break;
            }
        }
    }

    // Organic results
    let mut organic_count = 0;
    for resp in responses {
        for r in &resp.organic {
            if organic_count >= max_results { break; }
            format_result_line(
                &mut out,
                r.title.as_deref().unwrap_or(""),
                r.link.as_deref().unwrap_or(""),
                r.snippet.as_deref().unwrap_or(""),
                r.date.as_deref().unwrap_or(""),
            );
            organic_count += 1;
        }
    }

    // People Also Ask
    let mut paa_items: Vec<&SerperPeopleAlsoAsk> = Vec::new();
    for resp in responses {
        paa_items.extend(resp.people_also_ask.iter());
    }
    if !paa_items.is_empty() {
        let _ = writeln!(out, "\n**People Also Ask:**");
        for paa in paa_items.iter().take(max_people_also_ask) {
            let q = paa.question.as_deref().unwrap_or("");
            let a = paa.snippet.as_deref().unwrap_or("");
            let _ = writeln!(out, "- Q: {} A: {}", q, a);
        }
    }

    // News results
    let mut news_count = 0;
    let mut has_news_header = false;
    for resp in responses {
        for item in &resp.news {
            if news_count >= max_news { break; }
            if !has_news_header {
                let _ = writeln!(out, "\n**Recent News:**");
                has_news_header = true;
            }
            format_result_line(
                &mut out,
                item.title.as_deref().unwrap_or("(untitled)"),
                item.link.as_deref().unwrap_or(""),
                item.snippet.as_deref().unwrap_or(""),
                item.date.as_deref().unwrap_or(""),
            );
            news_count += 1;
        }
    }

    out
}

/// Format EvalResult items into readable text.
fn format_eval_results(results: &[EvalResult]) -> String {
    let mut out = String::new();
    for r in results {
        format_result_line(
            &mut out,
            r.title.as_deref().unwrap_or(""),
            r.url.as_deref().unwrap_or(""),
            r.snippet.as_deref().unwrap_or(""),
            r.date.as_deref().unwrap_or(""),
        );
    }
    out
}

/// Extract JSON from an LLM response that may contain markdown fences or preamble.
fn extract_json(response: &str) -> &str {
    let trimmed = response.trim();

    // Try to find JSON within ```json fences
    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            return after[..end].trim();
        }
    }
    // Try plain ``` fences
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        // Skip optional language tag on fence line
        let after = match after.find('\n') {
            Some(nl) => &after[nl + 1..],
            None => after,
        };
        if let Some(end) = after.find("```") {
            return after[..end].trim();
        }
    }
    // Try to find raw JSON object
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
    }
    trimmed
}
