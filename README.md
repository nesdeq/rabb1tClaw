```
          _    _    _ _    ___ _
 _ _ __ _| |__| |__/ | |_ / __| |__ ___ __ __
| '_/ _` | '_ \ '_ \ |  _| (__| / _` \ V  V /
|_| \__,_|_.__/_.__/_|\__|\___|_\__,_|\_/\_/
```

<img src="rabb1tClaw.gif" alt="rabb1tClaw" width="400" align="right">

A *very* fast, native Rust backend replacement for the Rabbit R1.

rabb1tClaw implements the [OpenClaw](https://github.com/openclaw/openclaw) WebSocket protocol, giving your R1 a direct line to the LLM providers you choose. No middleman cloud, no subscription. Your hardware, your models, your gateway.

Built on async Rust (Tokio + Axum), it handles hundreds of concurrent device connections with streaming responses that start arriving within milliseconds of the first token. Runs comfortably on a Raspberry Pi.

Use Case: Serve &/ develop you own r1 backend replacement starting from a minimal and *performance oriented* core that already solves onboarding and transport. Watch the Timestamps on the right.

This project comes at 23 Rust source files vs over 2.5k Typescripts @[OpenClaw](https://github.com/openclaw/openclaw), a very stripped down and well structured codebase to build ontop of or use as is.

<br clear="both">

## Status

Fully working MVP. Focus has been on protocol and onboarding so far. Each device gets a persistent conversation session with full history tracking, stored to disk across restarts. A 50-turn FIFO cutoff keeps context windows manageable automatically. Check TODO below.

## Supported Providers

| Provider | Models |
|----------|--------|
| **OpenAI** | gpt-4o, gpt-5.2, o3, etc. |
| **Anthropic** | claude-sonnet-4, claude-opus-4, etc. |
| **DeepInfra** | Llama, Mistral, Qwen, etc. |
| **Any OpenAI-compatible** | Custom endpoint + model |

Multiple providers at once. Switch the active one with a single command.

## Quick Start

```bash
git clone https://github.com/nesdeq/rabb1tClaw.git && cd rabb1tClaw
cargo build --release
cp .env.example target/release/.env   # fill in your API key(s)
./target/release/rabb1tclaw init
```

Init auto-detects your keys, fetches available models, lets you pick, and onboards your first device with a QR code. Takes about 30 seconds.

Then start the server:

```bash
./target/release/rabb1tclaw
```

Run Server via nohup or in detached terminal and use below cli commands from another.

## Config Files

All config lives under `~/.rabb1tclaw/`:

| File | Purpose |
|------|---------|
| `config.yaml` | Gateway settings, providers, bind IP |
| `devices.yaml` | Paired devices and tokens |
| `sessions/` | Per-device conversation history |

Files are `600` permissions since they contain API keys.

## CLI Reference

### Server

```
rabb1tclaw                            Start the server (runs init if no config)
rabb1tclaw server --stop              Stop running server
rabb1tclaw server --restart           Hot-reload config (SIGHUP)
rabb1tclaw server --get-ip            Print current bind IP
rabb1tclaw server --set-ip <IP>       Change bind IP
```

### Devices

```
rabb1tclaw devices --list             List paired devices
rabb1tclaw devices --onboard          Add device + QR code
rabb1tclaw devices --revoke <ID>      Revoke a device
rabb1tclaw devices --revoke-all       Revoke all devices
```

### Providers

```
rabb1tclaw providers --list           List configured providers
rabb1tclaw providers --add            Add a new provider interactively
rabb1tclaw providers --remove <NAME>  Remove a provider
rabb1tclaw providers --set-active <N> Set the active provider
```

### Setup

```
rabb1tclaw init                       Interactive setup (re-run anytime)
```

## Hot Reload

The server watches config files every 2 seconds. Edit them and changes apply live. Revoking a device disconnects it immediately. Use `rabb1tclaw server --restart` for instant reload.

## Roadmap

- Add common model config parameters (temperature, max tokens, system prompt, context limits)
- Add ClawHub skill compatibility with rust hakoniwa namespace isolation

## License

[MIT](LICENSE)
