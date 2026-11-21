---
name: release-check
description: Review readiness for a Vyrtel release candidate or stable tag using existing CI, compatibility checks, performance evidence and release documentation.
---

# Release check

1. Read `docs/releasing.md` and identify the exact candidate commit and version.
2. Confirm milestone scope, blockers, relevant approvals and compatibility changes.
3. Check required CI, binary, image, restore and end-to-end gates against that commit.
4. Check pre-tag performance profiling and where its results will be published.
5. Call out skipped, optional or unverified gates explicitly; never infer a pass.
6. Provide a clear **go / no-go** with remaining actions and evidence links.

Do not create a tag, publish a release, weaken a gate or declare readiness without explicit user authorisation and verified evidence.
Keep release evidence in the documented release/website process; do not introduce a separate evidence branch.
