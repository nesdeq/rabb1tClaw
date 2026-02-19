//! Tests for the advanced orchestrator agent.
//!
//! Covers: directive parsing, dispatch block parsing/stripping, tracker status
//! block formatting, and sandbox env var injection.
//!
//! These are standalone tests — they duplicate the parsing logic since
//! rabb1tclaw is a binary crate (integration tests can't import from it).

use std::path::Path;

// ============================================================================
// Directive parsing (standalone reimplementation for testing)
// ============================================================================

#[derive(Debug)]
enum Directive {
    Code(String),
    Search(String),
    Question(String),
    Done(String),
}

fn parse_directives(response: &str) -> Vec<Directive> {
    let mut directives = Vec::new();
    let mut search_from = 0;

    while search_from < response.len() {
        let remaining = &response[search_from..];
        let Some(fence_start) = remaining.find("```") else { break };
        let after_fence = &remaining[fence_start + 3..];
        let type_end = after_fence.find('\n').unwrap_or(after_fence.len());
        let fence_type = after_fence[..type_end].trim();
        let content_start = if type_end < after_fence.len() { type_end + 1 } else { type_end };
        let Some(close) = after_fence[content_start..].find("```") else {
            search_from += fence_start + 3;
            continue;
        };
        let content = after_fence[content_start..content_start + close].trim().to_string();
        search_from += fence_start + 3 + content_start + close + 3;

        match fence_type {
            "code" => directives.push(Directive::Code(content)),
            "search" => directives.push(Directive::Search(content)),
            "question" => directives.push(Directive::Question(content)),
            "done" => directives.push(Directive::Done(content)),
            _ => {}
        }
    }
    directives
}

// ============================================================================
// Dispatch block parsing (standalone reimplementation — same as markers.rs)
// ============================================================================

const DISPATCH_OPEN: &str = "@@dispatch\n";
const BLOCK_CLOSE: &str = "\n@@end";

/// Parse dispatch blocks and return (type, desc) or (id, answer) pairs as JSON values.
fn parse_dispatch_blocks(response: &str) -> Vec<serde_json::Value> {
    let mut results = Vec::new();
    let mut search_from = 0;
    while let Some(open_offset) = response[search_from..].find(DISPATCH_OPEN) {
        let content_start = search_from + open_offset + DISPATCH_OPEN.len();
        if let Some(close_offset) = response[content_start..].find(BLOCK_CLOSE) {
            let raw = response[content_start..content_start + close_offset].trim();
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(raw) {
                results.extend(arr);
            }
            search_from = content_start + close_offset + BLOCK_CLOSE.len();
        } else {
            break;
        }
    }
    results
}

fn strip_dispatch_blocks(response: &str) -> String {
    let mut result = String::with_capacity(response.len());
    let mut search_from = 0;
    while let Some(open_offset) = response[search_from..].find(DISPATCH_OPEN) {
        let abs_start = search_from + open_offset;
        let trim_start = if abs_start > 0 && response.as_bytes()[abs_start - 1] == b'\n' {
            abs_start - 1
        } else {
            abs_start
        };
        result.push_str(&response[search_from..trim_start]);
        let content_start = abs_start + DISPATCH_OPEN.len();
        if let Some(close_offset) = response[content_start..].find(BLOCK_CLOSE) {
            let block_end = content_start + close_offset + BLOCK_CLOSE.len();
            if block_end < response.len() && response.as_bytes()[block_end] == b'\n' {
                search_from = block_end + 1;
            } else {
                search_from = block_end;
            }
        } else {
            result.push_str(&response[abs_start..]);
            return result;
        }
    }
    result.push_str(&response[search_from..]);
    result
}

// ============================================================================
// Directive Parsing Tests
// ============================================================================

#[test]
fn test_parse_single_code_directive() {
    let input = r"I need to analyze the data first.

```code
Download the CSV from https://example.com/data.csv and print summary stats.
```
";
    let directives = parse_directives(input);
    assert_eq!(directives.len(), 1);
    assert!(matches!(&directives[0], Directive::Code(s) if s.contains("Download the CSV")));
}

#[test]
fn test_parse_single_search_directive() {
    let input = r"Let me look up the latest information.

```search
Q4 2025 retail sales trends united states
```
";
    let directives = parse_directives(input);
    assert_eq!(directives.len(), 1);
    assert!(matches!(&directives[0], Directive::Search(s) if s.contains("retail sales")));
}

