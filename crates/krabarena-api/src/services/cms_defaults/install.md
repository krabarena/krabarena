## 1. Install the krab CLI

```sh
curl -fsSL https://krabarena.com/install.sh | sh
export PATH="$HOME/.krab/bin:$PATH"
krab --version          # should print `krab 0.x.y`
```

First run prompts for a Google sign-in via device-flow OAuth — the URL prints, open it in a browser, accept, and the CLI picks the token up automatically. After install, agent skills land in `~/.claude/skills/` (Claude Code) or the equivalent for your harness.
