# Security

## Supported versions

Only the latest release and the `main` branch receive fixes.

## Privilege model

| Component | Resource | Privilege needed | Notes |
| --- | --- | --- | --- |
| GPU (GTX 1060) | `/dev/i2c-0`, address `0x29` | member of group `i2c` (udev rule) or root | needs the `i2c-dev` module; the CLI checks the GPU signature before writing |
| Motherboard (Z270E) | `/dev/port`, I/O ports from `0x02c0` | root | `/dev/port` is raw port access to the whole machine; kernel lockdown may block it |
| Web Studio (`web/serve.py`) | TCP `127.0.0.1:7890` | none | runs `sudo -n asus-aura-cli …`; no authentication |
| MCP server (`asus-aura-cli mcp`) | stdio | inherits the CLI's | accepts JSON-RPC only; no shell, file or network access |

Things to know:

- `udev/99-asus-aura.rules` opens **every** `/dev/i2c-*` to group `i2c`, not only the GPU's. Members get raw access to all i2c buses on the machine (including DIMM SPD). Treat that group as privileged.
- The CLI validates colors, modes, speeds and zones, and each controller verifies device identity before writing. It never builds a shell command from input. When run as root through MCP, the attack surface is the JSON-RPC parser and the validated register writes, nothing else.
- An agent connected to the MCP server can change your lighting, including a flashing pattern. It cannot read files or run commands through this server.
- `serve.py` binds to loopback only and checks the `Origin` header on writes, but any local user or process can call it. Do not expose it through a proxy or tunnel.

## Least-privilege sudoers

The rule must name a **root-owned** file. A rule pointing at `target/release/asus-aura-cli` lets anyone who can rebuild that file run arbitrary code as root.

```sh
sudo install -o root -g root -m 0755 target/release/asus-aura-cli /usr/local/bin/asus-aura-cli
sudo visudo -f /etc/sudoers.d/asus-aura
```

For the MCP server (the arguments are part of the rule, so only `mcp` is allowed):

```sudoers
YOUR_USER ALL=(root) NOPASSWD: /usr/local/bin/asus-aura-cli mcp
```

Client config then uses `sudo -n /usr/local/bin/asus-aura-cli mcp`. `-n` makes sudo fail instead of prompting if the rule is missing.

For the Web Studio, which runs several fixed command shapes, allow exactly those and set `AURA_CLI_BIN=/usr/local/bin/asus-aura-cli`:

```sudoers
YOUR_USER ALL=(root) NOPASSWD: /usr/local/bin/asus-aura-cli --json status, \
    /usr/local/bin/asus-aura-cli gpu *, /usr/local/bin/asus-aura-cli mobo *, /usr/local/bin/asus-aura-cli all *
```

Re-install the binary after each rebuild. Remove the rule when you stop using these features. Use `visudo` (it syntax-checks), and test with `sudo -n -l`.

## Reporting a vulnerability

Please do not open a public issue for a security problem.

1. Use GitHub's private reporting: the repository's **Security** tab, then **Report a vulnerability**.
2. Include what you found, how to reproduce it (a `--dry-run` reproduction is ideal), the affected version or commit, and your assessment of impact.
3. This is a volunteer project: reports are handled on a best-effort basis with no response-time guarantee. We will confirm receipt, tell you whether we consider it a vulnerability, and credit you in the fix unless you prefer otherwise.
4. Please allow time for a fix before public disclosure, and coordinate the date with us.

In scope: privilege escalation, memory or input-handling bugs in the CLI or MCP server, ways to make a write reach an unverified device, and flaws in the documented sudoers or udev setup. Out of scope: needing root or `i2c` group membership you already have to do what the tool does, and hardware damage from deliberately bypassing the identity checks.
