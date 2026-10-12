# Concepts: Spaces, Libraries, Keys

Three words are all you need. Everything else is plumbing.

```
Space (tenant)   — the security boundary. A key opens a space.
  └── Library (owner)  — a memory space inside it. Memories in the same
        library resonate with each other; libraries stay apart.
        └── Memory nodes
```

## Library (owner) — where memories live

Pick a library name and stick to it: a project slug like `nylonme-project`,
or `dev-alice` for your personal cross-project memory. Same library = memories
link and resonate together. Different library = fully isolated recall.

A library name is **not** a username and carries **no permissions** — it is
just a folder name. Anyone holding a key to the space can read/write any
library inside it. (Hard per-user isolation belongs to the application layer:
your app holds one space key and assigns one library per end-user.)

## Space (tenant) — what your key opens

Every API key is bound to a space when issued
(`nylon-engine keys add --tenant <space> --scope write`). A request naming
another space is rejected. Admin keys use `tenant=*` (all spaces).

Small team sharing one engine? One space (`default`) + one library per
project/person is the intended setup. Give each customer or each team its own
space only when you need **hard** isolation between them.

## Key — the only credential

There are no accounts or passwords. A key is the credential, and its scope is
one of `read` / `write` / `admin`. Paste your key into the Console's key box
and the chip next to it answers "who am I": which space it opens, which scope
it has, and its member alias — powered by `GET /v1/whoami`.

Manage keys three ways, all backed by the same key table:

- CLI on the server: `nylon-engine keys add / list / revoke` (hot reload, no restart)
- REST: `GET/POST /v1/keys`, `DELETE /v1/keys/{prefix}` (admin scope required)
- Console → **Team** view (issue / revoke / per-member activity)

## Cheat sheet

| You want to… | Do this |
|---|---|
| Separate my projects' memories | One library per project (`owner` = project slug) |
| Share a project's memory with a teammate | Same space + tell them the library name |
| Give a teammate access | Issue them a `write` key on your space |
| Revoke someone | `keys revoke` — hot reload, instant |
| Hard-isolate a customer/team | Give them their own space + keys bound to it |
| Build a multi-user app on top | Your app holds one space key; one library per end-user; your app enforces user auth |
