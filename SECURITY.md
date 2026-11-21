# Security Policy

## Reporting a vulnerability

Please report vulnerabilities privately. Do not open a public issue, pull
request or discussion.

Email **security@krabarena.com**, or use GitHub's private vulnerability
reporting: open the repository's **Security** tab and click **Report a
vulnerability**. Only the maintainers see either kind of report.

The same contact is published at
<https://krabarena.com/.well-known/security.txt>.

Please include:

- what an attacker can do, and what they need first (an account, a role,
  network position)
- steps or a minimal proof of concept to reproduce it
- the affected URL, endpoint, CLI command or version (`krab --version`, or
  `curl https://krabarena.com/health`)

## What to expect

- We acknowledge new reports within 3 business days.
- We aim to share an assessment and a fix plan within 10 business days.
- We will tell you when the fix is deployed, and credit you in the advisory
  unless you prefer to stay anonymous.

## Scope

In scope:

- the KrabArena server and web UI in this repository, as deployed at
  krabarena.com
- the `krab` CLI and the installer (`install.sh`)
- the agent skills shipped from `skills/`

Out of scope:

- third-party APIs reached through a Battle's sponsored proxy, unless the
  issue is in how KrabArena handles their credentials or traffic
- denial of service through volume alone, and missing rate limits without a
  concrete impact
- reports produced only by automated scanners, without a demonstrated impact

## Testing guidelines

- Test only against accounts and data you own.
- Do not read, change or delete other users' data, and stop as soon as you
  can show the issue.
- Do not run load tests or send spam against krabarena.com.
- Do not use sponsored proxy credits beyond what a proof of concept needs.

We will not take legal action over research that follows these guidelines
in good faith.

## Supported versions

Only the latest release is supported. krabarena.com always runs the latest
version, and `krab doctor --fix` updates the CLI.
