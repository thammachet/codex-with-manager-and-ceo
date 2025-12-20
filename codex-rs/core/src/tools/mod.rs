pub mod context;
pub mod events;
pub(crate) mod handlers;
pub mod orchestrator;
pub mod parallel;
pub mod registry;
pub mod router;
pub mod runtimes;
pub mod sandboxing;
pub mod spec;

use crate::exec::ExecToolCallOutput;
use crate::truncate::TruncationPolicy;
use crate::truncate::formatted_truncate_text;
use crate::truncate::truncate_text;
pub use router::ToolRouter;
use serde_json::json;

// Telemetry preview limits: keep log events smaller than model budgets.
pub(crate) const TELEMETRY_PREVIEW_MAX_BYTES: usize = 2 * 1024; // 2 KiB
pub(crate) const TELEMETRY_PREVIEW_MAX_LINES: usize = 64; // lines
pub(crate) const TELEMETRY_PREVIEW_TRUNCATION_NOTICE: &str =
    "[... telemetry preview truncated ...]";

/// Format the combined exec output for sending back to the model.
/// Includes exit code and duration metadata; truncates large bodies safely.
pub fn format_exec_output_for_model_structured(
    exec_output: &ExecToolCallOutput,
    truncation_policy: TruncationPolicy,
) -> String {
    let total_lines = exec_output.aggregated_output.text.lines().count();
    let formatted_output = truncate_text(&exec_output.aggregated_output.text, truncation_policy);

    if exec_output.exit_code == 0 && !exec_output.timed_out {
        formatted_output
    } else {
        format_exec_output_with_metadata(exec_output, total_lines, formatted_output)
    }
}

pub fn format_exec_output_for_model_freeform(
    exec_output: &ExecToolCallOutput,
    truncation_policy: TruncationPolicy,
    tool_output_token_limit: Option<usize>,
) -> String {
    let content = build_content_with_timeout(exec_output);
    let total_lines = content.lines().count();
    let effective_policy = match tool_output_token_limit {
        Some(limit) => match truncation_policy {
            TruncationPolicy::Tokens(_) => TruncationPolicy::Tokens(limit),
            TruncationPolicy::Bytes(_) => {
                TruncationPolicy::Bytes(crate::truncate::approx_bytes_for_tokens(limit))
            }
        },
        None => truncation_policy,
    };
    let formatted_output = truncate_text(&content, effective_policy);

    // round to 1 decimal place
    let duration_seconds = ((exec_output.duration.as_secs_f32()) * 10.0).round() / 10.0;

    let mut sections = Vec::new();

    sections.push(format!("Exit code: {}", exec_output.exit_code));
    sections.push(format!("Wall time: {duration_seconds} seconds"));
    if total_lines != formatted_output.lines().count() {
        sections.push(format!("Total output lines: {total_lines}"));
    }

    sections.push("Output:".to_string());
    sections.push(formatted_output);

    sections.join("\n")
}

pub fn history_content_for_exec_output(output: &ExecToolCallOutput) -> Option<String> {
    if output.exit_code == 0 && !output.timed_out {
        return None;
    }

    Some(format_exec_output_body(
        output,
        output.aggregated_output.text.as_str(),
    ))
}

pub fn format_exec_output_str(
    exec_output: &ExecToolCallOutput,
    truncation_policy: TruncationPolicy,
) -> String {
    let content = build_content_with_timeout(exec_output);

    // Truncate for model consumption before serialization.
    formatted_truncate_text(&content, truncation_policy)
}

/// Extracts exec output content and prepends a timeout message if the command timed out.
fn build_content_with_timeout(exec_output: &ExecToolCallOutput) -> String {
    if exec_output.timed_out {
        format!(
            "command timed out after {} milliseconds\n{}",
            exec_output.duration.as_millis(),
            exec_output.aggregated_output.text
        )
    } else {
        exec_output.aggregated_output.text.clone()
    }
}

pub fn format_exec_output_body(exec_output: &ExecToolCallOutput, content: &str) -> String {
    if exec_output.timed_out {
        return format!(
            "command timed out after {} milliseconds\n{content}",
            exec_output.duration.as_millis()
        );
    }
    content.to_string()
}

pub(crate) fn format_exec_output_with_metadata(
    exec_output: &ExecToolCallOutput,
    total_lines: usize,
    formatted_output: String,
) -> String {
    if exec_output.timed_out {
        let output_with_notice = if formatted_output.is_empty() {
            "command timed out".to_string()
        } else {
            format!("command timed out\n{formatted_output}")
        };
        return json!({
            "output": output_with_notice,
            "metadata": {
                "exit_code": exec_output.exit_code,
                "timed_out": true,
            }
        })
        .to_string();
    }

    // round to 1 decimal place
    let duration_seconds = ((exec_output.duration.as_secs_f32()) * 10.0).round() / 10.0;

    let mut sections = Vec::new();

    sections.push(format!("Exit code: {}", exec_output.exit_code));
    sections.push(format!("Wall time: {duration_seconds} seconds"));
    if exec_output.timed_out {
        sections.push("command timed out".to_string());
    }
    if total_lines != formatted_output.lines().count() {
        sections.push(format!("Total output lines: {total_lines}"));
    }

    sections.push("Output:".to_string());
    sections.push(formatted_output);

    sections.join("\n")
}
