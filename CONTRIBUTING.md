# Contributing to Perch

Thanks for helping. Perch is a small project with one maintainer, so short, focused changes are easiest to review.

## Reporting a bug

Open an [issue](https://github.com/idlebench/perch/issues/new/choose) with the bug form. The most useful details:

- Perch version (Perch window → General) and Windows build (`winver`)
- Screen resolution and scaling (Settings → System → Display)
- Which app was playing, if it's about media
- What you expected and what happened instead. A screen recording helps a lot.

## Suggesting a feature

Use the feature form. Say what you were trying to do, not just the feature. Perch stays small on purpose
(under ~80 MB RAM at rest, no frameworks), so some ideas won't fit, and that's fine.

## Working on the code

Requirements: Windows 11, Node.js 20+, Rust (stable), and the Visual Studio C++ build tools.

```sh
npm install
npm run tauri dev                 # run with hot reload
npm run build                     # type-check and build the UI
cargo check --manifest-path src-tauri/Cargo.toml
```

See the project layout in the [README](README.md#project-layout).

Ground rules:

- **Keep it light.** If a change adds memory or CPU at rest, say how much (the Perch window has a memory meter).
- **No new frameworks** in the UI, and new crates only when they clearly pay for themselves.
- **One change per pull request**, with a short description of what and why. Screenshots or a clip for anything
  visual.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org): `feat(shelf): ...`,
  `fix(island): ...`, `docs: ...`.

By contributing you agree that your work is released under the [MIT licence](LICENSE).
