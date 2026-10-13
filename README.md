<div align="center">

<img src="./assets/banner.jpg" alt="agx Logo" width="240" style="border-radius: 24px; box-shadow: 0 10px 30px rgba(0,0,0,0.5);" />

# ⚡ `agx`
### Multi-Account & Profile Manager for Google Antigravity (`agy`)

[![Rust](https://img.shields.io/badge/Rust-1.80%2B-orange.svg?style=for-the-badge&logo=rust)](https://www.rust-lang.org)
[![Terminal](https://img.shields.io/badge/PTY-portable--pty-blueviolet.svg?style=for-the-badge)](https://crates.io/crates/portable-pty)
[![UI](https://img.shields.io/badge/CLI-comfy--table-cyan.svg?style=for-the-badge)](https://crates.io/crates/comfy-table)
[![License](https://img.shields.io/badge/License-MIT-blue.svg?style=for-the-badge)](LICENSE)

*Seamlessly isolate, switch, and monitor quotas across unlimited `agy` developer accounts.*

</div>

---

## ✨ Features

- 🌌 **Zero-Leak Profile Isolation**: Transparently sandboxes environments by managing isolated user homes at `~/.agx/<instance-name>`.
- 🕹️ **Full Interactive PTY**: Powered by `portable-pty` & `crossterm` with raw mode passthrough, real-time terminal resize tracking, and interactive ANSI handling.
- 🚀 **Streamlined Flag Mapping**: Shorthand `-d` auto-expands to `--dangerously-skip-permissions` for frictionless execution.
- 📊 **Real-time Quota Dashboard (`agx usage`)**:
  - Queries quotas concurrently across all configured profiles.
  - Parses native `json` data structures (`gemini-weekly`, `5h`, `3p-weekly`, etc.).
  - Gorgeous rounded unicode tables with color-graded visual progress bars (`[████████░░] 80%`).
  - Total fleet aggregate calculation with average limit remaining.

---

## 🚀 Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/your-username/agx.git
cd agx

# Build optimized release binary
cargo build --release

# Install globally to your PATH
cp target/release/agx /usr/local/bin/ # or ~/.cargo/bin/
```

---

## 💻 Usage

```text
agx - Multi-account manager & wrapper for agy

USAGE:
    agx <instance-name> [args...]    Run agy with HOME=~/.agx/<instance-name>
    agx <instance-name> enable       Enable a previously disabled instance
    agx <instance-name> disable      Disable an instance from runs & quota checks
    agx usage                        Display usage quotas for all instances
    agx add <instance-name>          Create a new instance environment
    agx list                         List all configured instances
    agx --help, -h                   Show this help message

OPTIONS FOR RUN:
    -d                               Passes '--dangerously-skip-permissions' to agy
    [args...]                        All other arguments are forwarded directly to agy
```

### 1. Launch / Switch Instances
Run any session under a dedicated profile. If the profile doesn't exist, `agx` provisions it on the fly:

```bash
# Launch interactive agy shell under 'work' profile
agx work

# Launch with permissions bypassed (-d maps to --dangerously-skip-permissions)
agx work -d

# Forward commands directly to agy
agx personal chat "explain quantum computing"
```

### 2. Enable / Disable Accounts
Easily disable an account to prevent accidental runs or quota consumption, or re-enable it at any time:

```bash
# Temporarily disable an account
agx client-a disable

# Re-enable the account
agx client-a enable
```

### 3. Manage Profiles

```bash
# Explicitly initialize an account profile
agx add client-a

# List all local instances (shows (disabled) status next to disabled accounts)
agx list
```

### 4. Fleet-wide Usage Quota Dashboard

```bash
agx usage
```

#### Output Preview:
```text
Fetching usage for 2 instance(s)...

╭──────────┬───────────────────────┬───────────────────────────┬───────────┬──────────────────────┬──────────────────────╮
│ Instance │ Model Family          │ Quota / Limit             │ Remaining │ Progress             │ Resets At            │
├──────────┼───────────────────────┼───────────────────────────┼───────────┼──────────────────────┼──────────────────────┤
│ work     │ Gemini Models         │ Weekly Limit Remaining    │       68% │ [█████████░░░░░] 68% │ 2026-10-18T07:32:48Z │
│          │ Gemini Models         │ Five Hour Limit Remaining │       98% │ [█████████████░] 98% │ 2026-10-13T10:31:36Z │
│          │ Claude and GPT models │ Weekly Limit Remaining    │      100% │ [██████████████] 100%│ 2026-10-20T05:39:49Z │
│          │ Claude and GPT models │ Five Hour Limit Remaining │      100% │ [██████████████] 100%│ 2026-10-13T10:39:49Z │
├──────────┼───────────────────────┼───────────────────────────┼───────────┼──────────────────────┼──────────────────────┤
│ personal │ Gemini Models         │ Weekly Limit Remaining    │       95% │ [█████████████░] 95% │ 2026-10-19T14:20:10Z │
│          │ Gemini Models         │ Five Hour Limit Remaining │      100% │ [██████████████] 100%│ 2026-10-13T12:00:00Z │
╰──────────┴───────────────────────┴───────────────────────────┴───────────┴──────────────────────┴──────────────────────╯

╭───────────────────────────────┬───────────────────────────┬───────────┬───────────────┬──────────────────╮
│ Total Aggregated (Average)    │ Quota / Limit             │ Instances │ Avg Remaining │ Progress         │
├───────────────────────────────┼───────────────────────────┼───────────┼───────────────┼──────────────────┤
│ Gemini Models                 │ Weekly Limit Remaining    │     2     │           81% │ [█████████████░░]│
│ Gemini Models                 │ Five Hour Limit Remaining │     2     │           99% │ [███████████████]│
│ Claude and GPT models         │ Weekly Limit Remaining    │     2     │          100% │ [███████████████]│
╰───────────────────────────────┴───────────────────────────┴───────────┴───────────────┴──────────────────╯
```

---

## 🛠️ Architecture

```mermaid
flowchart TD
    CLI["agx &lt;instance&gt; [-d] [args...]"] --> Dispatcher["Command Dispatcher"]
    
    subgraph Profile Runtime
        Dispatcher -->|"Run"| Isolator["Sandbox HOME to ~/.agx/&lt;instance&gt;"]
        Isolator --> PTY["PTY Master/Slave (portable-pty)"]
        PTY --> Shell["Exec 'agy' with Raw Terminal (crossterm)"]
    end
    
    subgraph Dashboard
        Dispatcher -->|"agx usage"| Pool["Parallel Fetch Pool"]
        Pool --> Quota1["agy -p /usage --output-format json"]
        Quota1 --> Parser["Typed JSON Deserializer (serde)"]
        Parser --> PrettyTable["Comfy Table Renderer"]
    end
```

---

## 💖 Support & Donate

If `agx` makes your development workflow smoother, feel free to support ongoing maintenance and feature development!

<div align="center">

### EVM Multi-Chain Donation Address
**Supported Chains:** Ethereum (`ETH`), Polygon (`MATIC`/`POL`), BNB Smart Chain (`BNB`), Arbitrum (`ARB`)

```text
0xA111d8c3De9f45f4C4a639451630a7A243cf4A17
```

[![ETH](https://img.shields.io/badge/Ethereum-3C3C3D?style=for-the-badge&logo=ethereum&logoColor=white)](#-support--donate)
[![Polygon](https://img.shields.io/badge/Polygon-8247E5?style=for-the-badge&logo=polygon&logoColor=white)](#-support--donate)
[![BNB Chain](https://img.shields.io/badge/BNB_Chain-F3BA2F?style=for-the-badge&logo=binance&logoColor=black)](#-support--donate)
[![Arbitrum](https://img.shields.io/badge/Arbitrum-28A0F0?style=for-the-badge&logo=arbitrum&logoColor=white)](#-support--donate)

</div>

---

## 📄 License

MIT © [agx contributors](LICENSE)
