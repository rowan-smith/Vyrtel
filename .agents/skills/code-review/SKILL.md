---
name: code-review
description: Review a Vyrtel pull request or code change for acceptance criteria, architecture, correctness, security, durability and test gaps before merging.
---

# Code review

1. Read the change, linked issue and acceptance criteria. Review the actual diff.
2. Check crate boundaries and behaviour against the nearest `AGENTS.md` and relevant docs.
3. Look for concrete correctness, security, durability, resource and compatibility regressions.
4. Check that tests cover the important happy paths and failure cases; run suitable checks when possible.
5. Report only actionable findings, ordered by severity, with file/line evidence.

Finish with a short verdict: **blockers**, **non-blocking suggestions**, **checks run** and **remaining uncertainty**.
Do not invent defects, assert tests passed without running them or approve a change solely because CI is green.
