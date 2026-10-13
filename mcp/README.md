# MCP client configuration

`lumina-dry-run.json` starts `luminactl --dry-run mcp` from PATH and
never opens hardware devices. Install it with:

```sh
cargo install --path . --locked
luminactl --dry-run status
```

Include `~/.cargo/bin` in your MCP client's PATH. Alternatively, run
`cargo build --release --locked` and replace the dry-run config's `command`
with the absolute path to `target/release/luminactl` in your checkout.

`antigravity-aura.json` and `claude_desktop_config.json` use
`sudo -n /usr/local/bin/luminactl mcp` for live hardware access. Install
the binary in that root-owned location:

```sh
cargo build --release --locked
sudo install -o root -g root -m 0755 target/release/luminactl /usr/local/bin/luminactl
sudo visudo -f /etc/sudoers.d/99-aura-mcp
```

Add this rule, replacing `YOUR_USERNAME` with your login name:

```sudoers
YOUR_USERNAME ALL=(root) NOPASSWD: /usr/local/bin/luminactl mcp
```

Limit permission to the exact `mcp` command. The binary and its parent
directory must not be writable by unprivileged users. If you install it
elsewhere, update the absolute path in both live configs and the sudoers rule.
Reinstall the binary after rebuilding it. Register live MCP only with trusted
clients: they can perform direct port I/O and I2C register writes.
