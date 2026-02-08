//! Integration test for the 3-phase search pipeline.
//!
//! Requires:
//!   - SERP_API_KEY env var
//!   - A search model configured in ~/.rabb1tclaw/config.yaml
//!
//! Run with:
//!   SERP_API_KEY=... cargo test --test search -- --nocapture


// ============================================================================
// Standalone Serper types (mirrors agent.rs — no crate imports possible)
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
    queries: Vec<AnalyzedQuery>,
}

// Phase 2 LLM output
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
        for item in dotenvy::from_path_iter(&env_path).expect("Failed to read .env") {
            if let Ok((k, v)) = item {
                std::env::set_var(&k, &v);
            }
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
        let after = match after.find('\n') {
            Some(nl) => &after[nl + 1..],
            None => after,
        };
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
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36")
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

fn format_serper_results(responses: &[SerperResponse], max_results: usize, max_news: usize, max_paa: usize) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    for resp in responses {
        if let Some(kg) = &resp.knowledge_graph {
            if let Some(ref title) = kg.title {
                let _ = writeln!(out, "**Knowledge Graph:** {}", title);
                if let Some(ref desc) = kg.description { let _ = writeln!(out, "{}", desc); }
                let _ = writeln!(out);
                break;
            }
        }
    }
    let mut oc = 0;
    for resp in responses {
        for r in &resp.organic {
            if oc >= max_results { break; }
            let title = r.title.as_deref().unwrap_or("");
            let url = r.link.as_deref().unwrap_or("");
            let snippet = r.snippet.as_deref().unwrap_or("");
            let date = r.date.as_deref().unwrap_or("");
            if date.is_empty() {
                let _ = writeln!(out, "- **{}** ({})\n  {}", title, url, snippet);
            } else {
                let _ = writeln!(out, "- **{}** ({}) [{}]\n  {}", title, url, date, snippet);
            }
            oc += 1;
        }
    }
    let paa: Vec<&SerperPeopleAlsoAsk> = responses.iter().flat_map(|r| r.people_also_ask.iter()).collect();
    if !paa.is_empty() {
        let _ = writeln!(out, "\n**People Also Ask:**");
        for p in paa.iter().take(max_paa) {
            let _ = writeln!(out, "- Q: {} A: {}", p.question.as_deref().unwrap_or(""), p.snippet.as_deref().unwrap_or(""));
        }
    }
    let mut nc = 0;
    let mut hdr = false;
    for resp in responses {
        for item in &resp.news {
            if nc >= max_news { break; }
            if !hdr { let _ = writeln!(out, "\n**Recent News:**"); hdr = true; }
            let title = item.title.as_deref().unwrap_or("(untitled)");
            let snippet = item.snippet.as_deref().unwrap_or("");
            let url = item.link.as_deref().unwrap_or("");
            let date = item.date.as_deref().unwrap_or("");
            if date.is_empty() {
                let _ = writeln!(out, "- {} — {} ({})", title, snippet, url);
            } else {
                let _ = writeln!(out, "- {} [{}] — {} ({})", title, date, snippet, url);
            }
            nc += 1;
        }
    }
    out
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars { return s.to_string(); }
    let end = s.floor_char_boundary(max_chars);
    s[..end].to_string()
}

// System prompts — loaded from docs/ the same way the binary does
const SEARCH_ANALYZE_PROMPT: &str = include_str!("../docs/system_search_analyze.md");
const SEARCH_EVALUATE_PROMPT: &str = include_str!("../docs/system_search_evaluate.md");
const SEARCH_SYNTHESIZE_PROMPT: &str = include_str!("../docs/system_search_synthesize.md");

// ============================================================================
// Tests
// ============================================================================

