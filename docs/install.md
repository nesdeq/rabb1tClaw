# Installation

rabb1tClaw runs on **Linux only**. The code agent's sandbox uses Linux user namespaces (via hakoniwa), which have no equivalent on macOS or Windows. You can develop and test the non-sandbox parts anywhere, but the full system — including sandboxed Python execution — requires Linux.

## Prerequisites

### 1. Rust toolchain

Install via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

Verify:

```bash
rustc --version   # 1.75+ recommended
cargo --version
```

### 2. Python 3

Any Python 3 installation works — system package, [pyenv](https://github.com/pyenv/pyenv), or a standalone build. rabb1tClaw resolves the real binary path through shims automatically.

```bash
# Debian/Ubuntu
sudo apt install python3 python3-venv

# Fedora
sudo dnf install python3

# Arch
sudo pacman -S python
```

The code agent creates per-device `.venv` directories in the workspace, so `python3-venv` (or equivalent) must be available.

### 3. passt (user-mode networking for sandbox)

[passt](https://passt.top/) provides network access inside the sandboxed user namespace. Without it, the code agent can't install pip packages or make HTTP requests.

```bash
# Debian/Ubuntu
sudo apt install passt

# Fedora
sudo dnf install passt

# Arch (AUR)
yay -S passt
```

Verify:

```bash
passt --version
```

### 4. Kernel: allow unprivileged user namespaces

hakoniwa creates unprivileged user namespaces for sandboxing. On Ubuntu 24.04+ and some other distributions, AppArmor restricts this by default. You need to disable that restriction:

```bash
# Check current setting
sysctl kernel.apparmor_restrict_unprivileged_userns

# If it returns 1, disable the restriction:
sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
```

To make it persistent across reboots:

```bash
echo 'kernel.apparmor_restrict_unprivileged_userns=0' | sudo tee /etc/sysctl.d/99-userns.conf
sudo sysctl --system
```

> **Note:** On distributions that don't set this restriction (most Fedora, Arch, older Ubuntu), this step is not needed. If `sysctl kernel.apparmor_restrict_unprivileged_userns` returns "unknown key", you're fine.

### 5. OpenSSL development headers

Required at build time for TLS (vendored OpenSSL):

```bash
# Debian/Ubuntu
sudo apt install pkg-config libssl-dev

# Fedora
sudo dnf install pkg-config openssl-devel

# Arch
sudo pacman -S pkg-config openssl
```

### 6. API keys

You need at least one LLM provider key. Optionally, a Serper key for web search.

| Key | Source | Required |
|-----|--------|----------|
| `OPENAI_API_KEY` | [platform.openai.com](https://platform.openai.com/api-keys) | At least one provider |
| `ANTHROPIC_API_KEY` | [console.anthropic.com](https://console.anthropic.com/settings/keys) | At least one provider |
| `DEEPINFRA_API_KEY` | [deepinfra.com](https://deepinfra.com/dash/api_keys) | At least one provider |
| `SERP_API_KEY` | [serper.dev](https://serper.dev) | Optional (web search) |

Serper.dev gives 2,500 free searches with no payment info required.

## Build

```bash
git clone https://github.com/nesdeq/rabb1tClaw.git && cd rabb1tClaw
cargo build --release
```

The binary lands at `target/release/rabb1tclaw`.

## Configure

```bash
cp .env.example target/release/.env
```

Edit `target/release/.env` and fill in your API key(s). Then run:

```bash
./target/release/rabb1tclaw
```

First run detects your keys, fetches available models, applies smart defaults, onboards your first device with a QR code, and starts the server.

## Cross-compile (Raspberry Pi)

rabb1tClaw runs comfortably on a Raspberry Pi. To cross-compile for ARM64:

```bash
rustup target add aarch64-unknown-linux-gnu
sudo apt install gcc-aarch64-linux-gnu

export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
cargo build --release --target aarch64-unknown-linux-gnu
```

The Pi still needs passt, Python 3, and the kernel namespace setting configured locally.

## Verify

Quick sanity check that the sandbox works:

```bash
cargo test --test sandbox -- --nocapture
```

The network tests (`test_venv_creation_with_network`, `test_pip_install_with_network`, `test_http_request_from_sandbox`) require passt installed. Non-network tests only need Python 3 and unprivileged user namespaces.
