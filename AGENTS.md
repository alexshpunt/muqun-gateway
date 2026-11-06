# Contributing

Read `CONTEXT.md` for the architecture and existing development conventions.
Work on a branch. Complete local development and validation before opening a
pull request. Run `cargo fmt --all -- --check`, `cargo clippy --all-targets
-- -D warnings`, and `cargo test --locked`; report the actual results.
Do not open a ready PR with failed or missing checks.

Agent startup and prompt-delivery changes require real paired App evidence, not
just mocked protocol replies. Verify readiness before submission, an actual response,
follow-up delivery, and preservation of the original terminal focus. Use an isolated
Herdr session for agent-driven experiments. A user's normal interaction with an
existing session can establish App delivery and rendering; record what it proves
and do not request a duplicate scripted check for the same behavior. Verify remaining
gaps independently without injecting test input into working sessions. On the
development Mac, run `proxy_on` in a new terminal before starting AI agents. Ask before
answering trust/approval prompts; never automatically retry an ambiguous submission.

## Releases

- Preview publication also requires an explicit user release request. The manually
  dispatched `preview.yml` workflow stamps the next patch version as
  `X.Y.Z-preview.<run number>` in CI, checks and builds the complete matrix, then
  publishes a GitHub prerelease with `make_latest=false`. Its tag preserves the
  reviewed source commit; notes record the exact stamped version and source SHA.
  Keep human-written `release-notes/unreleased.md` current before dispatch. Never
  move an existing preview tag or silently downgrade an installed preview.

- Publish releases from reviewed, tested commits using `vX.Y.Z` tags. Creating
  or pushing a tag requires a user request to release; a feature request does
  not authorize publishing a version.
- Every GitHub Release must have human-written release notes covering changes,
  fixes, supported Herdr versions, App compatibility, and any required restart
  or migration steps. Commit `release-notes/vX.Y.Z.md` before tagging. Binary
  attachments and generated commit lists do not replace release notes.
- `.github/workflows/release.yml` creates the release once and attaches the
  platform binaries to that same release. Verify that its notes and every
  expected binary are present after publishing.
- Add new App-facing features behind explicit capability declarations. Older
  Apps must continue to use existing endpoints, and newer Apps must be able to
  explain when a Gateway or Herdr upgrade is required for an optional feature.