/// Full 3-phase pipeline with step-by-step output.
/// Run: SERP_API_KEY=... cargo test --test search search_pipeline_3phase -- --nocapture
#[tokio::test]
async fn search_pipeline_3phase() {
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

    let query = "Münster Veranstaltungen dieses Wochenende";

    // ── PHASE 1 ─────────────────────────────────────────────────────
    println!("┌──────────────────────────────────────────────────────────────┐");
    println!("│  PHASE 1: QUERY ANALYSIS                                    │");
    println!("└──────────────────────────────────────────────────────────────┘");
    println!("  Input: {:?}\n", query);

    let analyzed = if let Some(ref l) = llm {
        match call_llm(&client, l, SEARCH_ANALYZE_PROMPT, query).await {
            Ok(raw) => {
                println!("  LLM raw response ({} chars):", raw.len());
                for line in raw.lines() { println!("    │ {}", line); }
                let json_str = extract_json(&raw);
                match serde_json::from_str::<AnalyzeOutput>(json_str) {
                    Ok(out) => {
                        println!("\n  Parsed {} queries:", out.queries.len());
                        for (i, q) in out.queries.iter().enumerate() {
                            println!("    [{}] q={:?} type={:?} gl={:?} hl={:?} tbs={:?} loc={:?}",
                                i, q.q, q.r#type, q.gl, q.hl, q.tbs, q.location);
                        }
                        out.queries
                    }
                    Err(e) => {
                        println!("\n  JSON parse failed: {}", e);
                        println!("  Falling back to raw query");
                        vec![AnalyzedQuery { q: query.into(), r#type: "search".into(), gl: None, hl: None, tbs: None, location: None }]
                    }
                }
            }
            Err(e) => {
                println!("  LLM call failed: {}", e);
                vec![AnalyzedQuery { q: query.into(), r#type: "search".into(), gl: None, hl: None, tbs: None, location: None }]
            }
        }
    } else {
        println!("  (no LLM configured — raw query)");
        vec![AnalyzedQuery { q: query.into(), r#type: "search".into(), gl: None, hl: None, tbs: None, location: None }]
    };

    // ── PHASE 2a ────────────────────────────────────────────────────
    println!("\n┌──────────────────────────────────────────────────────────────┐");
    println!("│  PHASE 2a: SERPER FETCH                                     │");
    println!("└──────────────────────────────────────────────────────────────┘");

    let mut all_responses: Vec<SerperResponse> = Vec::new();
    for (i, aq) in analyzed.iter().enumerate() {
        let num = if aq.r#type == "news" { 5 } else { 10 };
        println!("  [{}] q={:?} type={} num={}", i, aq.q, aq.r#type, num);
        match fetch_serper(&client, &api_key, aq, num).await {
            Ok(resp) => {
                println!("      {} organic, {} news, kg={}, {} PAA",
                    resp.organic.len(), resp.news.len(),
                    resp.knowledge_graph.is_some(), resp.people_also_ask.len());
                // Print first 3 organic titles
                for (j, r) in resp.organic.iter().take(3).enumerate() {
                    println!("      organic[{}]: {} — {}", j,
                        r.title.as_deref().unwrap_or("?"),
                        r.link.as_deref().unwrap_or("?"));
                }
                all_responses.push(resp);
            }
            Err(e) => println!("      FAILED: {}", e),
        }
    }

    assert!(!all_responses.is_empty(), "All Serper requests failed — check SERP_API_KEY");

    let results_text = format_serper_results(&all_responses, 10, 5, 5);
    println!("\n  Formatted results: {} chars", results_text.len());
    println!("  ─── begin ───");
    let preview = truncate(&results_text, 3000);
    for line in preview.lines() { println!("  │ {}", line); }
    if results_text.len() > 3000 { println!("  │ ... ({} more chars)", results_text.len() - 3000); }
    println!("  ─── end ───");

    // ── PHASE 2b ────────────────────────────────────────────────────
    println!("\n┌──────────────────────────────────────────────────────────────┐");
    println!("│  PHASE 2b: EVALUATE                                         │");
    println!("└──────────────────────────────────────────────────────────────┘");

    let final_output = if let Some(ref l) = llm {
        let eval_input = format!("## Original Query\n\n{}\n\n## Search Results\n\n{}", query, results_text);
        match call_llm(&client, l, SEARCH_EVALUATE_PROMPT, &eval_input).await {
            Ok(raw) => {
                println!("  LLM raw response ({} chars):", raw.len());
                let raw_preview = truncate(&raw, 2000);
                for line in raw_preview.lines() { println!("    │ {}", line); }
                if raw.len() > 2000 { println!("    │ ... ({} more)", raw.len() - 2000); }

                let json_str = extract_json(&raw);
                match serde_json::from_str::<EvaluateOutput>(json_str) {
                    Ok(eval) => {
                        println!("\n  Verdict: {}", eval.verdict);
                        println!("  Results: {} items", eval.results.len());
                        println!("  Partial: {} items", eval.partial_results.len());
                        println!("  URLs for deep read: {:?}", eval.urls);

                        if eval.verdict == "need_deep_read" && !eval.urls.is_empty() {
                            // ── PHASE 3 ─────────────────────────────────
                            println!("\n┌──────────────────────────────────────────────────────────────┐");
                            println!("│  PHASE 3: DEEP READ + SYNTHESIZE                             │");
                            println!("└──────────────────────────────────────────────────────────────┘");

                            let urls: Vec<&str> = eval.urls.iter().take(3).map(|s| s.as_str()).collect();

                            // Fetch each URL
                            for (i, &url) in urls.iter().enumerate() {
                                print!("  [{}] {} ... ", i, url);
                                match fetch_and_extract(&client, url).await {
                                    Ok(content) => {
                                        println!("{} chars", content.len());
                                        let p = truncate(&content, 200);
                                        println!("      {:?}...", p);
                                    }
                                    Err(e) => println!("FAILED: {}", e),
                                }
                            }

                            // Build synthesis input
                            let mut pages = String::new();
                            let mut futs = Vec::new();
                            for &url in &urls {
                                let c = client.clone();
                                let u = url.to_string();
                                futs.push(async move { (u.clone(), fetch_and_extract(&c, &u).await) });
                            }
                            let results = futures::future::join_all(futs).await;
                            for (url, res) in results {
                                match res {
                                    Ok(content) => {
                                        let t = truncate(&content, 8000);
                                        pages.push_str(&format!("--- Page: {} ---\n{}\n\n", url, t));
                                    }
                                    Err(e) => {
                                        pages.push_str(&format!("--- Page: {} ---\n(failed: {})\n\n", url, e));
                                    }
                                }
                            }

                            let synth_input = format!(
                                "## Original Query\n\n{}\n\n## Search Snippets\n\n{}\n\n## Full Page Content\n\n{}",
                                query, results_text, pages);

                            println!("\n  Calling synthesis LLM ({} chars input)...", synth_input.len());

                            match call_llm(&client, l, SEARCH_SYNTHESIZE_PROMPT, &synth_input).await {
                                Ok(synth) => {
                                    println!("  Synthesis: {} chars\n", synth.len());
                                    synth
                                }
                                Err(e) => {
                                    println!("  Synthesis FAILED: {}\n  Using partial results", e);
                                    format_partial(&eval.partial_results)
                                }
                            }
                        } else {
                            println!("\n  Snippets sufficient — using evaluated results");
                            format_partial(&eval.results)
                        }
                    }
                    Err(e) => {
                        println!("  JSON parse failed: {}", e);
                        println!("  Using raw Serper results");
                        results_text
                    }
                }
            }
            Err(e) => {
                println!("  LLM call failed: {}", e);
                results_text
            }
        }
    } else {
        println!("  (no LLM — raw results)");
        results_text
    };

    // ── FINAL OUTPUT ────────────────────────────────────────────────
    let final_text = truncate(&final_output, 32000);

    println!("\n{}", "=".repeat(72));
    println!("  FINAL OUTPUT ({} chars)", final_text.len());
    println!("{}\n", "=".repeat(72));
    for line in final_text.lines() { println!("  {}", line); }
    println!("\n{}", "=".repeat(72));

    assert!(!final_text.is_empty(), "Final output empty");
    assert!(final_text.len() > 50, "Output suspiciously short: {} chars", final_text.len());
}

fn format_partial(results: &[EvalResult]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for r in results {
        let title = r.title.as_deref().unwrap_or("");
        let url = r.url.as_deref().unwrap_or("");
        let snippet = r.snippet.as_deref().unwrap_or("");
        let date = r.date.as_deref().unwrap_or("");
        if date.is_empty() {
            let _ = writeln!(out, "- **{}** ({})\n  {}", title, url, snippet);
        } else {
            let _ = writeln!(out, "- **{}** ({}) [{}]\n  {}", title, url, date, snippet);
        }
    }
    out
}

/// Smoke test: extract_json strips markdown fences correctly.
#[test]
fn test_extract_json_fenced() {
    assert_eq!(
        extract_json("Here:\n```json\n{\"queries\": []}\n```\nDone."),
        r#"{"queries": []}"#,
    );
}

#[test]
fn test_extract_json_raw() {
    let input = r#"{"verdict": "sufficient", "results": []}"#;
    assert_eq!(extract_json(input), input);
}

#[test]
fn test_extract_json_with_preamble() {
    assert_eq!(
        extract_json("Sure:\n{\"queries\": [{\"q\": \"test\"}]}"),
        r#"{"queries": [{"q": "test"}]}"#,
    );
}
