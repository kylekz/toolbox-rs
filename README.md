# toolbox-rs

Shared Rust crates for a family of desktop apps built on [gpui](https://github.com/zed-industries/zed)
and [gpui-kit](https://gpui-kit.com). Each crate started life copied from app to app; this repository
holds the one copy they share.

| Crate | What it is | gpui |
|---|---|---|
| [`gpui_automation`](crates/gpui_automation) | JSONL automation inside the app: input, windows, screenshots, named targets, background windows. No-op stand-ins without its `enabled` feature. | yes |
| [`gpui_automation_protocol`](crates/gpui_automation_protocol) | The wire format, the JSON-pointer matcher and the builtin commands' arguments. | no |
| [`gpui_automation_runner`](crates/gpui_automation_runner) | The end-to-end scenario runner as a library; each app wraps it in a small `e2e` bin with its own fixtures. | no |
| [`automation_harness`](crates/automation_harness) | Dev-only: the app and runner that prove the three above end to end. | yes |

How it works, the protocol and the scenario format: [docs/automation.md](docs/automation.md).
What comes next and why: [docs/PLAN.md](docs/PLAN.md).

## Using it

Apps depend on a tag, never on a branch:

```toml
[workspace.dependencies]
gpui_automation = { git = "https://github.com/kylekz/toolbox-rs", tag = "v0.1.0" }
gpui_automation_runner = { git = "https://github.com/kylekz/toolbox-rs", tag = "v0.1.0" }
```

toolbox-rs pins the gpui stack exactly (`gpui-pre =0.3.11`, `gpui-component =0.9.0`). An app on a
toolbox-rs tag uses exactly those versions: Cargo builds one gpui per app. A gpui bump is a new
minor tag here first; each app moves when it is ready.

Working on toolbox-rs and an app together, point the app at a local checkout without committing
it, in the app's `.cargo/config.toml` or its manifest:

```toml
[patch."https://github.com/kylekz/toolbox-rs"]
gpui_automation = { path = "../toolbox-rs/crates/gpui_automation" }
gpui_automation_protocol = { path = "../toolbox-rs/crates/gpui_automation_protocol" }
gpui_automation_runner = { path = "../toolbox-rs/crates/gpui_automation_runner" }
```

## Checking

```bash
script/check          # fmt, clippy (both gpui_automation shapes), the gpui boundary, tests, harness e2e
script/check --fast   # the same without opening windows
```

## Licence

MIT ([LICENSE](LICENSE)). No GPL code lives here: crates derived from Zed's GPL crates, such as
`sqlez`, stay in the apps that use them.
