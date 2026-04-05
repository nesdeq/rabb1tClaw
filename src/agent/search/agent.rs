use crate::agent::stream::collect_stream;
use crate::agent::runner::{resolve_agent_model, ResolvedAgentModel};
use crate::agent::tracker::{count_tokens, truncate};
use crate::config::native::{AgentKind, SEARCH_ANALYZE_PROMPT, SEARCH_SYNTHESIZE_PROMPT};
use crate::provider::{get_shared_client, ChatMessage};
use crate::state::GatewayState;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, info};

use super::tracker::{SearchQueryStatus, SearchQueryTracker};

// ============================================================================
// Serper Types
// ============================================================================

#[derive(serde::Deserialize, Debug)]
struct SerperOrganic {
    title: Option<String>,
    link: Option<String>,
    snippet: Option<String>,
    date: Option<String>,
}

#[derive(serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SerperKnowledgeGraph {
    title: Option<String>,
    description: Option<String>,
    #[serde(default)]
    attributes: HashMap<String, serde_json::Value>,
}

#[derive(serde::Deserialize, Debug)]
struct SerperPeopleAlsoAsk {
    question: Option<String>,
    snippet: Option<String>,
}

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

// ============================================================================
// Phase 1 Types
// ============================================================================

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
    #[serde(default = "default_depth")]
    depth: String,
    queries: Vec<AnalyzedQuery>,
}

fn default_depth() -> String {
    "thorough".into()
}

// ============================================================================
// Unified Result Types
// ============================================================================

/// A single search result from any source (organic, news, KG, PAA).
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    date: String,
}

/// An enriched result with fetched page content.
struct EnrichedResult {
    title: String,
    url: String,
    snippet: String,
    date: String,
    content: String,
}

// ============================================================================
// Public API
// ============================================================================

/// Bundled search operational limits.
#[derive(Debug, Clone, Copy)]
pub struct SearchLimits {
    pub max_results: usize,
    pub max_news: usize,
    pub max_people_also_ask: usize,
    pub max_total_tokens: usize,
    pub max_total_tokens_thorough: usize,
    pub max_page_tokens: usize,
    pub fetch_timeout_secs: u64,
}

impl SearchLimits {
    /// Build from gateway config, reading search agent overrides with defaults.
    pub fn from_config(cfg: &crate::config::GatewayConfig) -> Self {
        let sc = cfg.agent_config(AgentKind::Search);
        Self {
            max_results: sc.and_then(|a| a.max_results).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_RESULTS),
            max_news: sc.and_then(|a| a.max_news).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_NEWS),
            max_people_also_ask: sc.and_then(|a| a.max_people_also_ask).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_PEOPLE_ALSO_ASK),
            max_total_tokens: sc.and_then(|a| a.max_total_tokens).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_TOTAL_TOKENS),
            max_total_tokens_thorough: sc.and_then(|a| a.max_total_tokens_thorough).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_TOTAL_TOKENS_THOROUGH),
            max_page_tokens: sc.and_then(|a| a.max_page_tokens).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_PAGE_TOKENS),
            fetch_timeout_secs: sc.and_then(|a| a.fetch_timeout_secs).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_FETCH_TIMEOUT_SECS),
        }
    }
}

/// Run a web search and store results in the tracker.
pub async fn run_search(
    state: Arc<GatewayState>,
    tracker: Arc<SearchQueryTracker>,
    prefix: String,
    query_id: u32,
    query: String,
    limits: SearchLimits,
) {
    debug!("[SEARCH] [{}] Started: {}", query_id, query);

    let task_log_max = crate::agent::tasklog::max_entries(&state).await;

    let tag = query_id.to_string();
    let status = match run_search_inner(&state, &tag, &query, &limits).await {
        Ok(context) => {
            info!("[{}] [SEARCH] [{}] completed", prefix, query_id);
            SearchQueryStatus::Completed { context }
        }
        Err(e) => {
            info!("[{}] [SEARCH] [{}] failed", prefix, query_id);
            debug!("[SEARCH] [{}] error: {}", query_id, e);
            SearchQueryStatus::Failed { error: e.to_string() }
        }
    };

    let event = match &status {
        SearchQueryStatus::Completed { context } => format!("completed #{query_id} — {context}"),
        SearchQueryStatus::Failed { error } => format!("failed #{query_id} — {error}"),
        SearchQueryStatus::Running => unreachable!(),
    };
    tracker.complete(&prefix, query_id, status).await;
    crate::agent::tasklog::append(&prefix, &event, task_log_max);
}

