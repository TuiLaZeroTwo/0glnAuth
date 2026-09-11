# gln-auth

A Pumpkin (WASM) authentication plugin for offline-mode Minecraft servers: it
auto-logs-in premium (paid) accounts by verifying their names against Mojang's
session servers, and gives cracked (non-premium) players a classic
register/login password flow with session memory, an unauthenticated freeze,
and a login timeout. One active session per player name, per-IP rate limiting,
and Argon2 password hashing are built in.

## Requirements

- Rust 1.98+ with the `wasm32-wasip2` target installed
  (`rustup target add wasm32-wasip2`)
- A Pumpkin server with authentication disabled
  (`online_mode=false` in the server configuration)
- Outbound internet access from the server host (for the Mojang premium
  check) — optional if `premium_check_enabled = false`

## Build

```
cargo build --release
```

The plugin artifact is `target/wasm32-wasip2/release/gln_auth.wasm`.

## Install

1. Copy `gln_auth.wasm` into the server's `plugins/` directory.
2. Start the server. On first load the plugin creates:
   - `plugins/gln-auth/config.toml` — configuration (defaults; edit and
     restart to change),
   - `plugins/gln-auth/gln-auth.json` — account/session store.
3. Grant the plugin the permissions it requests (`fs.read.data`,
   `fs.write.data`, `http.outbound`) if your Pumpkin build prompts for them.

Note: the `plugins/gln-auth/` folder in this repository contains only an
example `config.toml` for reference — the live config lives in the server's
own `plugins/` directory.

## Configuration

All keys live in `plugins/gln-auth/config.toml` (TOML). The repo copy at
`plugins/gln-auth/config.toml` documents the same defaults.

| Key                    | Default | Meaning |
|------------------------|---------|---------|
| `timeout_secs`         | 120     | Seconds an unauthenticated player may stay before being kicked. |
| `session_minutes`      | 120     | Minutes a login is remembered; relog from the same IP resumes silently. |
| `max_login_tries`      | 5       | Failed `/login` attempts from one IP before the player is kicked. |
| `pw_min_len`           | 8       | Minimum password length. |
| `pw_max_len`           | 64      | Maximum password length. |
| `name_regex`           | `^[a-zA-Z0-9_]{3,16}$` | Regex new names must match. |
| `premium_check_enabled`| true    | Ask Mojang whether joining names are premium; auto-login if so. |
| `premium_cache_minutes`| 45      | TTL for cached premium/cracked verdicts. |

Messages are compiled into the plugin and are **not** configurable in v1
(there is no `[messages]` section).

## Commands

| Command | Args | Who can run | Effect |
|---------|------|-------------|--------|
| `/register` | `<password> <confirm>` | everyone | Create an account and log in. |
| `/login` | `<password>` | everyone | Log in (resets the failure counter on success). |
| `/logout` | — | everyone | Log out; the next join asks for `/login` again. |
| `/changepassword` | `<old> <new>` | everyone (must be logged in) | Change the account password. |
| `/unregister` | `<password>` | everyone (must be logged in) | Delete the account. |
| `/setpremium` | `<name> <on\|off>` | ops (level 3+) | Force the premium flag of an account. |
| `/forcelogin` | `<name>` | ops (level 3+) | Mark an account as logged in now. |

Short aliases: `/l` and `/log` work like `/login`.

## How auth works

On every join the plugin runs this flow:

1. **Stored premium flag** — an account with a stored `premium_id` is
   auto-authenticated immediately.
2. **Live premium check** — otherwise (if `premium_check_enabled`) the name
   is checked against Mojang's session servers (cached for
   `premium_cache_minutes`). A premium verdict is stored and auto-logins.
   Up to 3 HTTP attempts total are made; on total failure the check is
   skipped (fail-closed: the player is treated as cracked).
3. **Session resume** — a stored session that is unexpired AND from the same
   IP silently resumes (no prompt).
4. **Prompt** — otherwise the player is prompted to `/register` (no account)
   or `/login` (account exists, or account status unknown due to a store
   error — fail-closed) and is **frozen**: no movement, no chat, no commands
   except the auth commands above.
5. **Timeout** — a still-unauthenticated player is kicked with "Login
   timeout." after `timeout_secs` (default 120s) on their next action.

Additional rules:

- **Single session**: while a name is authenticated, a second join with the
  same name is denied at pre-login with "Already logged in from another
  session."
- **Rate limit**: after `max_login_tries` wrong passwords from one IP the
  player is kicked with "Too many failed login attempts." The counter resets
  on a successful login and when the player leaves.
- Passwords are hashed with Argon2; sessions are pinned to the last login IP.

## Acceptance checklist

Manual, against a live Pumpkin server (offline mode):

- [ ] 1. **Cracked register**: join with a new name → prompted to register →
  `/register <pw> <pw>` → authenticated, unfrozen.
- [ ] 2. **Cracked login + relog**: leave, rejoin → prompted → `/login <pw>`
  → authenticated. Rejoin within 120 min from the same IP → session resumed
  silently.
- [ ] 3. **Wrong password**: `/login <wrong>` 5× → kicked (rate limit
  message).
- [ ] 4. **Timeout**: join, do nothing → frozen; after 120s + any movement
  attempt → kicked with the timeout message.
- [ ] 5. **Premium auto-login**: join with a premium-verified name (e.g.
  one marked via `/setpremium <name> on`, or a real Mojang-owned name on a
  networked host) → auto logged in with the premium message.
- [ ] 6. **Single session**: authenticate in client A, join with the same
  name in client B → B denied with the duplicate-session message.

## Limitations

- The JSON flatfile store (`gln-auth.json`) is not safe for concurrent
  servers sharing a plugin directory — single server per store only.
- No email or 2FA in v1.
- The Mojang premium check requires the `http.outbound` plugin permission
  and network access; without it every player is treated as cracked
  (fail-closed, never fails-open).
