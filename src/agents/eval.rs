use serde::Serialize;
use std::time::Instant;

use crate::agents::agent::Agent;
use crate::agents::conversation::Conversation;
use crate::agents::llm::LlmClient;

// ---------------------------------------------------------------------------
// EvalCase
// ---------------------------------------------------------------------------

/// A single evaluation case
#[derive(Debug, Clone)]
pub struct EvalCase {
    pub name: Option<String>,
    pub input: String,
    pub expect_tool_calls: Option<Vec<String>>,
    pub expect_no_tool_calls: Option<bool>,
    pub expect_contains: Option<String>,
    pub expect_not_contains: Option<String>,
    pub expect_regex: Option<String>,
    /// LLM-as-judge (deferred — stored but not evaluated)
    pub expect_semantic: Option<String>,
    /// Deferred — stored but not evaluated
    pub expect_handoff: Option<String>,
}

// ---------------------------------------------------------------------------
// EvalResult
// ---------------------------------------------------------------------------

/// Result of running a single eval case
#[derive(Debug, Clone, Serialize)]
pub struct EvalResult {
    pub case_name: String,
    pub passed: bool,
    pub failures: Vec<String>,
    pub response_text: String,
    pub tool_calls_made: Vec<String>,
    pub latency_ms: u64,
    pub cost_usd: f64,
}

// ---------------------------------------------------------------------------
// EvalSuite
// ---------------------------------------------------------------------------

/// Collection of eval cases for an agent
pub struct EvalSuite {
    pub cases: Vec<EvalCase>,
}

impl EvalSuite {
    pub fn new(cases: Vec<EvalCase>) -> Self {
        Self { cases }
    }

    /// Run all eval cases against the given agent and return a summary.
    pub async fn run(&self, agent: &Agent, llm_client: &LlmClient) -> EvalSummary {
        let mut results: Vec<EvalResult> = Vec::with_capacity(self.cases.len());

        for case in &self.cases {
            let case_name = case
                .name
                .clone()
                .unwrap_or_else(|| format!("case-{}", results.len() + 1));

            // 1. Create a new Conversation and add the user message.
            let mut conv = Conversation::new(&agent.config.name);
            conv.add_user_message(&case.input);

            // 2. Run the agent turn, measuring latency.
            let start = Instant::now();
            let run_result = agent.run_turn(&mut conv, llm_client).await;
            let latency_ms = start.elapsed().as_millis() as u64;

            let (response_text, tool_calls_made, cost_usd) = match run_result {
                Ok(response) => {
                    let tool_names: Vec<String> =
                        response.tool_calls.iter().map(|tc| tc.name.clone()).collect();
                    // Cost is estimated from token counts; no pricing table here,
                    // so we record 0.0 (will be enriched by trace when available).
                    let cost_usd = 0.0_f64;
                    (response.text, tool_names, cost_usd)
                }
                Err(e) => {
                    // Treat a hard error as a failed case with the error message.
                    let result = EvalResult {
                        case_name,
                        passed: false,
                        failures: vec![format!("agent error: {e}")],
                        response_text: String::new(),
                        tool_calls_made: Vec::new(),
                        latency_ms,
                        cost_usd: 0.0,
                    };
                    results.push(result);
                    continue;
                }
            };

            // 3. Evaluate assertions.
            let mut failures: Vec<String> = Vec::new();

            // expect_tool_calls: all listed tools must appear in the response.
            if let Some(ref expected_tools) = case.expect_tool_calls {
                for expected in expected_tools {
                    if !tool_calls_made.contains(expected) {
                        failures.push(format!(
                            "Expected tool call '{}' but it was not made (calls: [{}])",
                            expected,
                            tool_calls_made.join(", ")
                        ));
                    }
                }
            }

            // expect_no_tool_calls: response must have no tool calls.
            if case.expect_no_tool_calls == Some(true) && !tool_calls_made.is_empty() {
                failures.push(format!(
                    "Expected no tool calls but got: [{}]",
                    tool_calls_made.join(", ")
                ));
            }

            // expect_contains: case-insensitive substring match.
            if let Some(ref needle) = case.expect_contains {
                if !response_text
                    .to_lowercase()
                    .contains(&needle.to_lowercase())
                {
                    let preview = preview_text(&response_text, 80);
                    failures.push(format!(
                        "Expected response to contain {:?} (case-insensitive)\n    Got: {:?}",
                        needle, preview
                    ));
                }
            }

            // expect_not_contains: response must NOT contain the string.
            if let Some(ref needle) = case.expect_not_contains {
                if response_text
                    .to_lowercase()
                    .contains(&needle.to_lowercase())
                {
                    failures.push(format!(
                        "Expected response NOT to contain {:?} but it did",
                        needle
                    ));
                }
            }

            // expect_regex: compile and match against full response text.
            if let Some(ref pattern) = case.expect_regex {
                match regex::Regex::new(pattern) {
                    Ok(re) => {
                        if !re.is_match(&response_text) {
                            let preview = preview_text(&response_text, 80);
                            failures.push(format!(
                                "Expected response to match regex {:?}\n    Got: {:?}",
                                pattern, preview
                            ));
                        }
                    }
                    Err(e) => {
                        failures.push(format!("Invalid regex {:?}: {}", pattern, e));
                    }
                }
            }

            // expect_semantic: deferred.
            if case.expect_semantic.is_some() {
                tracing::info!(case = %case_name, "semantic evaluation deferred");
            }

            // expect_handoff: deferred.
            if case.expect_handoff.is_some() {
                tracing::info!(case = %case_name, "handoff evaluation deferred");
            }

            results.push(EvalResult {
                case_name,
                passed: failures.is_empty(),
                failures,
                response_text,
                tool_calls_made,
                latency_ms,
                cost_usd,
            });
        }

        // Compute summary statistics.
        let total = results.len();
        let passed = results.iter().filter(|r| r.passed).count();
        let failed = total - passed;
        let total_cost_usd: f64 = results.iter().map(|r| r.cost_usd).sum();
        let total_latency_ms: u64 = results.iter().map(|r| r.latency_ms).sum();

        EvalSummary {
            total,
            passed,
            failed,
            results,
            total_cost_usd,
            total_latency_ms,
        }
    }

