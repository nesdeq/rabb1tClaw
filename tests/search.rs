//! Integration test for the search pipeline (plan → multi-type fetch → enrich → context).
//!
//! Requires:
//!   - `SERP_API_KEY` env var
//!   - Optionally a search LLM (`OPENAI_API_KEY` or `ANTHROPIC_API_KEY`) for Phase 1
//!
//! Run with:
//!   `SERP_API_KEY`=... cargo test --test search -- --nocapture


// ============================================================================
// Standalone Serper types (mirrors agent.rs — no crate imports possible)
// ============================================================================

use std::collections::{HashMap, HashSet};

#[derive(serde::Deserialize, Debug)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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

// Phase 1 LLM output
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

fn default_search_type() -> String { "search".into() }

#[derive(serde::Deserialize, Debug)]
struct AnalyzeOutput {
    #[serde(default = "default_depth")]
    depth: String,
    queries: Vec<AnalyzedQuery>,
}

fn default_depth() -> String { "thorough".into() }

// ============================================================================
// LLM config — from env vars (OPENAI_API_KEY / ANTHROPIC_API_KEY)
// ============================================================================

struct ResolvedLlm {
    base_url: String,
    api_key: String,
    api_type: String,
    model_id: String,
}

fn load_env() {
    let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".env");
    if env_path.exists() {
        for (k, v) in dotenvy::from_path_iter(&env_path).expect("Failed to read .env").flatten() {
            std::env::set_var(&k, &v);
        }
    }
}

fn resolve_search_llm() -> Option<ResolvedLlm> {
    load_env();

    // Prefer OpenAI (gpt-5.2), fall back to Anthropic
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.is_empty() {
            return Some(ResolvedLlm {
                base_url: "https://api.openai.com/v1".into(),
                api_key: key,
                api_type: "openai".into(),
                model_id: "gpt-5.2".into(),
            });
        }
    }
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return Some(ResolvedLlm {
                base_url: "https://api.anthropic.com/v1".into(),
                api_key: key,
                api_type: "anthropic".into(),
                model_id: "claude-sonnet-4-5-20250929".into(),
            });
        }
    }
    None
}

// ============================================================================
// Helpers
// ============================================================================

async fn call_llm(
    client: &reqwest::Client,
    llm: &ResolvedLlm,
    system_prompt: &str,
    user_content: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    match llm.api_type.as_str() {
        "anthropic" | "anthropic-messages" => {
            call_anthropic(client, llm, system_prompt, user_content).await
        }
        _ => {
            call_openai(client, llm, system_prompt, user_content).await
        }
    }
}

async fn call_openai(
    client: &reqwest::Client,
    llm: &ResolvedLlm,
    system_prompt: &str,
    user_content: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let url = format!("{}/chat/completions", llm.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": llm.model_id,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_content}
        ],
        "max_completion_tokens": 4096,
        "stream": false,
    });

    let resp = client.post(&url)
        .bearer_auth(&llm.api_key)
        .json(&body)
        .send().await?
        .error_for_status()?;
    let json: serde_json::Value = resp.json().await?;
    let text = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    Ok(text)
}

async fn call_anthropic(
    client: &reqwest::Client,
    llm: &ResolvedLlm,
    system_prompt: &str,
    user_content: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let url = format!("{}/messages", llm.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": llm.model_id,
        "system": system_prompt,
        "messages": [
            {"role": "user", "content": user_content}
        ],
        "max_tokens": 4096,
    });

    let resp = client.post(&url)
        .header("x-api-key", &llm.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?
        .error_for_status()?;
    let json: serde_json::Value = resp.json().await?;

    let text = json["content"].as_array()
        .and_then(|arr| arr.iter().find(|b| b["type"] == "text"))
        .and_then(|b| b["text"].as_str())
        .unwrap_or("")
        .to_string();
    Ok(text)
}

fn extract_json(response: &str) -> &str {
    let trimmed = response.trim();
    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            return after[..end].trim();
        }
    }
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        let after = after.find('\n').map_or(after, |nl| &after[nl + 1..]);
        if let Some(end) = after.find("```") {
            return after[..end].trim();
        }
    }
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
    }
    trimmed
}

