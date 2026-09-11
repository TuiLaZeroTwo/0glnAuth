# 0glnAuth Design Spec (2026-09-11)

## 1. Goal
Fully functional Auth system as Pumpkin Rust WASM plugin: auto-login for premium (Mojang-owned) users, register/login for cracked users. No email in v1. Auth-core MVP.

## 2. Context
- Workspace is greenfield (only `examples/` untracked).
- References: `examples/JPremium` (premium detection, sessions), `examples/AuthMeReReloaded` (register/login/limbo/freeze).
- Pumpkin Rust API: `cargo new --lib`, `.cargo/config.toml` target `wasm32-wasip2`, `crate-type=["cdylib"]`, deps `pumpkin-plugin-api`, `tracing`, LTO release. Output `*.wasm` into `plugins/`.
- `impl Plugin { new, metadata, on_load, on_unload }` + `register_plugin!`. `Context`: `get_server`, `register_command`, `register_event_handler(handler, priority, blocking)`.
- Commands are Brigadier trees, no `plugin.yml`.
- Events: blocking = sequential/mutable/cancellable; non-blocking = concurrent, no cancel. Only `PlayerJoinEvent`/`PlayerLeaveEvent` documented; full list unverified.
- Server must run `online_mode=false` / `authentication.enabled=false` for this plugin to make sense.

## 3. Decisions (user-locked)
- Storage: SQLite.
- Premium detection: Mojang API live check.
- Email / forgot-password: skipped in v1.
- Scope: auth-core MVP (register/login/logout/changepassword + sessions + freeze + timeout kick).

## 4. Architecture
Single crate `gln-auth`:
- `src/lib.rs`: `impl Plugin`, wiring.
- `config.rs`: serde TOML `config.toml`.
- `storage.rs`: SQLite access (`accounts`, `sessions`).
- `hash.rs`: Argon2 verify/hash.
- `session.rs`: expiry + IP match.
- `premium.rs`: Mojang resolver + in-memory cache.
- `commands.rs`: register/login/logout/changepassword/unregister + admin setpremium/forcelogin.
- `handlers.rs`: join/leave, freeze, timeout kick.
- `messages.rs`: message keys.
- Shared state: `Arc<RwLock<...>>` (non-blocking handlers are threaded).
- `on_load`: load config -> open SQLite -> register commands + handlers. Fail closed: corrupt DB denies boot, never open auth.

## 5. Data model
Table `accounts(name PK lower, uuid, premium_id NULL, hash, last_ip, last_seen, created)`:
- `isPremium = premium_id IS NOT NULL` (JPremium parity).
Table `sessions(name PK, ip, expires)`:
- Resume iff `now < expires AND ip == last_ip`, default 120 min (AuthMe `SessionService` parity).

## 6. Flows
1. Join (blocking handler): suppress `join_message`; if `premium_id` set or Mojang live-check hit -> auto-login; else if valid session -> silent login; else freeze + prompt, start timeout countdown.
2. `/register <pw> <confirm>`: validate length/regex/not-equal-name -> argon2 hash -> save -> set session -> unfreeze.
3. `/login <pw>`: argon2 verify -> set session -> unfreeze. Failure counter per IP -> kick after N.
4. `/logout`: clear session, re-freeze.
5. `/changepassword <old> <new>`: verify old, store new.
6. `/unregister <pw>` + admin `/forcelogin`, `/setpremium`.
7. Timeout: kick after 120s unauthed (AuthMe `TimeoutTask` parity).
8. Freeze (needs API verification): block chat/commands (allowlist login/register), movement, interact until authed.

## 7. Config (`config.toml`)
`timeout`, `session_time`, `max_login_tries`, `hash_algo`, `premium_check{enabled, cache_min}`, `name_regex`, `pw_min/max`, messages map.

## 8. Mojang premium (JPremium parity)
- `GET api.mojang.com/users/profiles/minecraft/{name}` then `api.minecraftservices.com/minecraft/profile/lookup/name/{name}`.
- 200 = premium, 204/404 = cracked, 429 = retry (max 3, backoff). 45-min cache.

## 9. Testing
- `cargo test`: hash, session expiry/IP, premium cache, validation.
- Manual: local Pumpkin `online_mode=false`, cracked register/login, premium auto-login, session resume, timeout kick.

## 10. Risks / fallbacks
- HTTP-from-WASM blocked -> manual `/setpremium` flag fallback.
- kick/freeze events missing in Pumpkin API -> teleport-loop + command filtering fallback.
- `rusqlite` on `wasm32-wasip2` fails -> JSON flatfile fallback.
- Phase 1 must verify: real event list (chat/move/command?), `kick()`, player UUID/IP access, rusqlite + HTTP on WASI.

## 11. Phases
1. Spike: inspect `pumpkin-plugin-api` source; probe rusqlite + HTTP on wasm32-wasip2.
2. Scaffold crate + release build.
3. Config + messages.
4. SQLite storage.
5. Argon2 hashing.
6. Sessions.
7. Commands.
8. Join/leave + freeze + timeout.
9. Mojang resolver + cache + auto-login.
10. Hardening (rate limits, single-session, docs).
