# SDK Forge

A Rust tool that uses Claude to build, maintain and port API client SDKs, and proves every result by
building and testing it in a Docker sandbox before you see it.

Nothing ships on the model's word alone: parse deterministically, let the model write code, then verify
by running tests in a sandbox and feeding failures back until they pass (or a retry and token cap is hit).

| Mode | Input | Output |
| --- | --- | --- |
| **Generate** | OpenAPI, Postman collection or Bruno folder, plus free-text instructions | A tested SDK in one or more languages |
| **Maintain** | An existing SDK repo plus an error message or improvement | A verified patch on a new branch, optionally pushed with a PR |
| **Port** | An existing SDK repo in one language | The same SDK rebuilt idiomatically in another, with the original's tests ported first |

Languages: Python, Node, PHP, Rust and Go. All five can be a Generate or Port target, and all five can be a Port source.

## Requirements

- Rust (to build), Docker (every build and test runs in a container), `git`.
- An Anthropic API key: either `ANTHROPIC_API_KEY` in your environment, or run `sdkforge-cli auth` once to keep it
  in the system keychain (the environment variable wins). Keys are never written to disk by SDK Forge and never
  passed into a sandbox.
- `gh` (GitHub CLI) or `glab` (GitLab CLI), only for `maintain --push`. Without write access, Maintain pushes to
  your fork on GitHub.

```
cargo build --release          # binary: target/release/sdkforge-cli
```

## Usage

```
# Generate: one language, or several in parallel into out/<lang>/
sdkforge-cli generate examples/petstore-mini.yaml -l python -o out -i "async-first"
sdkforge-cli generate examples/postman-mini.json -l python,node,php,rust,go -o out
sdkforge-cli generate examples/bruno-pets -l go --dry-run         # parse and plan only, no model calls

# Maintain: patch a repo, review the diff, commit to a new branch
sdkforge-cli maintain ./some-sdk -r "paginate stops early on an empty cursor" --bug
sdkforge-cli maintain https://github.com/acme/sdk -r "add retry on 503" --push   # push + open a PR

# Port: rebuild an SDK in another language
sdkforge-cli port https://github.com/acme/sdk-node --to rust -o port-out
sdkforge-cli port ./sdk-go --to php --accept-diff send_standard_happy_path       # accept a known behaviour difference
```

Delivery and review flags for Generate: `--deliver zip|repo` adds a README, package metadata and CI config where
the generated code has none, then zips the SDK or makes a new git repository with a first commit; `--format` runs the
language formatter in the sandbox first; `--approve` (also on Port) verifies in a scratch directory and asks before
anything is written to the output directory; `--no-cache` regenerates every resource instead of reusing earlier
model output (resources are generated four at a time, and unchanged ones come from `~/.cache/sdkforge`).

Useful flags: `--max-attempts` and `--token-budget` cap the fix loop; `--model` picks the Claude model;
`--no-parity` skips the traffic check in Port; `--from` sets the Port source language when it can't be detected.

## How it works

### Architecture

The model never touches your machine directly. Deterministic crates parse and plan, the model only writes
code, and Docker runs everything it wrote.

```mermaid
flowchart LR
    IN["OpenAPI, Postman,<br>Bruno or SDK repo"] --> ING[ingest]
    ING --> IR[model]
    IR --> GEN[generate]
    GEN --> VER[verify]
    GEN <--> CL(("Claude"))
    VER <--> DK(("Docker"))

    classDef ext fill:#e0e7ff,stroke:#6366f1,color:#312e81
    class CL,DK ext
```

### The verify and fix loop

Every mode ends in the same loop. A failure is fed back to the model; after the attempt cap or token
budget the run is flagged for review instead of shipped.

```mermaid
flowchart LR
    A[Model output] --> B{"Checks and<br>tests pass?"}
    B -- yes --> V[Verified]
    B -- no --> C{Attempts left?}
    C -- yes --> D[Model fixes it]
    D --> A
    C -- no --> F[Flagged for review]

    classDef good fill:#d1fae5,stroke:#059669,color:#064e3b
    classDef bad fill:#fee2e2,stroke:#dc2626,color:#7f1d1d
    class V good
    class F bad
```

### Generate

```mermaid
flowchart LR
    S[Spec] --> P[Parse]
    P --> PL["Plan<br>per language"]
    PL --> C[Core]
    C --> R[Resources]
    R --> V[Verify]
    V --> O[SDK]

    classDef good fill:#d1fae5,stroke:#059669,color:#064e3b
    class O good
```

Test cases are derived mechanically from the model rather than left to the model's imagination, and the
fidelity check rejects any API path in the generated code that is not in the spec.

### Maintain

```mermaid
flowchart TD
    A[Clone and branch] --> B["Write a failing test<br>(bugs only)"]
    B --> C[Model writes a diff]
    C --> D[Verify in sandbox]
    D -- fails --> C
    D -- passes --> E{You approve?}
    E -- changes requested --> C
    E -- yes --> F[Commit on new branch]
    F --> G["Push and open PR<br>(--push only)"]

    classDef good fill:#d1fae5,stroke:#059669,color:#064e3b
    class G good
```

### Port

The original's tests are ported first and define correct behaviour; a recorded-traffic comparison then
checks that the port sends the same HTTP requests as the original.

```mermaid
flowchart LR
    A[Read source API] --> B[Plan]
    B --> C[Port tests first]
    C --> D[Port code]
    D --> E[Verify]
    E --> F{Same HTTP<br>traffic?}
    F -- no --> G[Fix the port]
    G --> E
    F -- yes --> H[Ported SDK]

    classDef good fill:#d1fae5,stroke:#059669,color:#064e3b
    class H good
```

### Traffic parity

Both SDKs are driven through the same scripted scenarios against a recording server on a Docker network
with no internet access. The recorded requests are compared one by one.

```mermaid
sequenceDiagram
    participant D as Driver
    participant S as SDK
    participant R as Recorder
    loop each scenario
        D->>R: script the replies
        D->>S: call the SDK
        S->>R: HTTP request
        R-->>S: scripted reply
    end
    R-->>D: log compared with the original
```

## Safety rules

- Builds and tests run only inside Docker containers, never on your machine.
- API keys and Git tokens never enter a container. Tests run with the network off after dependency install.
- Credential-looking values are redacted from your instructions and from error output before they reach the model.
- Maintain never pushes the default branch, and nothing is committed until you approve the diff.
- The fix loop has an attempt cap and a token budget, so a stuck run cannot spend without limit.
- Generated file paths that try to escape the output folder are dropped.

## Project layout

```
crates/
  ir/        intermediate model
  ingest/    OpenAPI, Postman and Bruno adapters; tree-sitter source analysis
  planner/   per-language file layout and naming
  generate/  prompts, Claude client, file parsing, redaction, fidelity check
  verify/    Docker sandbox, toolchains per language, fix loop, traffic recording
  mock/      recording server and traffic comparison
  repo/      git operations and forge providers
  engine/    headless Generate, Maintain and Port pipelines, delivery, and the Host trait both front ends use
  cli/       sdkforge-cli, a thin terminal Host over the engine
  app/       GPUI desktop app (gpui-kit and gpui-omarchy), driving the engine through channels
examples/    sample specs
```

## Status

Generate, Maintain and Port work end to end from the command line, and the desktop app has its five screens
(Project, Instructions, Run, Diff review, Output). The app was built without a real model run, so treat it as
untested against a live API; see [WHATS_LEFT.md](WHATS_LEFT.md).

```
cargo run -p sdkforge-app      # the desktop app (needs the GPUI system libraries)
```
