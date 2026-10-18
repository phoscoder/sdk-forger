# What's left

Generate, Maintain and Port work end to end from the command line, share one headless engine with the desktop
app, and can deliver a finished SDK. This lists what is not done yet, in the order I would do it.

## 1. Checks that have not been run (need your credentials or a real remote)

None of the work since the engine refactor has been run against the live Claude API, a real Docker run of the
new paths, or a real git remote. The unit tests and `--dry-run` pass; the rest is unproven.

- [ ] Re-run the Paynow port with the improved scenario guidance. It stopped when the Anthropic account ran out
  of credit.
- [ ] `maintain --push` and the pull request path have never been run against a real remote. That includes the new
  fork-first path (`gh repo fork`) and the GitLab provider (`glab mr create`).
- [ ] `generate --deliver zip|repo --format` and `--approve` on a real run. Delivery is unit-tested, and the
  formatter recipes have never been run in Docker.
- [ ] Parallel resource generation and the cache (`~/.cache/sdkforge`) on a real run.
- [ ] `sdkforge-cli auth` against a real keychain (Secret Service). Only the "no key" path was exercised.
- [ ] The desktop app driving a real run. It launches and stays up, but nothing was clicked through.
- [ ] Port from Rust, PHP and TypeScript sources, and ports of large SDKs.
- [ ] A case where the fidelity check or the traffic parity check catches a real problem end to end.

## 2. Features from the plan that are not built

- [ ] Traffic parity for Generate (it only applies to Port today).
- [ ] AI refinement of weak types from Postman and Bruno examples, with the user confirming them in the Project screen.
- [ ] Publishing to PyPI, npm or crates.io. Delivery stops at a zip or a repo on purpose (open question below).
- [ ] Docker through `bollard` instead of the `docker` CLI.
- [ ] Forgejo behind the `ForgeProvider` trait (GitHub and GitLab exist, both through their CLIs, not `octocrab`).
- [ ] Desktop app polish: a spec preview on the Project screen, per-stage columns in the Run grid (it shows the
  latest stage line and a log per language), delivery and approval options, Maintain's `--bug`/`--push` toggles,
  and Port's `--accept-diff`.
- [ ] Redaction of spec-embedded secrets: URL credentials and token-shaped strings are scrubbed from prompts, and
  Bruno `environments/` folders are skipped, but values inside example bodies are still passed as-is where they
  reach the model.

## 3. Known limits

- The fidelity check matches API paths only; it no longer checks the HTTP method.
- Port's surface check skips symbol names shorter than 5 characters.
- Port's traffic parity compares requests and success or error, not return values.
- TypeScript sources can be read, but the traffic parity driver runs as plain `node`, so parity usually skips for them.
- Postman and Bruno types are inferred from one example, so every field seen is marked required.
- Rust output uses `ureq` and PHP output uses streams instead of curl, to keep sandbox dependencies small.
- The resource cache stores the model's first output for a resource, not the version that survived the fix loop.
- With `--approve`, a run that fails verification is still copied to the output directory so you can review it.
- The formatter pass has no PHP recipe.
- The app runs one session at a time and has no way to cancel the engine thread.

## 4. Housekeeping

- [ ] **Revoke the API key that was pasted into the first session and create a new one.** I did not do this; it is
  yours to do, and `sdkforge-cli auth` can store the new key in the keychain.
- [ ] The theme crates are not pinned as planned: `gpui-kit =0.6.0` and `gpui-omarchy 0.1.0` no longer compile
  (the `gpui-pre` dependency they use changed underneath them), so the app uses `gpui-kit 0.7.3` and
  `gpui-omarchy 0.2.1`. They are pinned exactly in `crates/app/Cargo.toml`. There is no `llms.txt` in
  `gpui-omarchy 0.2.1`; its `examples/gallery` and `docs/design.md` were the reference.
- [ ] CI skips `sdkforge-app` because building GPUI needs system libraries the stock runner lacks. Add a job that
  installs them if you want the app checked in CI.

## Open questions

- Support local models (Ollama) as well as the Claude API?
- Publish to package registries, or stop at a repo?
- Open source the engine, the app, or both?