async fn fetch_serper(
    client: &reqwest::Client,
    api_key: &str,
    query: &AnalyzedQuery,
    num: usize,
) -> Result<SerperResponse, Box<dyn std::error::Error>> {
    let endpoint = match query.r#type.as_str() {
        "news" => "https://google.serper.dev/news",
        _ => "https://google.serper.dev/search",
    };
    let mut body = serde_json::json!({ "q": query.q, "num": num });
    let obj = body.as_object_mut().unwrap();
    if let Some(ref gl) = query.gl { obj.insert("gl".into(), gl.clone().into()); }
    if let Some(ref hl) = query.hl { obj.insert("hl".into(), hl.clone().into()); }
    if let Some(ref tbs) = query.tbs { obj.insert("tbs".into(), tbs.clone().into()); }
    if let Some(ref loc) = query.location { obj.insert("location".into(), loc.clone().into()); }

    let resp: SerperResponse = client.post(endpoint)
        .header("X-API-KEY", api_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?
        .error_for_status()?
        .json().await?;
    Ok(resp)
}

async fn fetch_and_extract(
    client: &reqwest::Client,
    url: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let resp = client.get(url)
        .timeout(std::time::Duration::from_secs(15))
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
        .send().await?
        .error_for_status()?;
    let html = resp.text().await?;
    let result = tokio::task::spawn_blocking(move || {
        rs_trafilatura::extract(&html)
    }).await??;
    let text = result.content_text;
    if text.trim().is_empty() {
        return Err("trafilatura extracted no content".into());
    }
    Ok(text)
}

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

fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars { return s.to_string(); }
    // Find a char boundary at or before max_chars
    let mut end = max_chars;
    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
    s[..end].to_string()
}

// System prompt — loaded from src/prompts/
const SEARCH_ANALYZE_PROMPT: &str = include_str!("../src/prompts/system_search_analyze.md");

// ============================================================================
// Tests
// ============================================================================

