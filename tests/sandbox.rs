//! Integration test: hakoniwa sandbox + pyenv python + venv creation.

use std::path::Path;

/// Resolve real python binary + prefix via sys.executable (works through pyenv shims).
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

/// Specific /etc entries needed inside the sandbox.
const ETC_MOUNTS: &[&str] = &[
    "/etc/hosts",
    "/etc/nsswitch.conf",
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/localtime",
];

/// Real upstream resolv.conf (not systemd-resolved stub 127.0.0.53).
fn resolv_conf_source() -> &'static str {
    const REAL: &str = "/run/systemd/resolve/resolv.conf";
    if Path::new(REAL).exists() { REAL } else { "/etc/resolv.conf" }
}

/// Build a sandbox container with selective mounts only.
/// `network`: enable passt networking (only needed for pip/HTTP).
fn build_container(workspace: &Path, python_prefix: &Path, network: bool) -> hakoniwa::Container {
    let ws = workspace.to_string_lossy();
    let prefix_str = python_prefix.to_string_lossy().to_string();

    let mut c = hakoniwa::Container::new();

    // Python installation
    c.bindmount_ro(&prefix_str, &prefix_str);

    // /usr — shared libs and binaries
    c.bindmount_ro("/usr", "/usr");

    // Top-level dirs: symlinks on merged-usr, real dirs on traditional
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

    // Handle cross-filesystem bind mounts in user namespaces
    c.runctl(hakoniwa::Runctl::MountFallback);

    // DNS: real upstream resolv.conf (not systemd-resolved stub)
    c.bindmount_ro(resolv_conf_source(), "/etc/resolv.conf");

    // Selective /etc
    for path in ETC_MOUNTS {
        if Path::new(path).exists() {
            c.bindmount_ro(path, path);
        }
    }

    // Workspace RW
    c.bindmount_rw(&ws, "/workspace");

    // Virtual filesystems
    c.devfsmount("/dev").tmpfsmount("/tmp");

    // Resource limits
    c.setrlimit(hakoniwa::Rlimit::As, 2_000_000_000, 2_000_000_000)
        .setrlimit(hakoniwa::Rlimit::Nproc, 64, 64)
        .setrlimit(hakoniwa::Rlimit::Nofile, 256, 256);

    // Network only when needed
    if network {
        c.unshare(hakoniwa::Namespace::Network);
        c.network(hakoniwa::Pasta::default());
    }

    c
}

// ============================================================================
// No-network tests
// ============================================================================

#[test]
fn test_find_python() {
    let (executable, prefix) = find_python();
    println!("executable: {}", executable);
    println!("prefix: {}", prefix);
    assert!(Path::new(&executable).exists(), "binary must exist");
    assert!(Path::new(&prefix).is_dir(), "prefix must be a directory");
}

#[test]
fn test_echo_in_sandbox() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, prefix) = find_python();
    let container = build_container(tmp.path(), Path::new(&prefix), false);

    let output = container
        .command("/bin/echo")
        .arg("hello")
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .output()
        .expect("failed to run echo");

    println!("code: {}", output.status.code);
    println!("reason: {}", output.status.reason);
    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    println!("stderr: {}", String::from_utf8_lossy(&output.stderr));

    assert!(output.status.success(), "echo failed: {}", output.status.reason);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
}

#[test]
fn test_python_version_in_sandbox() {
    let tmp = tempfile::tempdir().unwrap();
    let (python, prefix) = find_python();
    let container = build_container(tmp.path(), Path::new(&prefix), false);

    let output = container
        .command(&python)
        .arg("--version")
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .output()
        .expect("failed to run python");

    println!("code: {}", output.status.code);
    println!("reason: {}", output.status.reason);
    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    println!("stderr: {}", String::from_utf8_lossy(&output.stderr));

    assert!(output.status.success(), "python --version failed: {}", output.status.reason);
}

