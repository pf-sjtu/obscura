# Configure the obscura MCP server in Claude Code

This guide registers obscura as a Model Context Protocol server in Claude Code so its browser-automation tools are available to the agent. It focuses on the configuration surface — every knob you can turn and what each one does — rather than on the tools themselves (see [Use the MCP server](Use-the-MCP-server.md) for the tool catalog).

## TL;DR

Register obscura once, globally, over stdio:

```bash
claude mcp add --scope user obscura -- "C:\path\to\obscura.exe" mcp --stealth
```

Verify it connected:

```bash
claude mcp list        # obscura: ... - ✔ Connected
claude mcp get obscura # full resolved config
```

On a Windows-on-ARM build the path is typically `C:\Code\Rust\obscura\target\release\obscura.exe`. Use the absolute path: a user-scope server is launched from arbitrary working directories, so a bare `obscura` only works if the binary is on `PATH`.

## How it works

Claude Code launches the configured command as a subprocess and speaks MCP over its stdin/stdout (newline-delimited JSON). `obscura mcp` with no `--http` flag is exactly that stdio server. There is no port, no daemon, and no network exposure — the process starts when Claude Code needs it and exits when the session ends.

The registration is written to a JSON file; nothing else on the system changes. For `--scope user` that file is `~/.claude.json` (`C:\Users\<you>\.claude.json` on Windows).

## Degrees of freedom

### 1. Scope — who sees the server

`--scope` decides which projects the server is available in. This is the single most important choice.

| Scope | Stored in | Visible to |
|-------|-----------|------------|
| `user` (recommended for a global tool) | `~/.claude.json` | every project you open |
| `project` | `.mcp/` config committed in the repo | anyone who checks out that repo |
| `local` (default if `--scope` is omitted) | per-project local state | only you, only in that project |

For a general-purpose browser the user scope is the right default — you get obscura in every project without re-adding it. Use `project` only when you want the server checked into a specific repo for your team.

### 2. Transport — stdio vs HTTP

obscura ships both transports. Claude Code can consume either.

**stdio (recommended).** The form shown above. Claude Code owns the process lifecycle, there is no listening socket, and nothing is reachable from the network. Prefer this unless you have a specific reason not to.

**HTTP.** Run obscura as a long-lived server and point Claude Code at the URL:

```bash
# terminal 1: start the server
obscura mcp --http --host 127.0.0.1 --port 3000

# terminal 2: register the endpoint
claude mcp add --scope user --transport http obscura-http http://127.0.0.1:3000/mcp
```

Use HTTP when you want one shared browser session across multiple clients, or when obscura runs in a container/sidecar and Claude Code connects over the network. The cost is that you must keep the server running yourself, and you take on the security considerations below.

### 3. Runtime flags — how obscura behaves

These are appended after `mcp` (everything after the `--` in `claude mcp add`). They apply to both transports.

| Flag | Default | Effect |
|------|---------|--------|
| `--stealth` | off | Anti-detection mode: real-browser TLS/JA3 fingerprint and JS surface. Requires a binary built with `--features stealth`. Recommended for scraping sites that block bots. |
| `--proxy <URL>` | none | Route all traffic through an HTTP or SOCKS5 proxy, e.g. `--proxy socks5://127.0.0.1:1080`. |
| `--user-agent <UA>` | built-in Chrome UA | Override the User-Agent string. |
| `--host <ADDR>` | `127.0.0.1` | HTTP transport only — bind address. Use `0.0.0.0` to expose beyond loopback (read the security note first). |
| `--port <N>` | `3000` | HTTP transport only — listening port. |

`--stealth` only does something if the binary was compiled with the `stealth` feature (`cargo build --release --features stealth`). A non-stealth binary accepts the flag but has no anti-detection backend to switch on.

### 4. Environment variables

Pass env vars with `-e KEY=value` on `claude mcp add`, or set them in the shell that launches an HTTP server.

| Variable | Applies to | Effect |
|----------|------------|--------|
| `OBSCURA_MCP_ALLOWED_ORIGINS` | HTTP transport | Comma-separated `Origin` allowlist. When set, a browser request from an unlisted origin is refused with `403` before it can drive the browser. Native (non-browser) clients send no `Origin` and are always allowed. Unset keeps the permissive default. |

Example, registering an HTTP server with an origin allowlist baked into its launch environment:

```bash
OBSCURA_MCP_ALLOWED_ORIGINS="https://app.example.com" obscura mcp --http --host 0.0.0.0
```

## Inspect, change, remove

```bash
claude mcp list             # health of every server
claude mcp get obscura      # resolved command, args, scope, status
claude mcp remove obscura -s user
```

To change flags (for example, to drop `--stealth` or add a proxy), remove and re-add:

```bash
claude mcp remove obscura -s user
claude mcp add --scope user obscura -- "C:\path\to\obscura.exe" mcp --proxy http://127.0.0.1:8080
```

You can also edit the entry directly in `~/.claude.json` under `mcpServers.obscura` — the `command`, `args`, and `env` fields map one-to-one to the options above.

## Security notes

- **stdio** exposes nothing to the network; it is the safe default.
- **HTTP** has no built-in authentication — anyone who can reach the port can drive a real browser through obscura. Keep it on `127.0.0.1`, set `OBSCURA_MCP_ALLOWED_ORIGINS`, and put it behind a reverse proxy or network isolation before binding `0.0.0.0`. A single request body is capped at 16 MiB so an unauthenticated caller cannot force a large allocation.
- obscura's own SSRF guard still applies to the URLs the browser fetches; the `--proxy` and stealth settings do not weaken it.

## Tools

Once connected, the obscura tools appear in Claude Code's tool list (navigation, snapshot/markdown/extract, click/fill/type, wait, evaluate JS, network and console diagnostics, cookies and storage, tabs). The full catalog with argument shapes is in [Use the MCP server](Use-the-MCP-server.md).
