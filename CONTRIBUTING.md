# Contributing to Vyrtel

Thanks for helping. Bug reports, fixes, docs and features are all welcome.

## Before you start

- **Bugs and small fixes**: open a pull request directly, or an issue if you'd like to discuss first.
- **Features and larger changes**: open an issue describing the problem first, so we can agree on the
  approach before you spend time on it. The [roadmap](docs/roadmap.md) lists what's already planned.
- **Security issues**: please don't open a public issue. Report them privately through GitHub's
  *Security → Report a vulnerability* on the repository.

## Contributor License Agreement

Every contributor signs a Contributor License Agreement (CLA) once, before their first pull request
can be merged. You keep the copyright in your work; the CLA gives the project owner a licence to use,
distribute and relicense it (including under different or commercial terms in future), and a patent
licence. Anything already released under the MIT License stays available under it.

| You are contributing…                                   | Sign                                                                                                                             |
|---------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------|
| as an individual, on your own time                      | the [Individual CLA](docs/cla/individual.md)                                                                                     |
| as part of your job, or your employer may own your work | the [Individual CLA](docs/cla/individual.md), **and** your employer signs the [Corporate CLA](docs/cla/corporate.md) listing you |

**Signing the Individual CLA** happens on your first pull request: the CLA bot comments with a link,
and you reply with:

```
I have read the CLA Document and I hereby sign the CLA
```

That's it; it covers all your future contributions. If you push more commits or open new pull
requests, you won't be asked again.

**The Corporate CLA** is signed by someone who can bind your organisation and emailed to
cla@vyrtel.com, along with the list of people it covers. The steps are in the document.

## Making a change

1. Fork the repository and create a branch from `main`.
2. Set up the toolchain and run the app: see [development](docs/development.md).
3. Make your change, with tests. Keep pull requests focused on one thing.
4. Run the checks before pushing:

   ```bash
   cargo fmt --all && cargo clippy --workspace --all-targets && cargo test --workspace
   cd web && npm run lint && npm run typecheck && npm test
   ```

5. Open a pull request describing **what** changed and **why**. Screenshots help for UI changes.

Docs live in [`docs/`](docs) as Markdown and are published to the website; see
[development → Website and docs site](docs/development.md#website-and-docs-site).

## Style

- Match the surrounding code: naming, comment density and idioms.
- User-facing text follows the [brand voice](docs/brand/README.md#voice): direct, technical, calm.
- Keep the binary small and the dependencies few; new dependencies need a reason.
