# 0gln Auth

A Pumpkin (WASM) authentication plugin for offline-mode Minecraft servers:
premium (paid) players claim their name once with `/premium` (verified
against Mojang's session servers), cracked (non-premium) players get a
classic register/login password flow with session memory, an
unauthenticated freeze, and a login timeout. One active session per player
name, per-IP rate limiting, and Argon2 password hashing are built in.

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

The plugin artifact is `target/wasm32-wasip2/release/zero_gln_auth.wasm`.

## Install

1. Copy `zero_gln_auth.wasm` into the server's `plugins/` directory.
2. Start the server. On first load the plugin creates, inside its data
   folder (`plugins/data/0gln Auth/` on the server):
   - `config.toml` — configuration (defaults; edit and restart to change),
   - `0gln-auth.json` — account/session store.
3. Grant the plugin the permissions it requests (`fs.read.data`,
   `fs.write.data`, `http.outbound`) if your Pumpkin build prompts for them.

Note: the `plugins/0gln-auth/` folder in this repository contains only an
example `config.toml` for reference — the live config lives in the server's
own plugin data folder.

## Configuration

All keys live in the plugin data folder's `config.toml` (TOML), i.e.
`plugins/data/0gln Auth/config.toml` on the server. The repo copy at
`plugins/0gln-auth/config.toml` documents the same defaults.

| Key                    | Default | Meaning |
|------------------------|---------|---------|
| `timeout_secs`         | 120     | Seconds an unauthenticated player may stay before being kicked. |
| `session_minutes`      | 120     | Minutes a login is remembered; relog from the same IP resumes silently. |
| `max_login_tries`      | 5       | Failed `/login` attempts from one IP before the player is kicked. |
| `pw_min_len`           | 8       | Minimum password length. |
| `pw_max_len`           | 64      | Maximum password length. |
| `name_regex`           | `^[a-zA-Z0-9_]{3,16}$` | Regex new names must match. |
| `premium_check_enabled`| true    | Whether `/premium` may ask Mojang to verify names. |
| `premium_cache_minutes`| 45      | TTL for cached premium/cracked verdicts. |
| `messages`             | see file | `[messages]` section: override any player-facing message text. |

### Custom messages

Every player-facing message can be reworded in the `[messages]` section of
`config.toml` (quoted keys, because they contain dots):

```toml
[messages]
"login.wrong" = "Falsches Passwort."
```

Keys you omit keep their built-in default; unknown keys are ignored, so an
old config keeps working across plugin updates. The full default table is
listed in the generated `config.toml`.

## Commands

| Command | Args | Who can run | Effect |
|---------|------|-------------|--------|
| `/register` | `<password> <confirm>` | everyone | Create a cracked account and log in. |
| `/login` | `<password>` | everyone | Log in (resets the failure counter on success). |
| `/premium` | — | everyone | Verify this name as premium with Mojang, create the account, and log in. Rejected if any account already exists for the name. |
| `/logout` | — | everyone | Log out; the next join asks for `/login` again. |
| `/changepassword` | `<old> <new>` | everyone (must be logged in) | Change the account password. |
| `/unregister` | `<password>` | everyone (must be logged in) | Delete the account. |
| `/setpremium` | `<name> <on\|off>` | ops (level 3+) | Force the premium flag of an account. `off` is rejected on an account with no password. |
| `/forcelogin` | `<name>` | ops (level 3+) | Mark an account as logged in now. |

Short aliases: `/l` and `/log` work like `/login`.

Admin recovery note: an account that is stuck (e.g. a premium player whose
Mojang name was released) can be brought back with `/forcelogin <name>`
followed by `/changepassword` while logged in.

## How auth works

On every join the plugin runs this flow:

1. **Stored premium flag + UUID match** — an account with a stored `premium_id`
   auto-authenticates ONLY if the UUID the client presented at login matches
   the Mojang UUID for that name (premium launchers send their real Mojang
   UUID; cracked launchers send an offline-derived one and are frozen out).
   A cracked client joining with a premium-flagged name is denied with
   "join with your premium launcher" and falls to the frozen /login path.
2. **Session resume** — a stored session that is unexpired AND from the same
   IP silently resumes (no prompt).
3. **Prompt (chat-choice)** — otherwise the player is unauthenticated and
   **frozen** (no movement, chat, block interaction, item drops, attacks,
   or commands except the auth commands above):
   - no account → "Are you premium or cracked?" — use `/premium` to verify
     a premium name against Mojang (up to 3 HTTP attempts, 5s connect
     timeout), or `/register` to create a cracked account,
   - account exists (or account status unknown due to a store error —
     fail-closed) → prompted to `/login`.
4. **Timeout** — a still-unauthenticated player is kicked with "Login
   timeout." after `timeout_secs` (default 120s) on their next action.

There is deliberately **no join-time premium auto-resolution**: a premium
name is only ever claimed through the explicit `/premium` command, so an
existing password account can never be silently converted (an `/premium`
claim on an existing name is rejected with "account already exists").

Additional rules:

- **Single session**: while a name has a live session — authenticated OR
  still frozen/unauthenticated — a second join with the same name is denied
  at pre-login with "Already logged in from another session."
- **Rate limit**: after `max_login_tries` wrong passwords from one IP the
  player is kicked with "Too many failed login attempts." The counter resets
  ONLY on a successful login — reconnecting does not reset it.
- Passwords are hashed with Argon2; sessions are pinned to the last login IP.

## Acceptance checklist

Manual, against a live Pumpkin server (offline mode):

- [ ] 1. **Cracked register**: join with a new name → prompted with the
  premium-or-cracked choice → `/register <pw> <pw>` → authenticated,
  unfrozen.
- [ ] 2. **Cracked login + relog**: leave, rejoin → prompted → `/login <pw>`
  → authenticated. Rejoin within 120 min from the same IP → session resumed
  silently.
- [ ] 3. **Wrong password**: `/login <wrong>` 5× → kicked (rate limit
  message). Reconnect immediately → the counter is still spent (kicked
  again after 0 more failures), only a successful login clears it.
- [ ] 4. **Timeout**: join, do nothing → frozen; after 120s + any movement
  attempt → kicked with the timeout message.
- [ ] 5. **Premium claim**: join with a fresh premium name → choice prompt →
  `/premium` → verified → premium welcome, unfrozen. Join again →
  auto-login via the stored premium flag.
- [ ] 6. **/premium on a non-premium name**: rejected with "That name is
  not premium, use /register."
- [ ] 7. **/premium with an existing account**: rejected with "account
  already exists, use /login" — the password account is never converted.
- [ ] 8. **Single session**: authenticate in client A, join with the same
  name in client B → B denied with the duplicate-session message. Also
  while A is still frozen (unauthenticated), B is denied.
- [ ] 9. **Freeze blocks interaction**: while unauthenticated, breaking a
  block, placing a block, right-clicking, dropping an item, and hitting a
  mob are all blocked; after `/login` they all work.
- [ ] 10. **Custom message**: put `"login.wrong" = "test"` in `[messages]`,
  restart, `/login <wrong>` → "test" shown; remove it → default text.

## Limitations

- **WARNING — if 0gln Auth fails to load, the server runs WITHOUT auth.**
  When the plugin's load fails (corrupt `0gln-auth.json` store, malformed
  `config.toml`, denied permissions), Pumpkin disables the plugin, logs an
  error, and keeps the server running. In offline mode that means anyone
  can join as any name. Fix the storage/config and confirm `0gln Auth` shows
  up in `/plugins` before opening the server to players.
- Residual risk (documented): `/premium` requires the claimant to join with
  the Mojang-signed-in launcher (the Login Start UUID must match Mojang's),
  so stock cracked launchers cannot claim or reuse a stranger's premium name.
  A custom-modified client that already knows the victim's Mojang UUID could
  still spoof it (offline mode never cryptographically authenticates it) —
  this defeats stock launchers, not a determined attacker.
- The JSON flatfile store (`0gln-auth.json`) is not safe for concurrent
  servers sharing a plugin directory — single server per store only.
- No email or 2FA in v1.
- The Mojang premium check requires the `http.outbound` plugin permission
  and network access; without it `/premium` always fails closed with "Could
  not verify premium status, try again later."
