use vclaw::tmux::TmuxController;

#[tokio::test]
async fn test_execute_command() {
    let ctrl = TmuxController::new("vclaw-test");
    // Start a detached session for testing
    ctrl.execute_raw("new-session -d -s vclaw-test").await.unwrap();
    let result = ctrl.execute_raw("list-sessions -F '#{session_name}'").await.unwrap();
    assert!(result.stdout.contains("vclaw-test"));
    // Cleanup
    ctrl.execute_raw("kill-session -t vclaw-test").await.ok();
}

#[tokio::test]
async fn test_list_panes() {
    let ctrl = TmuxController::new("vclaw-test-panes");
    ctrl.execute_raw("new-session -d -s vclaw-test-panes").await.unwrap();
    let panes = ctrl.list_panes().await.unwrap();
    assert!(!panes.is_empty());
    assert!(panes[0].id.starts_with('%'));
    ctrl.execute_raw("kill-session -t vclaw-test-panes").await.ok();
}

#[tokio::test]
async fn test_capture_pane() {
    let ctrl = TmuxController::new("vclaw-test-cap");
    ctrl.execute_raw("new-session -d -s vclaw-test-cap").await.unwrap();
    ctrl.send_keys("vclaw-test-cap", "echo hello-vclaw", true).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let content = ctrl.capture_pane("vclaw-test-cap", 50).await.unwrap();
    assert!(content.contains("hello-vclaw"));
    ctrl.execute_raw("kill-session -t vclaw-test-cap").await.ok();
}