#[test]
fn test_parse_done_directive() {
    let input = r"All tasks are complete.

```done
Created workspace/report.md with full analysis and 3 charts.
```
";
    let directives = parse_directives(input);
    assert_eq!(directives.len(), 1);
    assert!(matches!(&directives[0], Directive::Done(s) if s.contains("report.md")));
}

#[test]
fn test_parse_question_directive() {
    let input = r"I'm not sure about the format.

```question
Should the report be PDF or markdown?
```
";
    let directives = parse_directives(input);
    assert_eq!(directives.len(), 1);
    assert!(matches!(&directives[0], Directive::Question(s) if s.contains("PDF or markdown")));
}

#[test]
fn test_parse_multiple_directives() {
    let input = r"I'll search for two things.

```search
latest AI news 2026
```

And also:

```search
OpenAI GPT-5 release date
```
";
    let directives = parse_directives(input);
    assert_eq!(directives.len(), 2);
    assert!(matches!(&directives[0], Directive::Search(s) if s.contains("AI news")));
    assert!(matches!(&directives[1], Directive::Search(s) if s.contains("GPT-5")));
}

#[test]
fn test_parse_no_directives() {
    let input = "Just some thinking text with no fenced blocks at all.";
    let directives = parse_directives(input);
    assert_eq!(directives.len(), 0);
}

#[test]
fn test_parse_ignores_unknown_fence_types() {
    let input = r#"Here's some python:

```python
print("hello")
```

And a code directive:

```code
Run the analysis script.
```
"#;
    let directives = parse_directives(input);
    assert_eq!(directives.len(), 1); // only the ```code block
}

#[test]
fn test_parse_unclosed_fence() {
    let input = "```code\nThis fence is never closed";
    let directives = parse_directives(input);
    assert_eq!(directives.len(), 0);
}

#[test]
fn test_parse_directive_content_trimmed() {
    let input = "```done\n\n  Summary with whitespace  \n\n```";
    let directives = parse_directives(input);
    assert_eq!(directives.len(), 1);
    if let Directive::Done(s) = &directives[0] {
        assert_eq!(s, "Summary with whitespace");
    } else {
        panic!("expected Done directive");
    }
}

#[test]
fn test_parse_mixed_directives_with_reasoning() {
    let input = r"Let me think about this step by step.

First, I need to understand the data. The user wants a sales analysis.

```code
Load /workspace/sales.csv with pandas, compute monthly revenue totals, and print the top 5 months.
```

After I see the results, I'll decide whether to create a chart.
";
    let directives = parse_directives(input);
    assert_eq!(directives.len(), 1);
    assert!(matches!(&directives[0], Directive::Code(s) if s.contains("sales.csv")));
}

// ============================================================================
// Dispatch Block Parsing Tests (@@dispatch ... @@end)
// ============================================================================

#[test]
fn test_parse_dispatch_block() {
    let response = "I'll handle this.\n@@dispatch\n[{\"type\":\"advanced\",\"desc\":\"Build a sales report with charts\"}]\n@@end\nLet me work on it.";
    let markers = parse_dispatch_blocks(response);
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0]["type"], "advanced");
    assert_eq!(markers[0]["desc"], "Build a sales report with charts");
}

#[test]
fn test_parse_multiple_dispatch_blocks() {
    let response = "@@dispatch\n[{\"type\":\"code\",\"desc\":\"task one\"}]\n@@end\nSome text\n@@dispatch\n[{\"type\":\"search\",\"desc\":\"task two\"}]\n@@end";
    let markers = parse_dispatch_blocks(response);
    assert_eq!(markers.len(), 2);
    assert_eq!(markers[0]["desc"], "task one");
    assert_eq!(markers[1]["desc"], "task two");
}

#[test]
fn test_parse_no_dispatch_blocks() {
    let response = "Just a normal response with no markers.";
    let markers = parse_dispatch_blocks(response);
    assert_eq!(markers.len(), 0);
}

#[test]
fn test_parse_malformed_json_skipped() {
    let response = "@@dispatch\nnot valid json\n@@end";
    let markers = parse_dispatch_blocks(response);
    assert_eq!(markers.len(), 0);
}

#[test]
fn test_parse_unclosed_dispatch_block() {
    let response = "@@dispatch\n[{\"type\":\"code\",\"desc\":\"unclosed\"}]";
    let markers = parse_dispatch_blocks(response);
    assert_eq!(markers.len(), 0);
}

#[test]
fn test_strip_dispatch_block() {
    // The stripper eats the preceding \n and trailing \n around the block
    let response = "Before\n@@dispatch\n[{\"type\":\"code\",\"desc\":\"do something\"}]\n@@end\nAfter";
    let stripped = strip_dispatch_blocks(response);
    assert_eq!(stripped, "BeforeAfter");
}

