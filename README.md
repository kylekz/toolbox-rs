# toolbox-rs

Shared Rust crates for a family of desktop apps built on [gpui](https://github.com/zed-industries/zed)
and [gpui-kit](https://gpui-kit.com). Each crate started life copied from app to app; this repository
holds the one copy they share.

| Crate | What it is | gpui |
|---|---|---|
| [`gpui_automation`](crates/gpui_automation) | JSONL automation inside the app: input, windows, screenshots, named targets, background windows. No-op stand-ins without its `enabled` feature. | yes |
| [`gpui_automation_protocol`](crates/gpui_automation_protocol) | The wire format, the JSON-pointer matcher and the builtin commands' arguments. | no |
| [`gpui_automation_runner`](crates/gpui_automation_runner) | The end-to-end scenario runner as a library; each app wraps it in a small `e2e` bin with its own fixtures. | no |
| [`gpui_appearance`](crates/gpui_appearance) | Light/dark followed live with app and automation overrides, and window materials: Mica/Acrylic on Windows 11, real vibrancy on macOS. [docs/appearance.md](docs/appearance.md) | yes |
| [`automation_harness`](crates/automation_harness) | Dev-only: the app and runner that prove the automation crates and `gpui_appearance` end to end. | yes |
| [`toolbox_process`](crates/toolbox_process) | Running other programs: a process tree of their own (a Unix group, a Windows job), timeouts, cancellation, graceful stops, streaming with stdin, and no console window on Windows. [docs/process.md](docs/process.md) | no |
| [`toolbox_stills`](crates/toolbox_stills) | Still images through the OS decoders (ImageIO, WIC): sniffing, upright colour-managed decodes from files or bytes, HEIC and AVIF, a decode pool and a thumbnail cache. [docs/stills.md](docs/stills.md) | no |
| [`toolbox_store`](crates/toolbox_store) | An app's SQLite database on rusqlite: one writer and pooled readers, migrations that carry on from sqlez's `migrations` table, the foreign-database guard, checked backups and a key-value table. [docs/store.md](docs/store.md) | no |

How automation works, the protocol and the scenario format: [docs/automation.md](docs/automation.md).
Running other programs, and how each app adopts it: [docs/process.md](docs/process.md).
Still images, and how slide and fc adopt them: [docs/stills.md](docs/stills.md).
Light/dark and materials, and how each app adopts them: [docs/appearance.md](docs/appearance.md).
The database crate, and how slide and fc move onto it from sqlez: [docs/store.md](docs/store.md).
What comes next and why: [docs/PLAN.md](docs/PLAN.md).

## Using it

Apps depend on a tag, never on a branch:

```toml
[workspace.dependencies]
gpui_appearance = { git = "https://github.com/kylekz/toolbox-rs", tag = "v0.4.0" }
gpui_automation = { git = "https://github.com/kylekz/toolbox-rs", tag = "v0.4.0" }
gpui_automation_runner = { git = "https://github.com/kylekz/toolbox-rs", tag = "v0.4.0" }
toolbox_process = { git = "https://github.com/kylekz/toolbox-rs", tag = "v0.4.0" }
toolbox_stills = { git = "https://github.com/kylekz/toolbox-rs", tag = "v0.4.0" }
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
gpui_appearance = { path = "../toolbox-rs/crates/gpui_appearance" }
toolbox_process = { path = "../toolbox-rs/crates/toolbox_process" }
toolbox_stills = { path = "../toolbox-rs/crates/toolbox_stills" }
toolbox_store = { path = "../toolbox-rs/crates/toolbox_store" }
```

## Checking

```bash
script/check          # fmt, clippy (both gpui_automation shapes, gpui_appearance with serde), the gpui-free crates, tests, harness e2e
script/check --fast   # the same without opening windows
```

## Licence

MIT ([LICENSE](LICENSE)). No GPL code lives here: crates derived from Zed's GPL crates stay in the
apps that use them. `toolbox_store` replaces the apps' copies of Zed's `sqlez` with rusqlite (MIT),
reading the databases sqlez made by their format alone.