    /// Format an `EvalSummary` as a human-readable report string.
    pub fn format_results(summary: &EvalSummary) -> String {
        let mut out = String::new();

        out.push_str(&format!("Running {} eval cases...\n", summary.total));

        for result in &summary.results {
            if result.passed {
                out.push_str(&format!(
                    "  \u{2713} {:<40} {:>6}ms  ${:.4}\n",
                    result.case_name, result.latency_ms, result.cost_usd
                ));
            } else {
                out.push_str(&format!(
                    "  \u{2717} {:<40} {:>6}ms  ${:.4}\n",
                    result.case_name, result.latency_ms, result.cost_usd
                ));
                for failure in &result.failures {
                    // Indent each failure line.
                    for line in failure.lines() {
                        out.push_str(&format!("    {}\n", line));
                    }
                }
                // Show a preview of the actual response text.
                if !result.response_text.is_empty() {
                    let preview = preview_text(&result.response_text, 80);
                    out.push_str(&format!("    Got: {:?}\n", preview));
                }
            }
        }

        let pct = if summary.total > 0 {
            (summary.passed as f64 / summary.total as f64) * 100.0
        } else {
            0.0
        };

        out.push_str(&format!(
            "\nResults: {}/{} passed ({:.1}%)\n",
            summary.passed, summary.total, pct
        ));
        out.push_str(&format!(
            "Total cost: ${:.4}\n",
            summary.total_cost_usd
        ));

        out
    }
}

// ---------------------------------------------------------------------------
// EvalSummary
// ---------------------------------------------------------------------------

/// Summary of running all eval cases
#[derive(Debug, Clone, Serialize)]
pub struct EvalSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub results: Vec<EvalResult>,
    pub total_cost_usd: f64,
    pub total_latency_ms: u64,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Return at most `max_chars` characters from `text`, appending "..." if truncated.
