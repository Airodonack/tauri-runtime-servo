# .github/workflows

CI, run on every push to `main` and every pull request, on Ubuntu 26.04.

| File | What it holds |
|---|---|
| `check.yml` | `check`: formatting, clippy, rustdoc, the tests under Xvfb, and the helloworld example built in each mode. `smoke`: the example run headless by `smoke-test.sh`, which fails if a `RESULT` line is missing or the app does not exit cleanly. |
