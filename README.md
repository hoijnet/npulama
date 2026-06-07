# npulama

A Windows system-tray application that turns [Microsoft Foundry Local](https://github.com/microsoft/Foundry-Local) into a clean, network-accessible OpenAI-compatible API endpoint.

Foundry Local runs small language models directly on your hardware — NPU, GPU, or CPU — and automatically enable you to pick the right execution path based on what is available and the current power state of your device. npulama sits in front of foundry and exposes a standard OpenAI API that any client should be able to talk to (Zed, Continue, Open WebUI, curl, …), with optional token authentication for network access.

Only tested with Zed so far and a test OpenAI chat client.

The idea is to provide something that works like ollama and nolama, but for easy to install and run.

Give a star if you like it, or think it should evolve ⭐💫

---

## What it does

- Manages the full Foundry Local lifecycle (start service, download models, load/unload) from a tray icon — no terminal needed.
- Proxies all OpenAI API calls (`/v1/chat/completions`, `/v1/models`, …) from the Foundry-internal port to a configurable port on your machine and enables it over the network.
- Strips fields that Foundry does not yet support (e.g. `max_completion_tokens`) so standard OpenAI clients work without modification.
- Optionally requires a bearer token, making it safe to expose over your local network or through a tunnel.
- Select the right hardware for your use case: NPU for low-power efficient inference, GPU for throughput, CPU as fallback.

Looks like this. Not everything for systray handling is tested yet, but works on my machine:

<img width="1266" height="1360" alt="image" src="https://github.com/user-attachments/assets/8643df03-b1c1-48a7-8e03-6387f52c3075" />

---

## Prerequisites

### 1. Foundry Local CLI — version 0.10

npulama requires **Foundry Local CLI 0.10** or later (there is a current issue with the default cli). Earlier versions [do not load the models properly, see issue](https://github.com/microsoft/Foundry-Local/issues/757).

Install via winget:

```powershell
winget install Microsoft.FoundryLocal
```

Verify the version:

```powershell
foundry --version
# should print 0.10.x or later
```

If you already have Foundry Local installed and the version is older:

```powershell
winget upgrade Microsoft.FoundryLocal
```

> **Note:** Foundry Local requires Windows 11 and the AI-related Windows components. On devices with a Qualcomm, Intel, or AMD NPU the installer sets up the necessary execution providers automatically. This was tested using an Intel Core Ultra with Copilot+.

### 2. Rust toolchain (to build from source)

```powershell
winget install Rustlang.Rustup
rustup default stable
```

---

## Installation

You will likely need some redistributable Windows Studio components that get installed during rustup on Windows.

### Build from source

```powershell
git clone https://github.com/your-org/npulama.git
cd npulama
cargo build --release
```

The binary ends up at `target\release\npulama.exe`. Copy it wherever you like — it has no runtime dependencies beyond the Foundry Local CLI being on your PATH.

### First run

Double-click `npulama.exe` (or run it from a terminal). A tray icon appears. Open the window from the tray to:

1. See the model catalog — models already downloaded via the Foundry CLI appear immediately; no re-download needed.
2. Click **Load** next to a model to start serving it.
3. Click **Download** to fetch a model that is not yet cached.

npulama and the Foundry CLI share the same model cache (`~/.foundry/cache`), so anything you have pulled with `foundry model download <alias>` is available instantly.

---

## Configuration

Settings are saved automatically to `%APPDATA%\npulama\config.json`.

| Setting | Default | Description |
|---|---|---|
| Port | `11435` | The port npulama listens on |
| Bind all interfaces | off | When on, binds `0.0.0.0` instead of `127.0.0.1` — needed for networked access |
| Require auth token | off | Reject requests without a valid `Authorization: Bearer <token>` header |
| Tokens | — | One or more `sk-...` tokens that are accepted, recommended for interoperability |
| Preferred model | — | Model auto-loaded on startup |
| Context window | 4096 | Tokens of context passed to the model (up to 128K depending on model) |

---

## Network access

By default npulama listens only on `127.0.0.1` — safe for local use. To allow other devices on your network to reach it:

1. Open the npulama window from the tray icon.
2. Enable **Bind all interfaces**.
3. Enable **Require auth token** and add at least one token (e.g. `sk-mytoken`).
4. Set that token as the API key in your client.

Your client's base URL becomes `http://<your-machine-ip>:11435` and the API key is the token you set.

> **Security:** Do not expose npulama to the public internet without additional firewall rules. The bearer token provides authentication but not transport encryption. For encrypted remote access use a reverse proxy with TLS (e.g. Caddy, nginx, or a Cloudflare tunnel).

---

## Testing it out

### curl

```bash
curl http://127.0.0.1:11434/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "phi-4-mini",
    "stream": true,
    "messages": [{"role": "user", "content": "What is the capital of Sweden?"}]
  }'
```

With a token:

```bash
curl http://192.168.1.42:11434/v1/chat/completions \
  -H "Authorization: Bearer sk-mytoken" \
  -H "Content-Type: application/json" \
  -d '{"model": "phi-4-mini", "messages": [{"role": "user", "content": "Hello"}]}'
```

---

## Hardware selection

Foundry Local chooses the execution provider automatically based on what is available and the current power profile:

| Hardware | When used |
|---|---|
| **NPU** | Available and device is on battery or efficiency mode — fastest and most power-efficient |
| **GPU** (DirectML) | Available, NPU absent or in use — good throughput on AC power |
| **CPU** | Always available, used as fallback |

The model list in npulama shows which provider each model variant targets (`Npu`, `Gpu`, `Cpu`). Some models ship with multiple variants optimised for different hardware — load the one that matches your device for best performance and battery life.

---

## Recommended models

Any model in the [Foundry Local catalog](https://github.com/microsoft/Foundry-Local) works. Good starting points:

| Alias | Size | Best for |
|---|---|---|
| `phi-4-mini` | ~2 GB | Fast, NPU-optimised, general assistant and coding |
| `phi-3.5-mini` | ~2 GB | Code, reasoning |
| `mistral-7b` | ~4 GB | Longer context, higher quality responses |

List all available models and their download status:

```powershell
foundry model list
```

---

## Troubleshooting

**npulama says "No model loaded"**
Load a model from the tray window, or set a preferred model in settings so it loads automatically on startup.

**Model shows as not cached but I downloaded it with the CLI**
Both npulama and the Foundry CLI write to `~/.foundry/cache`. If npulama still shows the model as not cached after downloading, click the refresh button in the window or restart npulama — the catalog refreshes on startup.

**Stream errors or truncated responses**
This is bleeding edge, tested with Zed and not much more. Submit PRs and issues for things not working.

**Port already in use**
Change the port in Settings and click Apply, or stop the other process:

```powershell
netstat -ano | findstr :11435
Stop-Process -Id <PID> -Force
```

**Foundry version mismatch**
If npulama cannot start the Foundry service, confirm `foundry --version` reports 0.10 or later and that `foundry server start` works on its own from a terminal.

---

## Development

```powershell
# Unit + integration tests (no Foundry required)
cargo test --test proxy_integration

# End-to-end tests (requires Foundry running with phi-4-mini loaded)
cargo test --test e2e_foundry -- --nocapture --test-threads 1

# Run in debug mode
cargo run
```

---

## License

MPL 2.0