// ============================================================================
// Internal Pipeline
// ============================================================================

/// Search pipeline — generates search plan, fetches multi-type results,
/// enriches all URLs with page content, and assembles token-budgeted context.
pub async fn run_search_inner(
    state: &GatewayState,
    tag: &str,
    query: &str,
    limits: &SearchLimits,
) -> Result<String, anyhow::Error> {
    let api_key = std::env::var("SERP_API_KEY")
        .map_err(|_| anyhow::anyhow!("SERP_API_KEY not set"))?;

    let resolved = resolve_agent_model(state, AgentKind::Search).await;

    // ── Phase 1: Generate search plan (depth + refined queries) ──
    let (depth, analyzed_queries) = match &resolved {
        Some(r) => phase1_analyze(r, tag, query).await.unwrap_or_else(|e| {
            debug!("[SEARCH] [{}] Plan fallback (raw query): {}", tag, e);
            default_plan(query)
        }),
        None => default_plan(query),
    };

    // ── Phase 2: Multi-type Serper fetch (parallel) ──
    //
    // Quick:    1 search type per query (past-day web)
    // Thorough: 3 search types per query (web all-time + web past-day + news)
    let client = get_shared_client();

    let mut search_queries: Vec<AnalyzedQuery> = Vec::new();
    for aq in &analyzed_queries {
        if depth == "quick" {
            search_queries.push(make_variant(aq, "search", Some("qdr:d".into())));
        } else {
            search_queries.push(make_variant(aq, "search", aq.tbs.clone()));
            search_queries.push(make_variant(aq, "search", Some("qdr:d".into())));
            search_queries.push(make_variant(aq, "news", None));
        }
    }

    let mut futs = Vec::new();
    for sq in &search_queries {
        let num = if sq.r#type == "news" { limits.max_news } else { limits.max_results };
        futs.push(fetch_serper(client, &api_key, sq, num));
    }
    let responses = futures::future::join_all(futs).await;

    // Collect and deduplicate results by URL
    let all_results = collect_and_dedup(responses, limits.max_people_also_ask);

    if all_results.is_empty() {
        return Err(anyhow::anyhow!("no search results found"));
    }

    debug!("[SEARCH] [{}] Fetch: {} unique results from {} queries", tag, all_results.len(), search_queries.len());

    // ── Phase 3: Enrich results (fetch page content in parallel) ──
    let enriched = enrich_results(
        client, &all_results,
        limits.max_page_tokens, limits.fetch_timeout_secs,
    ).await;

    let with_content = enriched.iter().filter(|r| !r.content.is_empty()).count();
    debug!("[SEARCH] [{}] Enrich: {} results ({} with content)", tag, enriched.len(), with_content);

    if enriched.is_empty() {
        return Err(anyhow::anyhow!("no results could be enriched"));
    }

    // ── Phase 4: Token-budgeted context assembly ──
    let total_budget = if depth == "thorough" {
        limits.max_total_tokens_thorough
    } else {
        limits.max_total_tokens
    };
    let raw_context = create_context(tag, &enriched, total_budget);

    // ── Phase 5: LLM synthesis ──
    // Turn raw results into a clean, sourced answer for the main LLM.
    let resolved = resolved.ok_or_else(|| anyhow::anyhow!("no search model configured for synthesis"))?;

    let synth_input = format!(
        "## Original Query\n\n{query}\n\n## Search Results\n\n{raw_context}"
    );
    let context = call_llm(&resolved, SEARCH_SYNTHESIZE_PROMPT, &synth_input).await?;
    debug!("[SEARCH] [{}] Synthesized: {} chars from {} raw", tag, context.len(), raw_context.len());

    Ok(context)
}

