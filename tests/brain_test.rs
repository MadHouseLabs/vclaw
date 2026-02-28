use vclaw::brain::{build_tool_definitions, build_system_prompt, build_user_message};
use vclaw::event::PaneInfo;

#[test]
fn test_tool_definitions_structure() {
    let tools = build_tool_definitions();
    assert_eq!(tools.len(), 4);
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"tmux_execute"));
    assert!(names.contains(&"shell_input"));
    assert!(names.contains(&"read_pane"));
    assert!(names.contains(&"speak"));
}

#[test]
fn test_system_prompt_not_empty() {
    let prompt = build_system_prompt();
    assert!(prompt.len() > 100);
    assert!(prompt.contains("tmux"));
    assert!(prompt.contains("vclaw"));
}

#[test]
fn test_build_user_message_with_context() {
    let panes = vec![PaneInfo {
        id: "%0".into(),
        title: "~/dev".into(),
        size: "80x24".into(),
        active: true,
    }];
    let msg = build_user_message("run tests", &panes, "$ cargo test\nrunning 3 tests");
    assert!(msg.contains("run tests"));
    assert!(msg.contains("%0"));
    assert!(msg.contains("cargo test"));
}
