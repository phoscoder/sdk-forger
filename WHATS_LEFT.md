# What's left

Generate, Maintain and Port work end to end from the command line. This lists what is not done yet,
in the order I would do it.

## 1. Desktop app and a headless engine

- [ ] **Engine crate.** The Generate, Maintain and Port pipelines currently live in the `cli` crate and use
  `println!`, `die()` and stdin prompts. Move them into a headless engine that takes commands and streams
  events over channels, so the CLI and the app share one pipeline.
- [ ] **GPUI app.** `crates/app` only prints a placeholder. Screens still to build: Project, Instructions,
  Run (live grid of languages by stages with streaming logs), Diff review, Output.
- [ ] **Theming.** Use `gpui-kit =0.6.0` and `gpui-omarchy 0.1.0`, pinned. Call `gpui_omarchy::init(cx)` once at
  startup. Read gpui-omarchy's `llms.txt` first; models invent GPUI methods.
- [ ] **Threading.** GPUI thread and a tokio thread joined by channels (UI actions become commands, the engine
  streams events back). The engine currently uses a blocking HTTP client and the `docker` CLI.

## 2. Delivery step

- [ ] README, package metadata and CI config for generated SDKs (planned with `minijinja`).
- [ ] A formatter and linter pass on generated code.
- [ ] Deliver as a zip, a new repo with a first commit, or a published package.
- [ ] Publishing to PyPI, npm or crates.io. (Open question: or stop at a repo?)

## 3. Checks that have not been run

- [ ] Re-run the Paynow port with the improved scenario guidance. It stopped when the Anthropic account ran out
  of credit.
- [ ] `maintain --push` and the pull request path have never been run against a real remote.
- [ ] Port from Rust, PHP and TypeScript sources, and ports of large SDKs.
- [ ] A case where the fidelity check or the traffic parity check catches a real problem end to end.
- [ ] Traffic parity for Generate (it only applies to Port today).

## 4. Features from the plan that are not built

- [ ] AI refinement of weak types from Postman and Bruno examples, with the user confirming them in the Project screen.
- [ ] Cache unchanged resources between runs.
- [ ] Generate resources in parallel inside one language (languages already run in parallel).
- [ ] Approval before writing files for Generate and Port (Maintain already asks).
- [ ] Fork first when the user has no write access to the repo.
- [ ] Tokens in the system keychain with `keyring`. Keys are environment variables for now.
- [ ] Forges other than GitHub (GitLab, Forgejo) behind the `ForgeProvider` trait. GitHub uses the `gh` CLI, not `octocrab`.
- [ ] Docker through `bollard` instead of the `docker` CLI.
- [ ] A symbol index for Maintain (file selection is keyword based today).
- [ ] Redaction of Bruno environments and other spec-embedded secrets beyond key-looking lines.

## 5. Known limits

- The fidelity check matches API paths only; it no longer checks the HTTP method.
- Port's surface check skips symbol names shorter than 5 characters.
- Port's traffic parity compares requests and success or error, not return values.
- TypeScript sources can be read, but the traffic parity driver runs as plain `node`, so parity usually skips for them.
- Postman and Bruno types are inferred from one example, so every field seen is marked required.
- Rust output uses `ureq` and PHP output uses streams instead of curl, to keep sandbox dependencies small.

## 6. Housekeeping

- [ ] Run `cargo fmt` and `cargo clippy` across the workspace, and add CI.
- [ ] Revoke the API key that was pasted into the first session and create a new one.

## Open questions

- Support local models (Ollama) as well as the Claude API?
- Publish to package registries, or stop at a repo?
- Open source the engine, the app, or both?