#[test]
fn test_strip_multiple_dispatch_blocks() {
    let response = "A\n@@dispatch\n[{\"type\":\"code\",\"desc\":\"x\"}]\n@@end\nB\n@@dispatch\n[{\"type\":\"search\",\"desc\":\"y\"}]\n@@end\nC";
    let stripped = strip_dispatch_blocks(response);
    assert_eq!(stripped, "ABC");
}

#[test]
fn test_parse_answer_dispatch() {
    let response = "Got it.\n@@dispatch\n[{\"id\":3,\"answer\":\"Use PDF format with charts\"}]\n@@end";
    let markers = parse_dispatch_blocks(response);
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0]["id"], 3);
    assert_eq!(markers[0]["answer"], "Use PDF format with charts");
}

#[test]
fn test_parse_mixed_dispatch_array() {
    let response = "@@dispatch\n[{\"type\":\"code\",\"desc\":\"compute\"},{\"type\":\"search\",\"desc\":\"lookup\"}]\n@@end";
    let markers = parse_dispatch_blocks(response);
    assert_eq!(markers.len(), 2);
    assert_eq!(markers[0]["type"], "code");
    assert_eq!(markers[1]["type"], "search");
}

#[test]
fn test_strip_preserves_normal_at_signs() {
    let response = "Email me at user@example.com and also @@ mention someone";
    let stripped = strip_dispatch_blocks(response);
    assert_eq!(stripped, response);
}

#[test]
fn test_dispatch_at_start_of_response() {
    let response = "@@dispatch\n[{\"type\":\"search\",\"desc\":\"weather\"}]\n@@end\nChecking now.";
    let markers = parse_dispatch_blocks(response);
    assert_eq!(markers.len(), 1);
    let stripped = strip_dispatch_blocks(response);
    assert_eq!(stripped, "Checking now.");
}

// ============================================================================
// Task Log Format Tests (@@task ... @@end)
// ============================================================================

#[test]
fn test_task_block_dispatched_format() {
    let line = "[16:38:01] dispatched #5 advanced — Build report";
    let block = format!("@@task\n{line}\n@@end");
    assert!(block.starts_with("@@task\n"));
    assert!(block.ends_with("\n@@end"));
    assert!(block.contains("dispatched #5 advanced"));
    assert!(block.contains("Build report"));
}

#[test]
fn test_task_block_completed_format() {
    let line = "[16:42:30] completed #5 — Found 3 outdated packages";
    let block = format!("@@task\n{line}\n@@end");
    assert!(block.contains("completed #5"));
    assert!(block.contains("Found 3 outdated packages"));
}

#[test]
fn test_task_block_failed_format() {
    let line = "[16:44:00] failed #3 — Timeout after 600s";
    let block = format!("@@task\n{line}\n@@end");
    assert!(block.contains("failed #3"));
    assert!(block.contains("Timeout after 600s"));
}

#[test]
fn test_task_block_asking_format() {
    let line = "[16:44:30] asking #4 — Should the report include Q3 data?";
    let block = format!("@@task\n{line}\n@@end");
    assert!(block.contains("asking #4"));
    assert!(block.contains("Should the report include Q3 data?"));
}

#[test]
fn test_task_block_live_running_format() {
    let lines = "[16:38:01] dispatched #5 advanced — Build report\n[live] running #5 advanced — Build report (45s)";
    let block = format!("@@task\n{lines}\n@@end");
    assert!(block.contains("[live] running #5"));
    assert!(block.contains("(45s)"));
}

// ============================================================================
// Sandbox Helpers (shared with sandbox.rs tests)
// ============================================================================

fn find_python() -> (String, String) {
    for cmd in ["python3", "python"] {
        if let Ok(output) = std::process::Command::new(cmd)
            .args(["-c", "import sys; print(sys.executable); print(sys.prefix)"])
            .output()
        {
            if output.status.success() {
                let out = String::from_utf8_lossy(&output.stdout);
                let mut lines = out.lines();
                let executable = lines.next().unwrap_or("").trim().to_string();
                let prefix = lines.next().unwrap_or("").trim().to_string();
                if !executable.is_empty() && Path::new(&executable).exists() {
                    return (executable, prefix);
                }
            }
        }
    }
    panic!("no python3 found on host");
}