#[test]
fn test_venv_creation() {
    let tmp = tempfile::tempdir().unwrap();
    let (python, prefix) = find_python();

    let container = build_container(tmp.path(), Path::new(&prefix), false);

    let output = container
        .command(&python)
        .args(["-m", "venv", "/workspace/.venv"])
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(60)
        .output()
        .expect("failed to run venv creation");

    println!("code: {}", output.status.code);
    println!("reason: {}", output.status.reason);
    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    println!("stderr: {}", String::from_utf8_lossy(&output.stderr));

    assert!(
        output.status.success(),
        "venv creation failed (code {}): reason={} stdout={} stderr={}",
        output.status.code,
        output.status.reason,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    assert!(tmp.path().join(".venv/bin/python").exists(), ".venv/bin/python must exist");
    assert!(tmp.path().join(".venv/bin/pip").exists(), ".venv/bin/pip must exist");
}

#[test]
fn test_script_execution() {
    let tmp = tempfile::tempdir().unwrap();
    let (python, prefix) = find_python();

    // Create venv first
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

    // Write a test script
    std::fs::write(tmp.path().join("test.py"), "print('hello from sandbox')").unwrap();

    // Execute it
    let container = build_container(tmp.path(), Path::new(&prefix), false);
    let output = container
        .command("/workspace/.venv/bin/python")
        .arg("/workspace/test.py")
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(30)
        .output()
        .expect("script execution failed");

    println!("code: {}", output.status.code);
    println!("reason: {}", output.status.reason);
    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    println!("stderr: {}", String::from_utf8_lossy(&output.stderr));

    assert!(output.status.success(), "script failed: {}", output.status.reason);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello from sandbox");
}

#[test]
fn test_file_creation_in_sandbox() {
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

    // Script that creates a file
    std::fs::write(
        tmp.path().join("write_test.py"),
        r#"
with open('/workspace/output.md', 'w') as f:
    f.write('# Hello\n\nThis is a test.\n')
print('file written')
"#,
    ).unwrap();

    let container = build_container(tmp.path(), Path::new(&prefix), false);
    let output = container
        .command("/workspace/.venv/bin/python")
        .arg("/workspace/write_test.py")
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(30)
        .output()
        .expect("script execution failed");

    println!("code: {}", output.status.code);
    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    println!("stderr: {}", String::from_utf8_lossy(&output.stderr));

    assert!(output.status.success(), "script failed: {}", output.status.reason);

    let content = std::fs::read_to_string(tmp.path().join("output.md")).expect("output.md must exist");
    assert!(content.contains("Hello"), "file content: {}", content);
}

// ============================================================================
// Network tests (require passt on host)
// ============================================================================

#[test]
fn test_venv_creation_with_network() {
    let tmp = tempfile::tempdir().unwrap();
    let (python, prefix) = find_python();

    let container = build_container(tmp.path(), Path::new(&prefix), true);

    let output = container
        .command(&python)
        .args(["-m", "venv", "/workspace/.venv"])
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(60)
        .output()
        .expect("failed to run venv creation");

    println!("code: {}", output.status.code);
    println!("reason: {}", output.status.reason);
    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    println!("stderr: {}", String::from_utf8_lossy(&output.stderr));

    assert!(
        output.status.success(),
        "venv+network failed (code {}): reason={}",
        output.status.code, output.status.reason,
    );

    assert!(tmp.path().join(".venv/bin/python").exists());
    assert!(tmp.path().join(".venv/bin/pip").exists());
}

#[test]
fn test_pip_install_with_network() {
    let tmp = tempfile::tempdir().unwrap();
    let (python, prefix) = find_python();

    // Create venv (no network needed)
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

    // pip install (needs network)
    let container = build_container(tmp.path(), Path::new(&prefix), true);
    let output = container
        .command("/workspace/.venv/bin/pip")
        .args(["install", "--quiet", "requests"])
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(120)
        .output()
        .expect("pip install failed to execute");

    println!("code: {}", output.status.code);
    println!("reason: {}", output.status.reason);
    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    println!("stderr: {}", String::from_utf8_lossy(&output.stderr));

    assert!(
        output.status.success(),
        "pip install failed (code {}): reason={} stderr={}",
        output.status.code,
        output.status.reason,
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn test_http_request_from_sandbox() {
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

    // Script that makes an HTTP request
    std::fs::write(
        tmp.path().join("http_test.py"),
        r#"
import urllib.request
resp = urllib.request.urlopen('https://httpbin.org/get', timeout=10)
print(f'status: {resp.status}')
"#,
    ).unwrap();

    let container = build_container(tmp.path(), Path::new(&prefix), true);
    let output = container
        .command("/workspace/.venv/bin/python")
        .arg("/workspace/http_test.py")
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(30)
        .output()
        .expect("http test failed to execute");

    println!("code: {}", output.status.code);
    println!("reason: {}", output.status.reason);
    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    println!("stderr: {}", String::from_utf8_lossy(&output.stderr));

    assert!(output.status.success(), "http request failed: {}", output.status.reason);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("status: 200"), "expected 200, got: {}", stdout);
}
