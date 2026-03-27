pub fn compose_visible_final_reply(
    final_response: &str,
    final_plan_text: Option<&str>,
) -> Option<String> {
    let final_response = final_response.trim();
    let final_plan_text = final_plan_text
        .map(str::trim)
        .filter(|text| !text.is_empty());

    match (!final_response.is_empty(), final_plan_text) {
        (true, Some(plan)) => Some(format!("{final_response}\n\n## Proposed Plan\n\n{plan}")),
        (true, None) => Some(final_response.to_owned()),
        (false, Some(plan)) => Some(plan.to_owned()),
        (false, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::compose_visible_final_reply;

    #[test]
    fn combines_assistant_and_plan() {
        let composed = compose_visible_final_reply("Done.", Some("# Plan\n- one"));
        assert_eq!(
            composed.as_deref(),
            Some("Done.\n\n## Proposed Plan\n\n# Plan\n- one")
        );
    }

    #[test]
    fn falls_back_to_plan_only() {
        let composed = compose_visible_final_reply("   ", Some("# Plan\n- one"));
        assert_eq!(composed.as_deref(), Some("# Plan\n- one"));
    }

    #[test]
    fn returns_assistant_only_when_no_plan_exists() {
        let composed = compose_visible_final_reply("Done.", None);
        assert_eq!(composed.as_deref(), Some("Done."));
    }

    #[test]
    fn returns_none_when_both_inputs_are_empty() {
        let composed = compose_visible_final_reply("   ", Some("   "));
        assert_eq!(composed, None);
    }
}
