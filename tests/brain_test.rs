use vclaw::brain::{
    build_system_prompt, build_tool_definitions, build_user_message, ClaudeCodeState,
};

#[test]
fn test_tool_definitions_structure() {
    let tools = build_tool_definitions();
    assert_eq!(tools.len(), 2);
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"shell_input"));
    assert!(names.contains(&"speak"));
}

#[test]
fn test_system_prompt_not_empty() {
    let prompt = build_system_prompt("", "");
    assert!(prompt.len() > 100);
    assert!(prompt.contains("vclaw"));
}

#[test]
fn test_build_user_message_with_context() {
    let msg = build_user_message("run tests", "%0", &ClaudeCodeState::Idle);
    assert!(msg.contains("run tests"));
    assert!(msg.contains("%0"));
}