// ============================================================================
// Phase 1: Query Analysis + Depth
// ============================================================================

async fn phase1_analyze(
    resolved: &ResolvedAgentModel,
    tag: &str,
    query: &str,
) -> Result<(String, Vec<AnalyzedQuery>), anyhow::Error> {
    let response = call_llm(resolved, SEARCH_ANALYZE_PROMPT, query).await?;
    let json_str = extract_json(&response);
    let output: AnalyzeOutput = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("plan JSON parse error: {e} — raw: {response}"))?;

    if output.queries.is_empty() {
        return Err(anyhow::anyhow!("plan returned empty queries"));
    }

    let depth = match output.depth.as_str() {
        "quick" | "thorough" => output.depth,
        _ => "thorough".into(),
    };

    debug!("[SEARCH] [{}] Plan: depth={}, {} queries", tag, depth, output.queries.len());
    Ok((depth, output.queries))
}

/// Fallback when Phase 1 fails or no LLM is configured.
fn default_plan(query: &str) -> (String, Vec<AnalyzedQuery>) {
    ("thorough".to_string(), vec![AnalyzedQuery {
        q: query.to_string(),
        r#type: "search".into(),
        gl: None, hl: None, tbs: None, location: None,
    }])
}

// ============================================================================
// Result Collection + Deduplication
// ============================================================================

fn collect_and_dedup(
    responses: Vec<Result<SerperResponse, anyhow::Error>>,
    max_paa: usize,
) -> Vec<SearchResult> {
    let mut seen_urls = HashSet::new();
    let mut results: Vec<SearchResult> = Vec::new();
    let mut has_kg = false;
    let mut paa_count = 0;

    for res in responses {
        let resp = match res {
            Ok(r) => r,
            Err(e) => { debug!("[SEARCH] Serper request failed: {}", e); continue; }
        };

        // Knowledge graph (first one only)
        if !has_kg {
            if let Some(kg) = &resp.knowledge_graph {
                if let Some(ref title) = kg.title {
                    let mut body = kg.description.clone().unwrap_or_default();
                    if !kg.attributes.is_empty() {
                        let attrs: String = kg.attributes.iter()
                            .map(|(k, v)| v.as_str().map_or_else(
                                || format!("{k}: {v}"),
                                |s| format!("{k}: {s}"),
                            ))
                            .collect::<Vec<_>>()
                            .join(" | ");
                        if !body.is_empty() { body.push_str(" | "); }
                        body.push_str(&attrs);
                    }
                    results.push(SearchResult {
                        title: title.clone(),
                        url: "#knowledge_graph".into(),
                        snippet: body,
                        date: String::new(),
                    });
                    has_kg = true;
                }
            }
        }

        // Organic + news results (deduplicated by URL)
        for r in resp.organic.iter().chain(resp.news.iter()) {
            let url = r.link.as_deref().unwrap_or("");
            if !url.is_empty() && seen_urls.insert(url.to_string()) {
                results.push(SearchResult {
                    title: r.title.as_deref().unwrap_or("").to_string(),
                    url: url.to_string(),
                    snippet: r.snippet.as_deref().unwrap_or("").to_string(),
                    date: r.date.as_deref().unwrap_or("").to_string(),
                });
            }
        }

        // People Also Ask
        for paa in &resp.people_also_ask {
            if paa_count >= max_paa { break; }
            let q = paa.question.as_deref().unwrap_or("");
            let a = paa.snippet.as_deref().unwrap_or("");
            if !q.is_empty() {
                results.push(SearchResult {
                    title: format!("Q: {q}"),
                    url: "#people_also_ask".into(),
                    snippet: a.to_string(),
                    date: String::new(),
                });
                paa_count += 1;
            }
        }
    }

    results
}