/// Full pipeline: plan → multi-type fetch → enrich → context assembly.
/// Run: `SERP_API_KEY`=... cargo test --test search `search_pipeline` -- --nocapture
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn search_pipeline() {
    let api_key = match std::env::var("SERP_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => { eprintln!("SERP_API_KEY not set — skipping"); return; }
    };

    let llm = resolve_search_llm();
    let has_llm = llm.is_some();
    let client = reqwest::Client::builder().tcp_nodelay(true).build().unwrap();

    println!("\n{}", "=".repeat(72));
    println!("  SEARCH PIPELINE INTEGRATION TEST");
    println!("  LLM available: {has_llm}");
    println!("{}\n", "=".repeat(72));

    let query = "Wetter Münster Vorhersage bis Ende der Woche Tageswerte Regen Temperatur Wind";

    // ── PHASE 1: SEARCH PLAN ──────────────────────────────────────
    println!("┌──────────────────────────────────────────────────────────────┐");
    println!("│  PHASE 1: SEARCH PLAN (depth + queries)                     │");
    println!("└──────────────────────────────────────────────────────────────┘");
    println!("  Input: {query:?}\n");

    let (depth, analyzed) = if let Some(ref l) = llm {
        match call_llm(&client, l, SEARCH_ANALYZE_PROMPT, query).await {
            Ok(raw) => {
                println!("  LLM raw response ({} chars):", raw.len());
                for line in raw.lines() { println!("    │ {line}"); }
                let json_str = extract_json(&raw);
                match serde_json::from_str::<AnalyzeOutput>(json_str) {
                    Ok(out) => {
                        let d = if out.depth == "quick" || out.depth == "thorough" {
                            out.depth
                        } else {
                            "thorough".into()
                        };
                        println!("\n  Depth: {d}");
                        println!("  Parsed {} queries:", out.queries.len());
                        for (i, q) in out.queries.iter().enumerate() {
                            println!("    [{}] q={:?} type={:?} gl={:?} hl={:?} tbs={:?} loc={:?}",
                                i, q.q, q.r#type, q.gl, q.hl, q.tbs, q.location);
                        }
                        (d, out.queries)
                    }
                    Err(e) => {
                        println!("\n  JSON parse failed: {e}");
                        println!("  Falling back to raw query");
                        ("thorough".into(), vec![AnalyzedQuery { q: query.into(), r#type: "search".into(), gl: None, hl: None, tbs: None, location: None }])
                    }
                }
            }
            Err(e) => {
                println!("  LLM call failed: {e}");
                ("thorough".into(), vec![AnalyzedQuery { q: query.into(), r#type: "search".into(), gl: None, hl: None, tbs: None, location: None }])
            }
        }
    } else {
        println!("  (no LLM configured — raw query, thorough depth)");
        ("thorough".into(), vec![AnalyzedQuery { q: query.into(), r#type: "search".into(), gl: None, hl: None, tbs: None, location: None }])
    };

    // ── PHASE 2: MULTI-TYPE SERPER FETCH ──────────────────────────
    println!("\n┌──────────────────────────────────────────────────────────────┐");
    println!("│  PHASE 2: MULTI-TYPE SERPER FETCH (depth={depth:10})       │");
    println!("└──────────────────────────────────────────────────────────────┘");

    // Expand queries into search variants based on depth
    let mut search_queries = Vec::new();
    for aq in &analyzed {
        if depth == "quick" {
            search_queries.push(("web_past_day", make_variant(aq, "search", Some("qdr:d".into()))));
        } else {
            search_queries.push(("web_all_time", make_variant(aq, "search", aq.tbs.clone())));
            search_queries.push(("web_past_day", make_variant(aq, "search", Some("qdr:d".into()))));
            search_queries.push(("news", make_variant(aq, "news", None)));
        }
    }

    let mut all_responses: Vec<SerperResponse> = Vec::new();
    for (i, (label, sq)) in search_queries.iter().enumerate() {
        let num = if sq.r#type == "news" { 5 } else { 10 };
        println!("  [{}] {} — q={:?} type={} num={}", i, label, sq.q, sq.r#type, num);
        match fetch_serper(&client, &api_key, sq, num).await {
            Ok(resp) => {
                println!("      {} organic, {} news, kg={}, {} PAA",
                    resp.organic.len(), resp.news.len(),
                    resp.knowledge_graph.is_some(), resp.people_also_ask.len());
                all_responses.push(resp);
            }
            Err(e) => println!("      FAILED: {e}"),
        }
    }

    assert!(!all_responses.is_empty(), "All Serper requests failed — check SERP_API_KEY");

    // Deduplicate results
    let mut seen_urls = HashSet::new();
    let mut unique_urls: Vec<(String, String, String)> = Vec::new(); // (title, url, snippet)

    for resp in &all_responses {
        // Knowledge graph
        if let Some(kg) = &resp.knowledge_graph {
            if let Some(ref title) = kg.title {
                let mut body = kg.description.clone().unwrap_or_default();
                if !kg.attributes.is_empty() {
                    let attrs: String = kg.attributes.iter()
                        .map(|(k, v)| format!("{}: {}", k, v.as_str().unwrap_or(&v.to_string())))
                        .collect::<Vec<_>>()
                        .join(" | ");
                    if !body.is_empty() { body.push_str(" | "); }
                    body.push_str(&attrs);
                }
                if seen_urls.insert("#knowledge_graph".to_string()) {
                    unique_urls.push((title.clone(), "#knowledge_graph".into(), body));
                }
            }
        }
        for r in &resp.organic {
            let url = r.link.as_deref().unwrap_or("").to_string();
            if !url.is_empty() && seen_urls.insert(url.clone()) {
                unique_urls.push((
                    r.title.as_deref().unwrap_or("").to_string(),
                    url,
                    r.snippet.as_deref().unwrap_or("").to_string(),
                ));
            }
        }
        for r in &resp.news {
            let url = r.link.as_deref().unwrap_or("").to_string();
            if !url.is_empty() && seen_urls.insert(url.clone()) {
                unique_urls.push((
                    r.title.as_deref().unwrap_or("").to_string(),
                    url,
                    r.snippet.as_deref().unwrap_or("").to_string(),
                ));
            }
        }
    }

    println!("\n  Total unique results: {}", unique_urls.len());
    for (i, (title, url, _)) in unique_urls.iter().take(10).enumerate() {
        println!("    [{}] {} — {}", i, title, truncate(url, 60));
    }
    if unique_urls.len() > 10 {
        println!("    ... and {} more", unique_urls.len() - 10);
    }

    // ── PHASE 3: ENRICH (fetch page content) ──────────────────────
    println!("\n┌──────────────────────────────────────────────────────────────┐");
    println!("│  PHASE 3: ENRICH (fetch page content)                       │");
    println!("└──────────────────────────────────────────────────────────────┘");

    let max_fetch = 10;
    let fetchable: Vec<&(String, String, String)> = unique_urls.iter()
        .filter(|(_, url, _)| !url.starts_with('#'))
        .take(max_fetch)
        .collect();

    println!("  Fetching {} URLs...\n", fetchable.len());

    let mut enriched_content: Vec<(String, String, String, String)> = Vec::new(); // (title, url, snippet, content)

    // Add non-fetchable results first
    for (title, url, snippet) in &unique_urls {
        if url.starts_with('#') {
            enriched_content.push((title.clone(), url.clone(), snippet.clone(), snippet.clone()));
        }
    }

    // Fetch all URLs in parallel
    let mut futs = Vec::new();
    for (_, url, _) in &fetchable {
        let c = client.clone();
        let u = url.clone();
        futs.push(async move { (u.clone(), fetch_and_extract(&c, &u).await) });
    }
    let results = futures::future::join_all(futs).await;

    let mut fetched_count = 0;
    for (i, (url, res)) in results.iter().enumerate() {
        let (title, _, snippet) = fetchable[i];
        match res {
            Ok(content) => {
                let trimmed = truncate(content, 5000);
                println!("  [{}] {} chars — {}", i, content.len(), truncate(url, 50));
                enriched_content.push((title.clone(), url.clone(), snippet.clone(), trimmed));
                fetched_count += 1;
            }
            Err(e) => {
                println!("  [{}] FAILED — {} — {}", i, truncate(url, 50), e);
                enriched_content.push((title.clone(), url.clone(), snippet.clone(), String::new()));
            }
        }
    }

    println!("\n  Successfully fetched: {}/{}", fetched_count, fetchable.len());

    // ── PHASE 4: TOKEN-BUDGETED CONTEXT ASSEMBLY ──────────────────
    println!("\n┌──────────────────────────────────────────────────────────────┐");
    println!("│  PHASE 4: CONTEXT ASSEMBLY                                  │");
    println!("└──────────────────────────────────────────────────────────────┘");

    let max_context_chars = 32000; // simplified char limit for test
    let mut context = String::new();
    let mut included = 0;

    for (title, url, snippet, content) in &enriched_content {
        let content_section = if content.is_empty() { snippet } else { content };
        let entry = format!(
            "Title: {title}\nURL: {url}\nSnippet: {snippet}\nContent: {content_section}\n\n"
        );
        if context.len() + entry.len() > max_context_chars {
            println!("  Char limit reached at {included} results");
            break;
        }
        context.push_str(&entry);
        included += 1;
    }

    println!("  Included: {}/{} results", included, enriched_content.len());
    println!("  Context: {} chars", context.len());

    // ── FINAL OUTPUT ────────────────────────────────────────────────
    let final_text = truncate(&context, 32000);

    println!("\n{}", "=".repeat(72));
    println!("  FINAL CONTEXT ({} chars, {} results)", final_text.len(), included);
    println!("{}\n", "=".repeat(72));
    // Show first 3000 chars
    let preview = truncate(&final_text, 3000);
    for line in preview.lines() { println!("  {line}"); }
    if final_text.len() > 3000 { println!("\n  ... ({} more chars)", final_text.len() - 3000); }
    println!("\n{}", "=".repeat(72));

    assert!(!final_text.is_empty(), "Final output empty");
    assert!(final_text.len() > 50, "Output suspiciously short: {} chars", final_text.len());
    assert!(included > 0, "No results included in context");
}

/// Smoke test: `extract_json` strips markdown fences correctly.
#[test]
fn test_extract_json_fenced() {
    assert_eq!(
        extract_json("Here:\n```json\n{\"queries\": []}\n```\nDone."),
        r#"{"queries": []}"#,
    );
}

#[test]
fn test_extract_json_raw() {
    let input = r#"{"depth": "thorough", "queries": []}"#;
    assert_eq!(extract_json(input), input);
}

#[test]
fn test_extract_json_with_preamble() {
    assert_eq!(
        extract_json("Sure:\n{\"queries\": [{\"q\": \"test\"}]}"),
        r#"{"queries": [{"q": "test"}]}"#,
    );
}
