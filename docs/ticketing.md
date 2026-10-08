# Vyrtel issue and Project operating guide

## Source of truth

Use GitHub Issues as the durable work record and [Project 2](https://github.com/users/rowan-smith/projects/2) for scheduling. One deliverable outcome per issue. Native issue milestones represent target delivery; labels make repository search work; Project fields make planning views work. Do not maintain a second independent status in the issue body.

The baseline scope is in [delivery-scope.md](delivery-scope.md), with the import source in [backlog.json](planning/backlog.json). Story points are provisional relative effort, including validation and documentation. They are not hours, days or a release-date commitment. Split 8-point stories before Ready. The 72 stories total 249 provisional points; do not use that sum as a delivery forecast.

## Install

The PR adds three GitHub issue forms and a PR checklist. Forms become selectable after merge to the default branch. Labels must exist before form labels apply.

The importer uses Python 3.10+ and authenticated GitHub CLI `gh`. First inspect its dry run:

```bash
python3 tools/planning/import_backlog.py
```

For repository plus Project writes, use a CLI login with repository write and Project permissions:

```bash
gh auth refresh -s project
python3 tools/planning/import_backlog.py --apply
```

To configure repository labels/milestones and create issues without modifying the Project:

```bash
python3 tools/planning/import_backlog.py --apply --repository-only
```

Import defaults to rowan-smith/Vyrtel and rowan-smith Project 2. It creates missing managed labels and milestones, creates stories by stable body marker, resolves exact issue dependency links, adds issues to the existing Project, and sets missing planning fields on Project items. It does not delete existing labels, milestones, views or issues; it does not reset populated Project status/fields or overwrite manually edited story bodies. If an existing Project field is incompatible or lacks a required option, it stops with a precise remediation message rather than replacing that field. Rerun after correcting it.

Review the manifest before applying: this command creates up to 72 issues and configures their metadata. Keep dry-run output and the resulting mapping in the review record. No assignees or due dates are inferred. Optional Project-only customisations below are manual; the importer does not configure views or built-in workflows.

## Labels

Use exactly one type label and priority label after refinement. Use one primary area label; add another only when it materially helps triage.

| Family | Values | Rule |
|---|---|---|
| Type | type:story, type:bug, type:feature, type:discovery | Feature requests become scoped stories before Ready; change the type label, retaining the issue where practical. |
| Priority | priority:P0, priority:P1, priority:P2, priority:P3 | Maintainer scheduling decision; keep aligned with Project Priority. |
| Severity | severity:critical, severity:high, severity:medium, severity:low | Bugs only; impact independent from priority. |
| Area | api, alerts, dashboards, docs, ingest, metrics, operations, performance, query, release, security, storage, traces, web | Prefix every value with area:. |
| Workflow | needs-triage, blocked, release-blocker | Only apply while condition is true; remove blocked when dependencies resolve. |

Priority definitions: P0 urgent incident/release blocker; P1 core milestone outcome; P2 normal planned improvement; P3 low-priority discovery or optional work. A planned P0 hardening story does not claim an active outage.

Severity definitions: critical means confirmed data loss, security exposure or total outage; high means a core workflow is unavailable without a reasonable workaround; medium means partial failure or a usable workaround; low means minor presentation/usability impact. Reporters suggest severity and priority; maintainers confirm them. Do not put confirmed secrets or private telemetry in public issue evidence.

## Project fields

| Field | Type | Values / source |
|---|---|---|
| Status | Built-in single select | Preferred: Backlog, Ready, In Progress, In Review, Done. Existing Todo or New is accepted as the initial backlog option. |
| Priority | Single select | P0, P1, P2, P3; importer can recognise existing option names beginning with those codes. |
| Area | Single select | Primary area values listed above. |
| Estimate | Number | Fibonacci-style story points; unset for unrefined reports. |
| Target release | Single select | 0.1.2, 0.1.3, 0.2.0, 0.2.1, 0.2.2, Future. |
| Milestone | Native issue field | The milestone assigned to the issue, not a duplicate custom milestone field. |
| Assignees | Native issue field | Set only when someone accepts ownership. |
| Iteration | Optional iteration | Add after capacity and cadence are known; no speculative sprint dates. |

Status and request decision are different concepts. New requests go to Backlog with needs-triage. Under-review decisions stay Backlog until refinement. Planned means a milestone/priority is assigned. In-progress work uses In Progress. Shipped/completed uses Done and a closed issue. Declined uses a closed issue with not-planned reason and an explanation; it is not a shipped feature.

Changing issue-form dropdowns does not automatically populate Project fields or labels. During triage, confirm metadata on both surfaces. Avoid relying on Project-only values because repository issue search cannot see all custom fields.

## Views to configure

1. **Triage**: table, open issues with needs-triage; show type, area, priority, severity and age.
2. **Delivery**: board by Status, exclude Future discovery, show priority/estimate/milestone.
3. **Release planning**: table grouped by Milestone, sorted by Priority; include Estimate and dependency links.
4. **Bugs**: table of type:bug grouped by severity; show priority, version, workaround and reproducibility.
5. **Blocked**: table of blocked issues, with named blockers and owner.
6. **Discovery**: table of type:discovery; show decision outcome and research timebox.
7. **Release gate**: table of release-blocker issues; no release ships while required blockers are open.

Use Project built-in auto-add for open issues in this repository, and its closed-issue status workflow to move completed work to Done. Verify the repository filter before enabling. External reporters may lack Project write access, so do not rely on an issue-form projects setting to enrol every issue. Create a separate Done/archive policy based on reporting needs rather than hiding unfinished work.

## Definition of Ready

- A named user role, capability and benefit are present.
- The problem/current behaviour is understood; do not duplicate a completed capability.
- Acceptance criteria are observable, include relevant error cases, and have a validation approach.
- Scope exclusions and exact dependency issue links are present.
- No hard dependency remains unresolved.
- Priority, area, milestone and a refined estimate are assigned.
- A team member can implement it without inventing a major product contract.

## Definition of Done

- Every acceptance criterion has evidence in the PR or issue.
- Relevant edge cases, error handling and access requirements are verified.
- Existing required checks pass; docs/API contracts/migrations are updated where applicable.
- Changes are reviewed and merged; the issue links to the implementing PR.
- Release-dependent stories are marked shipped only when their delivery condition is satisfied.
- Discovery issues close with a reviewed decision and explicit follow-up/deferral, not feature implementation.

## Dependencies and issue hierarchy

Stable IDs such as REL-01 are planning references. Import resolves them to exact #issue URLs in a managed Dependencies section. Use “Blocked by” only for a hard ordering constraint. Use “Related to” for context. Maintain these links as scope changes. The importer creates Markdown relationships, not native GitHub blocked-by graph edges; maintainers can add native relationships in GitHub where available.

Use milestones and Project Area for workstreams. Do not create giant parent epics that pretend to be deliverable user stories. If adding a tracking epic later, label it explicitly and exclude its roll-up points from velocity calculations.

## Milestone exit gates

- **0.1.2**: all required reliability/release stories complete; restore drill and crash evidence reviewed; release artifacts validate.
- **0.1.3**: existing core UI/auth/integration workflows pass; inaccessible/error/empty states are handled.
- **0.2.0 query + metrics**: core P1 stories in both workstreams pass correctness/interoperability fixtures; performance evidence reviewed; compatibility notes published. P2 deferrals require an explicit decision.
- **0.2.1**: selective deletion is recovery-tested; durable notification retries and suppression rules verified.
- **0.2.2**: agreed dashboard stories have persistence/accessibility/navigation evidence; optional items can be moved with explicit rationale.
- **Future**: research outputs inform decisions; it is not a promised product release.

## Triage and refinement cadence

Review new reports at least weekly and urgent incidents as they arrive. Confirm duplicates, reproducibility, impact and workaround before scheduling. Preserve reporters' original evidence. Close duplicates with a canonical issue link; close declined requests with an outcome/reason. Re-estimate after contracts are clarified. Limit In Progress to actual available capacity and split scope when a ticket gains unrelated acceptance criteria.

The closed ECharts PR is historical context, not an open blocker. No speculative defects are seeded as bug reports; use the bug form for observed failures.

## Sources

- Repository README and source at the baseline commit.
- docs/roadmap.md for existing product intentions and non-goals.
- GitHub issue-form schema and Projects API documentation for supported metadata behaviour.
