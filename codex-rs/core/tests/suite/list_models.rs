use codex_app_server_protocol::AuthMode;
use codex_common::model_presets::ReasoningEffortPreset;
use codex_common::model_presets::builtin_model_presets;
use codex_core::protocol_config_types::ReasoningEffort;
use pretty_assertions::assert_eq;

#[test]
fn gpt_5_2_preset_includes_expected_metadata() {
    let presets = builtin_model_presets(None);
    let preset = presets
        .iter()
        .find(|preset| preset.id == "gpt-5.2")
        .expect("gpt-5.2 preset should be available");

    assert_eq!(preset.model, "gpt-5.2");
    assert_eq!(preset.display_name, "gpt-5.2");
    assert_eq!(
        preset.description,
        "Latest frontier model with improvements across knowledge, reasoning and coding"
    );
    assert_eq!(preset.default_reasoning_effort, ReasoningEffort::Medium);
    assert_eq!(
        reasoning_efforts(preset.supported_reasoning_efforts),
        vec![
            reasoning_effort(
                ReasoningEffort::Low,
                "Balances speed with some reasoning; useful for straightforward queries and short explanations"
            ),
            reasoning_effort(
                ReasoningEffort::Medium,
                "Provides a solid balance of reasoning depth and latency for general-purpose tasks"
            ),
            reasoning_effort(
                ReasoningEffort::High,
                "Greater reasoning depth for complex or ambiguous problems"
            ),
            reasoning_effort(
                ReasoningEffort::XHigh,
                "Extra high reasoning for complex problems"
            ),
        ]
    );
}

#[test]
fn api_key_model_list_includes_gpt_5_2() {
    let presets = builtin_model_presets(Some(AuthMode::ApiKey));
    assert!(presets.iter().any(|preset| preset.id == "gpt-5.2"));
}

fn reasoning_efforts(efforts: &[ReasoningEffortPreset]) -> Vec<(ReasoningEffort, &'static str)> {
    efforts
        .iter()
        .map(|preset| (preset.effort, preset.description))
        .collect()
}

fn reasoning_effort(
    effort: ReasoningEffort,
    description: &'static str,
) -> (ReasoningEffort, &'static str) {
    (effort, description)
}
