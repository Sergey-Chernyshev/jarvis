<p align="center"><a href="CONTRIBUTING.md">Русский</a> · <b>English</b></p>

# Contributing to Jarvis

Thanks for your interest! Jarvis is a macOS menu-bar app that watches your Claude Code sessions (Rust + Tauri). All contributions are welcome: bug reports, ideas, docs, code.

> By participating you agree to abide by the [Code of Conduct](CODE_OF_CONDUCT.md).

## Where to start

- **Found a bug?** Open an [issue](https://github.com/Sergey-Chernyshev/jarvis/issues/new/choose) using the "Bug" template.
- **Have an idea?** Open an issue using the "Feature request" template — let's discuss before you write code.
- **Want to pick up a task?** Browse [issues](https://github.com/Sergey-Chernyshev/jarvis/issues), especially those labelled `good first issue` and `help wanted`. Comment on the issue to claim it.

For large changes, **open an issue first** to agree on the approach — so you don't spend time on something that won't be merged.

## Prerequisites

- **macOS 11+** (the project is macOS-only — a Tauri menu-bar app).
- **Rust** (stable) — install via [rustup](https://rustup.rs/).
- **Node.js 20+** and npm.
- **CMake** — required to build `whisper.cpp` (the `whisper-native` feature): `brew install cmake`.

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

Under the hood `npm start` builds the binary with the `wakeword-ort,whisper-native,stt-vad` features and ad-hoc-signs it (needed for microphone access on macOS). See `package.json` for the full set of commands (`setup`, `status`, `bundle`, …).

> **Model weights:** the default STT / wake-word weights ship under non-commercial licenses (CC BY-NC-SA). The project code is MIT. See [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Code style

- **Rust:** run `cargo fmt` and `cargo clippy` before committing. CI checks both.
- **Comments and UI strings** are in Russian (the project is Russian-first), matching the surrounding code.
- Follow existing patterns: look at neighbouring files and write in the same style.

## Commits & Pull Requests

- **Commit messages** follow [Conventional Commits](https://www.conventionalcommits.org/): `feat(stt): …`, `fix(convo): …`, `docs: …`, `chore: …`. Text is in Russian, matching the project history.
- Branch off `master`: `feat/<short>`, `fix/<short>`.
- Direct pushes to `master` are disabled — changes land **only via Pull Request**.
- In your PR:
  - fill in the template (what and why);
  - make sure **CI is green** (`cargo fmt --check`, `clippy`, `cargo test`);
  - keep the PR focused — one logical change per PR;
  - bilingual docs: if you edit the Russian version (`README.md`, `CONTRIBUTING.md`), mirror it into `*.en.md` in the same PR.

## Security

Do not open public issues for vulnerabilities. See [SECURITY.md](SECURITY.md) for how to report privately.

## License

By contributing, you agree that your contribution will be licensed under the [MIT](LICENSE) license.