// ============================================================================
// Enrichment (parallel URL fetch + content extraction)
// ============================================================================

async fn enrich_results(
    client: &reqwest::Client,
    results: &[SearchResult],
    max_page_tokens: usize,
    timeout_secs: u64,
) -> Vec<EnrichedResult> {
    // Determine which indices need URL fetching (skip #-prefixed special entries)
    let fetch_indices: Vec<usize> = results.iter().enumerate()
        .filter(|(_, r)| !r.url.starts_with('#'))
        .map(|(i, _)| i)
        .collect();

    // Fire all fetches in parallel
    let mut futs = Vec::new();
    for &i in &fetch_indices {
        futs.push(fetch_and_extract(client, &results[i].url, timeout_secs));
    }
    let fetched = futures::future::join_all(futs).await;

    // Map fetch results by index
    let mut fetch_content: HashMap<usize, String> = HashMap::new();
    for (j, res) in fetched.into_iter().enumerate() {
        let i = fetch_indices[j];
        match res {
            Ok(text) => { fetch_content.insert(i, truncate(&text, max_page_tokens)); }
            Err(e) => { debug!("[SEARCH] Fetch failed {}: {}", results[i].url, e); }
        }
    }

    // Build enriched results in original order
    results.iter().enumerate().map(|(i, r)| {
        let content = if r.url.starts_with('#') {
            r.snippet.clone() // KG/PAA: snippet IS the content
        } else {
            fetch_content.remove(&i).unwrap_or_default()
        };
        EnrichedResult {
            title: r.title.clone(),
            url: r.url.clone(),
            snippet: r.snippet.clone(),
            date: r.date.clone(),
            content,
        }
    }).collect()
}

// ============================================================================
// Token-budgeted Context Assembly
// ============================================================================

fn create_context(tag: &str, results: &[EnrichedResult], max_total_tokens: usize) -> String {
    let mut parts = Vec::new();
    let mut total_tokens = 0;
    let mut included = 0;

    for result in results {
        // Use fetched content when available, fall back to snippet
        let content_section = if result.content.is_empty() {
            &result.snippet
        } else {
            &result.content
        };

        let date_part = if result.date.is_empty() {
            String::new()
        } else {
            format!("\nDate: {}", result.date)
        };

        let entry = format!(
            "Title: {}\nURL: {}{}\nSnippet: {}\nContent: {}\n",
            result.title, result.url, date_part, result.snippet, content_section
        );

        let entry_tokens = count_tokens(&entry);
        if total_tokens + entry_tokens > max_total_tokens {
            break;
        }

        parts.push(entry);
        total_tokens += entry_tokens;
        included += 1;
    }

    debug!("[SEARCH] [{}] Context: {}/{} results, {} tokens", tag, included, results.len(), total_tokens);

    parts.join("\n")
}

// ============================================================================
// Helpers
// ============================================================================

/// Create a variant of an analyzed query with different type and tbs.
fn make_variant(aq: &AnalyzedQuery, type_str: &str, tbs: Option<String>) -> AnalyzedQuery {
    AnalyzedQuery {
        q: aq.q.clone(),
        r#type: type_str.to_string(),
        gl: aq.gl.clone(),
        hl: aq.hl.clone(),
        tbs,
        location: aq.location.clone(),
    }
}

/// One-shot LLM call: system prompt + single user message -> collected response.
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
        .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36")
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7")
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Sec-CH-UA", "\"Not(A:Brand\";v=\"8\", \"Chromium\";v=\"144\", \"Google Chrome\";v=\"144\"")
        .header("Sec-CH-UA-Mobile", "?0")
        .header("Sec-CH-UA-Platform", "\"Linux\"")
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "none")
        .header("Sec-Fetch-User", "?1")
        .header("Upgrade-Insecure-Requests", "1")
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
        let after = after.find('\n').map_or(after, |nl| &after[nl + 1..]);
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
