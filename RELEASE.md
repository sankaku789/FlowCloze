# Release Checklist

## 0.1.0

- [x] Run `cargo fmt --check`.
- [x] Run `cargo clippy --all-targets -- -D warnings`.
- [x] Run `cargo test`.
- [x] Run `cargo build --release`.
- [x] Verify `cargo run -- --version` prints `flowcloze 0.1.0`.
- [x] Review `cargo package --allow-dirty --list`.
- [x] Run `cargo package --allow-dirty`.
- [x] Choose and add a project license before publishing outside this repository.
- [ ] Create a git tag after committing release changes: `git tag v0.1.0`.
- [ ] Push the branch and tag: `git push origin main v0.1.0`.
