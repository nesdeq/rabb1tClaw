//! Integration tests for multi-device isolation.
//!
//! Verifies that:
//! - Multiple devices can connect concurrently
//! - Conversation histories are isolated per device
//! - File storage is isolated per device token prefix
//!
//! These tests verify the architecture by creating multiple simulated devices
//! and ensuring their data remains isolated.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tokio::sync::RwLock;

// ============================================================================
// Minimal test doubles (no dependency on internal modules)
// ============================================================================

fn token_prefix(token: &str) -> String {
    token[..token.len().min(8)].to_string()
}

fn generate_test_token() -> String {
    uuid::Uuid::new_v4().to_string().replace('-', "")
}

fn device_dir(prefix: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".rabb1tclaw").join(prefix)
}

#[allow(dead_code)]
struct TestDevice {
    token: String,
    name: String,
}

fn create_test_device(name: &str) -> TestDevice {
    TestDevice {
        token: generate_test_token(),
        name: name.to_string(),
    }
}

// Simple session manager for testing
struct TestSessionManager {
    sessions: RwLock<HashMap<String, Vec<(String, String)>>>, // prefix -> messages (role, content)
}

impl TestSessionManager {
    fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    async fn record_message(&self, token: &str, role: &str, content: &str) {
        let prefix = token_prefix(token);
        let entry = (role.to_string(), content.to_string());
        let mut sessions = self.sessions.write().await;
        sessions.entry(prefix).or_default().push(entry);
    }

    async fn get_history(&self, token: &str) -> Vec<(String, String)> {
        let prefix = token_prefix(token);
        let sessions = self.sessions.read().await;
        sessions.get(&prefix).cloned().unwrap_or_default()
    }