fn preview_text(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        text.to_string()
    } else {
        format!("{}...", &text[..max_chars])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns an `EvalCase` with all optional fields set to `None`.
    fn default_case() -> EvalCase {
        EvalCase {
            name: None,
            input: String::new(),
            expect_tool_calls: None,
            expect_no_tool_calls: None,
            expect_contains: None,
            expect_not_contains: None,
            expect_regex: None,
            expect_semantic: None,
            expect_handoff: None,
        }
    }

    // -----------------------------------------------------------------------
    // Helpers that run assertion logic without a live agent
    // -----------------------------------------------------------------------

    /// Apply the `expect_contains` assertion to a given response text and
    /// return the collected failure messages.
    fn check_contains(needle: &str, response_text: &str) -> Vec<String> {
        let mut failures = Vec::new();
        if !response_text
            .to_lowercase()
            .contains(&needle.to_lowercase())
        {
            let preview = preview_text(response_text, 80);
            failures.push(format!(
                "Expected response to contain {:?} (case-insensitive)\n    Got: {:?}",
                needle, preview
            ));
        }
        failures
    }

    /// Apply the `expect_regex` assertion and return collected failure messages.
    fn check_regex(pattern: &str, response_text: &str) -> Vec<String> {
        let mut failures = Vec::new();
        match regex::Regex::new(pattern) {
            Ok(re) => {
                if !re.is_match(response_text) {
                    let preview = preview_text(response_text, 80);
                    failures.push(format!(
                        "Expected response to match regex {:?}\n    Got: {:?}",
                        pattern, preview
                    ));
                }
            }
            Err(e) => {
                failures.push(format!("Invalid regex {:?}: {}", pattern, e));
            }
        }
        failures
    }

    // -----------------------------------------------------------------------
    // EvalCase construction
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_case_all_none() {
        let case = default_case();
        assert!(case.name.is_none());
        assert!(case.expect_contains.is_none());
        assert!(case.expect_regex.is_none());
        assert!(case.expect_tool_calls.is_none());
        assert!(case.expect_no_tool_calls.is_none());
        assert!(case.expect_semantic.is_none());
        assert!(case.expect_handoff.is_none());
    }

    #[test]
    fn test_eval_case_field_override() {
        let case = EvalCase {
            name: Some("test".to_string()),
            input: "hello".to_string(),
            expect_contains: Some("world".to_string()),
            ..default_case()
        };
        assert_eq!(case.name.as_deref(), Some("test"));
        assert_eq!(case.input, "hello");
        assert_eq!(case.expect_contains.as_deref(), Some("world"));
        // Everything else stays None.
        assert!(case.expect_regex.is_none());
    }

    // -----------------------------------------------------------------------
    // expect_contains assertion
    // -----------------------------------------------------------------------

    #[test]
    fn test_eval_case_contains_assertion_passes() {
        let _case = EvalCase {
            name: Some("test".to_string()),
            input: "test".to_string(),
            expect_contains: Some("hello".to_string()),
            ..default_case()
        };
        // "Hello World" contains "hello" (case-insensitive) → no failures.
        let failures = check_contains("hello", "Hello World");
        assert!(failures.is_empty(), "expected no failures, got: {:?}", failures);
    }

    #[test]
    fn test_eval_case_contains_assertion_fails() {
        let _case = EvalCase {
            name: Some("test".to_string()),
            input: "test".to_string(),
            expect_contains: Some("hello".to_string()),
            ..default_case()
        };
        // "Goodbye" does not contain "hello" → one failure.
        let failures = check_contains("hello", "Goodbye");
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("hello"));
    }

    #[test]
    fn test_eval_case_contains_case_insensitive() {
        let failures = check_contains("HELLO", "hello world");
        assert!(
            failures.is_empty(),
            "contains check should be case-insensitive"
        );
    }

    // -----------------------------------------------------------------------
    // expect_not_contains assertion
    // -----------------------------------------------------------------------

    #[test]
    fn test_not_contains_passes_when_absent() {
        let response = "The weather is sunny today.";
        let needle = "refund";
        let is_present = response
            .to_lowercase()
            .contains(&needle.to_lowercase());
        assert!(!is_present, "should pass when needle is absent");
    }

    #[test]
    fn test_not_contains_fails_when_present() {
        let response = "We can process a refund for you.";
        let needle = "refund";
        let is_present = response
            .to_lowercase()
            .contains(&needle.to_lowercase());
        assert!(is_present, "should fail when needle is present");
    }

    // -----------------------------------------------------------------------
    // expect_regex assertion
    // -----------------------------------------------------------------------

    #[test]
    fn test_eval_case_regex_assertion_passes() {
        // Pattern matches a dollar amount like "$42.00".
        let failures = check_regex(r"\$\d+\.\d{2}", "Your total is $42.00.");
        assert!(
            failures.is_empty(),
            "expected no failures, got: {:?}",
            failures
        );
    }

    #[test]
    fn test_eval_case_regex_assertion_fails() {
        let failures = check_regex(r"\$\d+\.\d{2}", "No price mentioned here.");
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("regex"));
    }

    #[test]
    fn test_eval_case_regex_invalid_pattern() {
        // An unclosed bracket is an invalid regex.
        let failures = check_regex(r"[invalid", "some text");
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("Invalid regex"));
    }

    #[test]
    fn test_eval_case_regex_case_sensitive_by_default() {
        // The default regex is case-sensitive.
        let failures_upper = check_regex("Hello", "hello world");
        assert_eq!(
            failures_upper.len(),
            1,
            "case-sensitive regex should NOT match 'hello' with 'Hello'"
        );

        let failures_lower = check_regex("hello", "hello world");
        assert!(
            failures_lower.is_empty(),
            "should match when case matches"
        );
    }

    // -----------------------------------------------------------------------
    // EvalSummary / format_results
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_results_all_pass() {
        let summary = EvalSummary {
            total: 2,
            passed: 2,
            failed: 0,
            results: vec![
                EvalResult {
                    case_name: "case-one".to_string(),
                    passed: true,
                    failures: vec![],
                    response_text: "ok".to_string(),
                    tool_calls_made: vec![],
                    latency_ms: 100,
                    cost_usd: 0.0001,
                },
                EvalResult {
                    case_name: "case-two".to_string(),
                    passed: true,
                    failures: vec![],
                    response_text: "fine".to_string(),
                    tool_calls_made: vec![],
                    latency_ms: 200,
                    cost_usd: 0.0002,
                },
            ],
            total_cost_usd: 0.0003,
            total_latency_ms: 300,
        };

        let report = EvalSuite::format_results(&summary);
        assert!(report.contains("Running 2 eval cases"));
        assert!(report.contains("2/2 passed"));
        assert!(report.contains("100.0%"));
        assert!(report.contains("case-one"));
        assert!(report.contains("case-two"));
        // Check checkmark (✓) appears.
        assert!(report.contains('\u{2713}'));
    }

    #[test]
    fn test_format_results_with_failure() {
        let summary = EvalSummary {
            total: 1,
            passed: 0,
            failed: 1,
            results: vec![EvalResult {
                case_name: "bad-case".to_string(),
                passed: false,
                failures: vec!["Expected: contains \"refund\"".to_string()],
                response_text: "I can help you with...".to_string(),
                tool_calls_made: vec![],
                latency_ms: 680,
                cost_usd: 0.0004,
            }],
            total_cost_usd: 0.0004,
            total_latency_ms: 680,
        };

        let report = EvalSuite::format_results(&summary);
        assert!(report.contains("0/1 passed"));
        assert!(report.contains("bad-case"));
        // Cross mark (✗) should appear.
        assert!(report.contains('\u{2717}'));
        assert!(report.contains("Expected: contains"));
    }

    #[test]
    fn test_format_results_empty() {
        let summary = EvalSummary {
            total: 0,
            passed: 0,
            failed: 0,
            results: vec![],
            total_cost_usd: 0.0,
            total_latency_ms: 0,
        };
        let report = EvalSuite::format_results(&summary);
        assert!(report.contains("Running 0 eval cases"));
        assert!(report.contains("0/0 passed"));
    }

    // -----------------------------------------------------------------------
    // EvalSuite::new
    // -----------------------------------------------------------------------

    #[test]
    fn test_eval_suite_new() {
        let cases = vec![
            EvalCase {
                name: Some("a".to_string()),
                input: "hi".to_string(),
                ..default_case()
            },
            default_case(),
        ];
        let suite = EvalSuite::new(cases);
        assert_eq!(suite.cases.len(), 2);
    }
}
