<!--
The title is the commit. This repository squash-merges, so the pull request
title becomes the subject on `main` — and `semantic-pr.yml` checks that title
rather than the individual commits. Conventional Commits, one of:

    feat  fix  docs  refactor  chore  ci  build  test  perf  style  revert

That subject is also the changelog line, generated straight from it. Write one
that reads as a release note, not as a note to yourself.

Everything below renders; these comments do not. Delete a heading that has
nothing under it rather than leaving it empty.
-->

## What this changes

<!--
What was wrong, or missing, and what the change does about it.

The diff already says what. The part that cannot be recovered later is why, and
which of the alternatives were rejected — that is what this section is for.
-->

## How it was verified

<!--
What you ran, and what it showed.

`just check` is the floor rather than the answer: it proves nothing broke, not
that the thing you meant to fix is fixed. For a bug, the convincing evidence is
a test that fails before the change and passes after it — say so if there is
one. For behaviour that cannot be unit-tested, say what you exercised by hand
and on which platform.
-->

- [ ] `just check` passes

## Left out on purpose

<!--
Scope you decided against, and why — including anything you found while working
on this and chose not to fix here.

Delete this heading if there is nothing.
-->

<!--
One last thing, for anything touching `src/engine`: AGENTS.md lists rules under
**Non-negotiable design rules** that look like ordinary code and are not. If
this change moves work ahead of capture, makes capture async, gives targets a
shared queue, or turns a Telegram failure into a plain retry, say why here — it
will be the first question asked.
-->
