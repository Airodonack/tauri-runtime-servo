# .github/workflows: continuous integration

GitHub Actions runs these on every push to `main` and on every pull request. They are the same checks a contributor runs before sending a change (the [root README](../../README.md#reporting-problems-and-contributing) lists them), on a clean Ubuntu runner with nothing but the packages the build needs, so a change that only builds on one machine is caught. The runtime builds against sibling checkouts of the Servo fork and its Stylo branch, so the job clones all three side by side.

| File | What it holds |
|---|---|
| `check.yml` | Formatting, clippy with warnings as errors (every feature), rustdoc with warnings as errors, the crate's tests under Xvfb, and the helloworld example's build in production mode, development mode and with WebDriver, on Ubuntu 24.04, all in the release profile so Servo is compiled once. |
