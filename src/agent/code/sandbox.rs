use crate::config::native::device_dir;
use std::path::{Path, PathBuf};
use tracing::debug;

/// Per-device workspace directory.
pub fn workspace_dir(token_prefix: &str) -> PathBuf {
    device_dir(token_prefix).join("workspace")
}

/// Resolve the real python3 binary path and its installation prefix.
/// Works with pyenv shims, system python, virtualenvs — anything.
fn find_python() -> anyhow::Result<(String, PathBuf)> {
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
                    return Ok((executable, PathBuf::from(prefix)));
                }
            }
        }
    }

    anyhow::bail!("no python3 found on host (tried python3, python)")
}

/// Specific /etc entries needed inside the sandbox (NOT the whole /etc).
/// Note: /etc/resolv.conf handled separately (see `resolv_conf_source`).
const ETC_MOUNTS: &[&str] = &[
    "/etc/hosts",           // hostname resolution
    "/etc/nsswitch.conf",   // name service switch
    "/etc/ssl",             // TLS certificates
    "/etc/ca-certificates", // TLS certificates (Debian/Ubuntu)
    "/etc/ld.so.cache",     // dynamic linker cache
    "/etc/ld.so.conf",      // dynamic linker config
    "/etc/ld.so.conf.d",    // dynamic linker config includes
    "/etc/localtime",       // timezone
];

/// Find the right resolv.conf to mount: the real upstream DNS, not the
/// systemd-resolved stub (127.0.0.53 is unreachable inside passt namespace).
fn resolv_conf_source() -> &'static str {
    const REAL: &str = "/run/systemd/resolve/resolv.conf";
    if Path::new(REAL).exists() { REAL } else { "/etc/resolv.conf" }
}

// Resource limits (security boundaries — not user-configurable)
const SANDBOX_MEMORY_LIMIT: u64 = 2_000_000_000; // 2GB virtual memory
const SANDBOX_MAX_PROCS: u64 = 64;
const SANDBOX_MAX_FILES: u64 = 256;

/// Build a sandbox: selective mounts only — no host root.
/// `network`: enable passt networking (needed for pip/HTTP, NOT for venv creation).
fn build_container(workspace: &Path, python_prefix: &Path, network: bool) -> hakoniwa::Container {
    let ws = workspace.to_string_lossy();
    let prefix_str = python_prefix.to_string_lossy().to_string();

    let mut c = hakoniwa::Container::new();

    // Python installation (RO, same host path so prefix resolution works)
    c.bindmount_ro(&prefix_str, &prefix_str);

    // /usr — shared libs and binaries on all distros
    c.bindmount_ro("/usr", "/usr");

    // Top-level dirs vary by distro layout:
    //   merged-usr (Arch/CachyOS): /lib→usr/lib, /bin→usr/bin (symlinks)
    //   traditional (Debian/Ubuntu): /lib, /bin are real directories
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

    // DNS: use real upstream resolv.conf (not systemd-resolved stub)
    c.bindmount_ro(resolv_conf_source(), "/etc/resolv.conf");

    // Selective /etc — TLS, dynamic linker, etc. (NOT the whole directory)
    for path in ETC_MOUNTS {
        if Path::new(path).exists() {
            c.bindmount_ro(path, path);
        }
    }

    // Device workspace (RW) — .venv and scripts live here
    c.bindmount_rw(&ws, "/workspace");

    // Virtual filesystems
    c.devfsmount("/dev").tmpfsmount("/tmp");

    c.setrlimit(hakoniwa::Rlimit::As, SANDBOX_MEMORY_LIMIT, SANDBOX_MEMORY_LIMIT)
        .setrlimit(hakoniwa::Rlimit::Nproc, SANDBOX_MAX_PROCS, SANDBOX_MAX_PROCS)
        .setrlimit(hakoniwa::Rlimit::Nofile, SANDBOX_MAX_FILES, SANDBOX_MAX_FILES);

    // Network access via passt (only when needed — pip install, HTTP from scripts)
    if network {
        c.unshare(hakoniwa::Namespace::Network);
        c.network(hakoniwa::Pasta::default());
    }

    c
}

/// Create `.venv` in workspace. Returns the python prefix for sandbox reuse.
/// Skips venv creation if already exists but still resolves the prefix.
pub(crate) fn ensure_venv(workspace: &Path, exec_timeout_secs: u64) -> anyhow::Result<PathBuf> {
    let (python, prefix) = find_python()?;

    let venv = workspace.join(".venv");
    if venv.exists() {
        return Ok(prefix);
    }

    std::fs::create_dir_all(workspace)?;

    let container = build_container(workspace, &prefix, false);
    let output = container
        .command(&python)
        .args(["-m", "venv", "/workspace/.venv"])
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(exec_timeout_secs)
        .output()
        .map_err(|e| anyhow::anyhow!("venv creation failed: {e}"))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "venv creation failed (code {}): reason={} stdout={} stderr={}",
            output.status.code, output.status.reason, stdout, stderr
        );
    }

    debug!("[CODE] Created venv at {}", venv.display());
    Ok(prefix)
}

/// Install packages via pip in the sandbox. Returns (success, `combined_output`).
/// Always uses the workspace venv's pip.
pub(crate) fn pip_install(
    workspace: &Path,
    python_prefix: &Path,
    packages: &[String],
    exec_timeout_secs: u64,
    env_vars: &[(String, String)],
) -> anyhow::Result<(bool, String)> {
    if packages.is_empty() {
        return Ok((true, String::new()));
    }

    let container = build_container(workspace, python_prefix, true);
    let mut args: Vec<&str> = vec!["install", "--quiet"];
    args.extend(packages.iter().map(String::as_str));

    let mut cmd = container.command("/workspace/.venv/bin/pip");
    cmd.args(&args)
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(exec_timeout_secs);
    for (k, v) in env_vars {
        cmd.env(k, v);
    }

    let output = cmd.output()
        .map_err(|e| anyhow::anyhow!("pip install failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut combined = format!("{stdout}{stderr}");
    if !output.status.success() && combined.trim().is_empty() {
        combined = format!("container error: {}", output.status.reason);
    }

    Ok((output.status.success(), combined))
}

/// Execute a Python script in the sandbox. Returns (success, stdout, stderr).
/// Always uses the workspace venv's python.
/// Optional `env_vars` injects environment variables into the sandbox process.
pub(crate) fn execute_in_sandbox(
    workspace: &Path,
    python_prefix: &Path,
    script_name: &str,
    exec_timeout_secs: u64,
    env_vars: &[(String, String)],
) -> anyhow::Result<(bool, String, String)> {
    let container = build_container(workspace, python_prefix, true);
    let script_path = format!("/workspace/{script_name}");

    let mut cmd = container.command("/workspace/.venv/bin/python");
    cmd.arg(&script_path)
        .current_dir("/workspace")
        .stdout(hakoniwa::Stdio::piped())
        .stderr(hakoniwa::Stdio::piped())
        .wait_timeout(exec_timeout_secs);
    for (k, v) in env_vars {
        cmd.env(k, v);
    }

    let output = cmd.output()
        .map_err(|e| anyhow::anyhow!("sandbox exec failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() && stdout.trim().is_empty() && stderr.trim().is_empty() {
        stderr = format!("container error: {}", output.status.reason);
    }

    Ok((output.status.success(), stdout, stderr))
}