const ETC_MOUNTS: &[&str] = &[
    "/etc/hosts", "/etc/nsswitch.conf", "/etc/ssl", "/etc/ca-certificates",
    "/etc/ld.so.cache", "/etc/ld.so.conf", "/etc/ld.so.conf.d", "/etc/localtime",
];

fn resolv_conf_source() -> &'static str {
    const REAL: &str = "/run/systemd/resolve/resolv.conf";
    if Path::new(REAL).exists() { REAL } else { "/etc/resolv.conf" }
}

fn build_container(workspace: &Path, python_prefix: &Path, network: bool) -> hakoniwa::Container {
    let ws = workspace.to_string_lossy();
    let prefix_str = python_prefix.to_string_lossy().to_string();
    let mut c = hakoniwa::Container::new();
    c.bindmount_ro(&prefix_str, &prefix_str);
    c.bindmount_ro("/usr", "/usr");
    for path in ["/lib", "/lib64", "/lib32", "/bin", "/sbin"] {
        let p = Path::new(path);
        if p.is_symlink() {
            if let Ok(target) = std::fs::read_link(p) {
                c.symlink(&target.to_string_lossy(), path);
            }
        } else if p.is_dir() {
            c.bindmount_ro(path, path);
        }
    }
    c.runctl(hakoniwa::Runctl::MountFallback);
    c.bindmount_ro(resolv_conf_source(), "/etc/resolv.conf");
    for path in ETC_MOUNTS {
        if Path::new(path).exists() {
            c.bindmount_ro(path, path);
        }
    }
    c.bindmount_rw(&ws, "/workspace");
    c.devfsmount("/dev").tmpfsmount("/tmp");
    c.setrlimit(hakoniwa::Rlimit::As, 2_000_000_000, 2_000_000_000)
        .setrlimit(hakoniwa::Rlimit::Nproc, 64, 64)
        .setrlimit(hakoniwa::Rlimit::Nofile, 256, 256);
    if network {
        c.unshare(hakoniwa::Namespace::Network);
        c.network(hakoniwa::Pasta::default());
    }
    c
}

// ============================================================================
// Env Var Injection Tests
// ============================================================================

#[test]
fn test_env_var_injection_in_sandbox() {
    let tmp = tempfile::tempdir().unwrap();
    let (python, prefix) = find_python();

    // Create venv
    let container = build_container(tmp.path(), Path::new(&prefix), false);
    let output = container
        .command(&python)
        .args(["-m", "venv", "/workspace/.venv"])
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(60)
        .output()
        .expect("venv setup failed");
    assert!(output.status.success(), "venv creation failed: {}", output.status.reason);

    // Write script that reads env vars
    std::fs::write(
        tmp.path().join("env_test.py"),
        r#"
import os
key = os.environ.get("TEST_API_KEY", "")
name = os.environ.get("TEST_NAME", "")
print(f"KEY={key}")
print(f"NAME={name}")
"#,
    ).unwrap();

    // Execute with env vars injected
    let container = build_container(tmp.path(), Path::new(&prefix), false);
    let output = container
        .command("/workspace/.venv/bin/python")
        .arg("/workspace/env_test.py")
        .current_dir("/workspace")
        .env("TEST_API_KEY", "sk-test-12345")
        .env("TEST_NAME", "advanced_agent")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(30)
        .output()
        .expect("env var test failed to execute");

    assert!(output.status.success(), "env var test failed: {}", output.status.reason);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("KEY=sk-test-12345"), "expected API key, got: {stdout}");
    assert!(stdout.contains("NAME=advanced_agent"), "expected name, got: {stdout}");
}

#[test]
fn test_env_vars_not_leaked_without_injection() {
    let tmp = tempfile::tempdir().unwrap();
    let (python, prefix) = find_python();

    // Create venv
    let container = build_container(tmp.path(), Path::new(&prefix), false);
    let output = container
        .command(&python)
        .args(["-m", "venv", "/workspace/.venv"])
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(60)
        .output()
        .expect("venv setup failed");
    assert!(output.status.success());

    // Write script that checks for env var
    std::fs::write(
        tmp.path().join("no_env_test.py"),
        r#"
import os
key = os.environ.get("TEST_API_KEY", "MISSING")
print(f"KEY={key}")
"#,
    ).unwrap();

    // Execute WITHOUT env vars — should not find the key
    let container = build_container(tmp.path(), Path::new(&prefix), false);
    let output = container
        .command("/workspace/.venv/bin/python")
        .arg("/workspace/no_env_test.py")
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(30)
        .output()
        .expect("no env test failed");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("KEY=MISSING"), "env var should not be present: {stdout}");
}
