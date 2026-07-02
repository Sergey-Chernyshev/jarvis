<p align="center"><b>English</b> · <a href="CONTRIBUTING.ru.md">Русский</a></p>

# Contributing to Jarvis

Thanks for taking the time to contribute! Jarvis is a macOS menu-bar mission-control for Claude Code and Codex CLI sessions, built with Rust + Tauri. Every kind of contribution is welcome: bug reports, feature ideas, documentation, code.

> By participating you agree to abide by the [Code of Conduct](CODE_OF_CONDUCT.md).

## Where to start

- **Found a bug?** Open an [issue](https://github.com/Sergey-Chernyshev/jarvis/issues/new/choose) using the "Bug" template.
- **Have an idea?** Open an issue using the "Feature request" template — let's discuss it before you write code.
- **Want to pick up a task?** Browse the [issues](https://github.com/Sergey-Chernyshev/jarvis/issues), especially those labelled `good first issue` and `help wanted`. Comment on the issue to claim it.

For large changes, **open an issue first** and agree on the approach — so you don't spend time on something that can't be merged.

## Prerequisites

- **macOS 11+** (the project is macOS-only — a Tauri menu-bar app).
- **Rust** (stable) — install via [rustup](https://rustup.rs/).
- **Node.js 20+** and npm.
- **CMake** — required to build `whisper.cpp` (the `whisper-native` feature): `brew install cmake`.
- **tmux** (optional) — needed for the reply-into-session and remote-control features: `brew install tmux`.

```bash
git clone https://github.com/Sergey-Chernyshev/jarvis.git
cd jarvis
npm ci
```

## Build & run

```bash
npm start          # build (release, all features), ad-hoc sign, and run the dev profile (~/.jarvis-dev)
npm test           # cargo test
```

Under the hood `npm start` builds the binary with the `wakeword-ort,whisper-native,stt-vad` features and ad-hoc-signs it (required for microphone access on macOS). It runs against a separate dev profile (`~/.jarvis-dev`), so your production install stays untouched. See `package.json` for the full set of commands (`setup`, `teardown`, `status`, `bundle`, `start:prod`).

> **Model weights:** the default TTS / wake-word weights ship under non-commercial licenses (CC BY-NC-SA). The project code is MIT. See [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Code style

- **Clippy and tests are blocking:** CI requires clean `cargo clippy` and green `cargo test`.
- **`cargo fmt` is informational only.** The project uses a compact, hand-formatted style that doesn't match default rustfmt — CI shows the diff but does not block on it. **Please don't mass-reformat files**; keep diffs minimal and match the surrounding code.
- **Comments and UI strings** are currently in Russian — match the surrounding code.
- Follow existing patterns: look at neighbouring files and write in the same style.

## Commits & Pull Requests

- **Commit messages** follow [Conventional Commits](https://www.conventionalcommits.org/): `feat(stt): …`, `fix(convo): …`, `docs: …`, `chore: …`. Write the subject in English or Russian — whichever you're comfortable with (the existing history is mostly Russian).
- Branch off `master`: `feat/<short>`, `fix/<short>`.
- Direct pushes to `master` are disabled — changes land **only via Pull Request** with green CI.
- In your PR:
  - fill in the template (what and why);
  - make sure **CI is green** (`cargo clippy`, `cargo test`);
  - keep the PR focused — one logical change per PR;
  - **bilingual docs:** English is canonical (`README.md`, `CONTRIBUTING.md`). If you edit an English doc, mirror the change into its Russian counterpart (`README.ru.md`, `CONTRIBUTING.ru.md`) in the same PR.

## Security

Do not open public issues for vulnerabilities. See [SECURITY.md](SECURITY.md) for how to report privately.

## License

By contributing, you agree that your contribution will be licensed under the [MIT](LICENSE) license.
