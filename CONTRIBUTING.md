# Contributing Guidelines

Thank you for your interest in contributing to our project. Whether it's a bug report, new feature, correction, or additional
documentation, we greatly value feedback and contributions from our community.

Please read through this document before submitting any issues or pull requests to ensure we have all the necessary
information to effectively respond to your bug report or contribution.


## Reporting Bugs/Feature Requests

We welcome you to use the GitHub issue tracker to report bugs or suggest features.

When filing an issue, please check existing open, or recently closed, issues to make sure somebody else hasn't already
reported the issue. Please try to include as much information as you can. Details like these are incredibly useful:

* A reproducible test case or series of steps
* The version of our code being used
* Any modifications you've made relevant to the bug
* Anything unusual about your environment or deployment


## Contributing via Pull Requests
Contributions via pull requests are much appreciated. Before sending us a pull request, please ensure that:

1. You are working against the latest source on the *main* branch.
2. You check existing open, and recently merged, pull requests to make sure someone else hasn't addressed the problem already.
3. You open an issue to discuss any significant work - we would hate for your time to be wasted.

To send us a pull request, please:

1. Fork the repository.
2. Modify the source; please focus on the specific change you are contributing. If you also reformat all the code, it will be hard for us to focus on your change.
3. Ensure local tests pass.
4. Commit to your fork using clear commit messages.
5. Send us a pull request, answering any default questions in the pull request interface.
6. Pay attention to any automated CI failures reported in the pull request, and stay involved in the conversation.

GitHub provides additional document on [forking a repository](https://help.github.com/articles/fork-a-repo/) and
[creating a pull request](https://help.github.com/articles/creating-a-pull-request/).


## Running Tests
CI uses [cargo-nextest](https://nexte.st) for test execution. Install it once with `cargo install cargo-nextest --locked`, then run the full suite the same way CI does:

```bash
cargo nextest run --release --workspace   # unit and integration tests
cargo test --release --doc --workspace    # doctests (nextest does not run these)
```


## Releasing

The crates in this workspace are versioned independently. The wrapper crates mirror the version of the crate they wrap, so that a downstream crate can depend on the wrapper using the same version requirement it would have used for the real crate: `shuttle-tokio` is on 1.x because `tokio` is, `shuttle-parking_lot` tracks `parking_lot` 0.12.x, and so on. The internal `-impl`/`-inner` crates are on their own 0.1.x lines. A release is therefore never "bump everything to X" — it is "these crates changed, publish these crates".

`scripts/publish.py` handles the bookkeeping. It compares every crate's local version against crates.io and publishes exactly the ones that aren't there yet, in dependency order. It publishes nothing when there is nothing to publish, so it is safe to re-run.

```bash
scripts/publish.py             # show what would be published, change nothing
scripts/publish.py --dry-run   # package and verify it, without uploading
scripts/publish.py --markdown  # preview the summary reviewers approve against
scripts/publish.py --all       # ignore what's already published; for checking manifests
```

A release is driven by the version bump itself. There is no separate release command to run:

1. Open a PR that bumps the versions of the crates you want to release and adds a `CHANGELOG.md` section for them. When a crate's version requirement on a sibling still matches the sibling's new version, the dependent doesn't need republishing — it picks the new version up on its own. Say so in the changelog entry, as the earlier `tokio wrappers` entries do.
2. Merge it once CI is green. The `Publish dry run` job packages every crate and checks it against the rules crates.io enforces, so manifest problems surface here rather than halfway through a release.
3. Merging triggers the **Publish** workflow. It works out which crates are now ahead of crates.io, builds each one from its packaged tarball to prove it can be published, and writes the list to the run summary.
4. The workflow then waits for approval on the `crates-io` environment, and GitHub notifies its reviewers. Approve the deployment and exactly the crates in that list are published, in dependency order.

Nothing is requested when no version was bumped, so ordinary commits to `main` don't ask anyone to approve anything.

A published version can never be replaced or removed. That approval gate is the only thing standing in front of an irreversible action, so it needs required reviewers configured on the `crates-io` environment under **Settings → Environments** — without them GitHub approves automatically and uploads proceed unattended. If a release fails partway through, re-running is safe: the workflow re-reads crates.io and publishes only what is still missing.

Publishing authenticates with [crates.io trusted publishing](https://crates.io/docs/trusted-publishing) rather than a stored API token. Each crate needs a Trusted Publisher configured once on its crates.io page, pointing at this repository, `publish.yml`, and the `crates-io` environment. A crate published for the first time needs that done before it can go out.

## Finding contributions to work on
Looking at the existing issues is a great way to find something to contribute on. As our projects, by default, use the default GitHub issue labels (enhancement/bug/duplicate/help wanted/invalid/question/wontfix), looking at any 'help wanted' issues is a great place to start.


## Code of Conduct
This project has adopted the [Amazon Open Source Code of Conduct](https://aws.github.io/code-of-conduct).
For more information see the [Code of Conduct FAQ](https://aws.github.io/code-of-conduct-faq) or contact
opensource-codeofconduct@amazon.com with any additional questions or comments.


## Security issue notifications
If you discover a potential security issue in this project we ask that you notify AWS/Amazon Security via our [vulnerability reporting page](http://aws.amazon.com/security/vulnerability-reporting/). Please do **not** create a public github issue.


## Licensing

See the [LICENSE](LICENSE) file for our project's licensing. We will ask you to confirm the licensing of your contribution.