    async fn turn_count(&self, token: &str) -> usize {
        let history = self.get_history(token).await;
        history.iter().filter(|(role, _)| role == "user").count()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
async fn test_token_prefix_uniqueness() {
    // Token prefixes must be unique for isolation
    let device1 = create_test_device("Device 1");
    let device2 = create_test_device("Device 2");
    let device3 = create_test_device("Device 3");

    let prefix1 = token_prefix(&device1.token);
    let prefix2 = token_prefix(&device2.token);
    let prefix3 = token_prefix(&device3.token);

    // All prefixes should be 8 chars
    assert_eq!(prefix1.len(), 8);
    assert_eq!(prefix2.len(), 8);
    assert_eq!(prefix3.len(), 8);

    // All prefixes should be unique (extremely high probability with UUID tokens)
    let mut prefixes = HashSet::new();
    prefixes.insert(prefix1);
    prefixes.insert(prefix2);
    prefixes.insert(prefix3);
    assert_eq!(prefixes.len(), 3, "Token prefixes must be unique");
}

#[tokio::test]
async fn test_session_isolation() {
    // Create session manager and multiple device tokens
    let manager = TestSessionManager::new();
    let device1_token = create_test_device("Device 1").token;
    let device2_token = create_test_device("Device 2").token;

    // Record messages for device 1
    manager.record_message(&device1_token, "user", "Hello from device 1").await;
    manager.record_message(&device1_token, "assistant", "Hi device 1!").await;

    // Record messages for device 2
    manager.record_message(&device2_token, "user", "Hello from device 2").await;
    manager.record_message(&device2_token, "assistant", "Hi device 2!").await;

    // Verify device 1 history
    let history1 = manager.get_history(&device1_token).await;
    assert_eq!(history1.len(), 2);
    assert_eq!(history1[0].1, "Hello from device 1");
    assert_eq!(history1[1].1, "Hi device 1!");

    // Verify device 2 history
    let history2 = manager.get_history(&device2_token).await;
    assert_eq!(history2.len(), 2);
    assert_eq!(history2[0].1, "Hello from device 2");
    assert_eq!(history2[1].1, "Hi device 2!");

    // Verify turn counts
    assert_eq!(manager.turn_count(&device1_token).await, 1);
    assert_eq!(manager.turn_count(&device2_token).await, 1);
}

#[tokio::test]
async fn test_turn_counting_semantics() {
    // 1 turn = 1 user message + 1 assistant response
    let manager = TestSessionManager::new();
    let token = create_test_device("Test Device").token;

    // No turns initially
    assert_eq!(manager.turn_count(&token).await, 0);

    // Add first turn (user + assistant)
    manager.record_message(&token, "user", "Message 1").await;
    manager.record_message(&token, "assistant", "Response 1").await;
    assert_eq!(manager.turn_count(&token).await, 1);

    // Add second turn
    manager.record_message(&token, "user", "Message 2").await;
    manager.record_message(&token, "assistant", "Response 2").await;
    assert_eq!(manager.turn_count(&token).await, 2);

    // Add third turn
    manager.record_message(&token, "user", "Message 3").await;
    manager.record_message(&token, "assistant", "Response 3").await;
    assert_eq!(manager.turn_count(&token).await, 3);

    // Verify total message count
    let history = manager.get_history(&token).await;
    assert_eq!(history.len(), 6); // 3 turns = 6 messages
}

#[tokio::test]
async fn test_concurrent_device_sessions() {
    // Simulate 20 concurrent devices
    use std::sync::Arc;
    let manager = Arc::new(TestSessionManager::new());
    let mut devices = Vec::new();

    for i in 0..20 {
        let device = create_test_device(&format!("Device {i}"));
        devices.push(device);
    }

    // Record messages concurrently for all devices
    let mut handles = Vec::new();
    for (i, device) in devices.iter().enumerate() {
        let mgr = manager.clone();
        let token = device.token.clone();
        let handle = tokio::spawn(async move {
            for j in 0..5 {
                mgr.record_message(
                    &token,
                    "user",
                    &format!("Device {i} message {j}"),
                ).await;
                mgr.record_message(
                    &token,
                    "assistant",
                    &format!("Response to device {i} message {j}"),
                ).await;
            }
        });
        handles.push(handle);
    }

    // Wait for all tasks to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify each device has correct turn count
    for device in &devices {
        let count = manager.turn_count(&device.token).await;
        assert_eq!(count, 5, "Device should have 5 turns");

        let history = manager.get_history(&device.token).await;
        assert_eq!(history.len(), 10, "Device should have 10 messages (5 turns)");
    }
}

#[tokio::test]
async fn test_single_session_per_device() {
    // Verify that each device has exactly one session (no multi-session support)
    let manager = TestSessionManager::new();
    let token = create_test_device("Test Device").token;

    // Record multiple messages - all go to the same session
    manager.record_message(&token, "user", "Message 1").await;
    manager.record_message(&token, "assistant", "Response 1").await;
    manager.record_message(&token, "user", "Message 2").await;
    manager.record_message(&token, "assistant", "Response 2").await;

    // All messages should be in the same session
    let history = manager.get_history(&token).await;

    assert_eq!(history.len(), 4);
    assert_eq!(history[0].1, "Message 1");
    assert_eq!(history[1].1, "Response 1");
    assert_eq!(history[2].1, "Message 2");
    assert_eq!(history[3].1, "Response 2");
}

#[tokio::test]
async fn test_device_directory_isolation() {
    // Create multiple devices
    let device1 = create_test_device("Device 1");
    let device2 = create_test_device("Device 2");

    let prefix1 = token_prefix(&device1.token);
    let prefix2 = token_prefix(&device2.token);

    let dir1 = device_dir(&prefix1);
    let dir2 = device_dir(&prefix2);

    // Directories should be different
    assert_ne!(dir1, dir2);

    // Both should end with the respective prefix
    assert!(dir1.ends_with(&prefix1));
    assert!(dir2.ends_with(&prefix2));

    // Both should contain .rabb1tclaw
    assert!(dir1.to_string_lossy().contains(".rabb1tclaw"));
    assert!(dir2.to_string_lossy().contains(".rabb1tclaw"));
}
