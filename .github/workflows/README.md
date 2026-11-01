# .github/workflows: continuous integration

GitHub Actions runs these on every push to `main` and on every pull request. They are the same checks a contributor runs before sending a change (the [root README](../../README.md#reporting-problems-and-contributing) lists them), on a clean Ubuntu runner with nothing but the packages the build needs, so a change that only builds on one machine is caught. The runtime builds against a sibling checkout of the Servo fork, so the build job clones the two side by side (Stylo is a git dependency cargo fetches itself).

| File | What it holds |
|---|---|
| `check.yml` | Two jobs on Ubuntu 26.04 (the first Ubuntu with GStreamer 1.26 or newer, which the build needs). `check`: formatting, clippy with warnings as errors (every feature, every example in the workspace), rustdoc with warnings as errors, the crate's tests under Xvfb (the ignored ones too, which need a display), and the helloworld example's build in production mode, development mode and with WebDriver, all in the release profile so Servo is compiled once. `smoke`, after it: the development-mode example, passed on as an artifact, run headless by [`examples/helloworld/smoke-test.sh`](../../examples/helloworld/README.md#the-smoke-test), which fails if a `RESULT` line is missing or the app does not exit cleanly. |
