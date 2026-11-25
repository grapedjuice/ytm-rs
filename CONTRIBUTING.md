# Contributing to ytm-rs

Thanks for helping out! Bug reports, ideas, fixes, features and translations are all welcome.

## Issues and questions

- **Bugs and ideas:** [open an issue](https://github.com/grapedjuice/ytm-rs/issues/new/choose) with the bug report or feature request form. For a bug, say what you did, what you expected and what happened, plus your OS and the app's version. A screenshot, clip or log helps a lot.
- **Questions:** not sure something is a bug, or want to show your setup? Use [Discussions](https://github.com/grapedjuice/ytm-rs/discussions).
- Search the open issues first: your bug or idea may already be there.

## Before you start on code

- For anything small (a bug, a typo, a missing translation), go ahead and open a pull request.
- For anything large (a new feature, a new dependency, a change to how something works), it's worth opening an issue or commenting on the existing one first. That can save you work on something that might not fit.
- Before you start, check the open pull requests and recent commits on main: someone may already be on it.
- If you'd like to take an issue, say so on it, so two people don't build the same thing. An issue with someone assigned is taken.

## Building and checking

See [Building from source](README.md#building-from-source) for what each platform needs. Before you open a pull request, both of these have to pass, as they do in CI:

```sh
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Then run the app and try what you changed. Mention what you tried in the pull request.

## Writing the change

- **One pull request, one change.** A fix and an unrelated cleanup are two pull requests.
- **Keep it small.** Reuse what's already in the code before adding something new, and say in the pull request why a new dependency is needed.
- **Text people see goes through translations.** Use `tr!` with an id in `locales/en-US/`, rather than an English string in the code. Other languages fall back to English until they're translated.
- **Tests help.** A new parser or calculation is easier to review with a small test next to it that fails if it breaks.
- **Comments say why**, not what the next line does.
- **Commits and comments are about this app.** Say what it does and why, without naming other apps or clients.

## Pull requests

- **Title:** what changes for someone using the app, with the area first, for example "Song lists: a click no longer starts a drag". The same goes for commit messages.
- **Description:** fill in the [pull request template](.github/pull_request_template.md): what was wrong or missing, what changed and where, and what you checked. Link the issue: "Fixes #13" when the pull request fixes all of it, so it closes when the pull request is merged, or "Refs #13" when it's one part.
- Reviews may ask for changes. Push them to the same branch, and reply on the comment when it's done.

## Labels

Pull requests are labelled automatically: a `size:` label from the lines they change, and an `area:` label when the files make it obvious. Maintainers add the rest.

Useful lists: [issues to pick up](https://github.com/grapedjuice/ytm-rs/issues?q=is%3Aissue+is%3Aopen+no%3Aassignee+label%3A%22good+first+issue%22%2C%22help+wanted%22) · [issues not sorted yet](https://github.com/grapedjuice/ytm-rs/issues?q=is%3Aissue+is%3Aopen+no%3Alabel) · [pull requests ready for review](https://github.com/grapedjuice/ytm-rs/pulls?q=is%3Apr+is%3Aopen+draft%3Afalse+review%3Anone) · [small pull requests](https://github.com/grapedjuice/ytm-rs/pulls?q=is%3Apr+is%3Aopen+draft%3Afalse+label%3A%22size%3A+XS%22%2C%22size%3A+S%22)

## Translations

Translations don't need Rust or a build. See [Translating ytm-rs](TRANSLATING.md).

## License

By contributing, you agree that your contribution is licensed under the project's [GPL-3.0](LICENSE) license.
