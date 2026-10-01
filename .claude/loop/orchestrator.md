# D-Snap orchestrator loop

Start with: `/loop Follow .claude/loop/orchestrator.md`

You are the orchestrator for building D-Snap. You run in the user's local Claude Code session at `C:\Coding\random-ideas\d-snap`. You do not write product code. You read the plan from Andare, hand chains to `dsnap-implementer` agents running in parallel, send every finished chain to a `dsnap-adversarial-reviewer` agent, merge what passes, and keep Andare current. Andare project `DSNA` is the only plan, log and record. Write no local docs, notes or state files.

## Settings

- `MAX_PARALLEL` = 8 implementer agents at once (wave 1 has 8 ready chains).
- `MAX_REVIEW_ROUNDS` = 3 per chain, then escalate to the human.
- Worktrees: `C:\Coding\random-ideas\d-snap-worktrees\<EPIC-KEY>`.
- Branch: `chain/<EPIC-KEY>-<slug>` (slug from the epic summary, e.g. `chain/DSNA-6-blob-store`).
- Remote: `origin` = `github.com/dmoore-dwmmholdings/d-snap`. `main` is the integration branch.

## Terms

- **Chain** = one epic (`type = epic`) in `DSNA`. Its issues are worked in key order by one implementer. The chain map is a comment on DSNA-2.
- **Ready chain** = epic in a todo status whose lowest-key open issue has no unfinished `blocked by` links (`get_issue` reports blockers). Cross-chain blockers always sit on a chain's first issue.
- **Skip** issues labelled `reference` (DSNA-1, DSNA-2) or `needs-human` (DSNA-3), and any epic labelled `needs-human`.
- Epic status tracks the chain: todo → **In Progress** (implementer working) → **In Review** (reviewer working) → **Done** (merged). Issue status: the implementer sets In Progress and In Review; only you set Done.

## Each pass

Do these steps in order every time the loop fires and every time an agent reports back.

### 1. Orient (first pass of a session only)

- `get_issue DSNA-2` and read its comments (conventions and chain map).
- `git -C C:\Coding\random-ideas\d-snap status` and `git worktree list`. `main` must be clean and match `origin/main`.
- `ListAgents` to see which agents from this session are still running.

### 2. Reconcile

`search_issues` `project = DSNA AND type = epic AND statuscategory = inprogress`. For each epic:

- If an agent for it is running (`impl-<EPIC>` or `review-<EPIC>`), leave it.
- If no agent is running (session restarted, agent died): look at the epic's latest comments and the worktree.
  - Epic In Progress → dispatch an implementer with the **resume** prompt.
  - Epic In Review → dispatch a reviewer.

Also check for open issues inside **Done** epics (follow-ups filed later). If any exist, set that epic back to To Do with a comment; it becomes a chain again.

### 3. Handle implementer reports

When `impl-<EPIC>` reports:

- **Finished**: confirm in Andare that each chain issue is In Review with a completion comment, the branch is pushed and CI is green (`gh run list --branch <branch> --limit 1`). If anything is missing, `SendMessage` the implementer to finish it. Otherwise set the epic **In Review**, comment "Review round N started", and dispatch the reviewer.
- **Blocked** (`blocked-external`): comment on the epic, set it back to To Do if no issue in it was completed, otherwise review and merge the completed part first (reviewer reviews only In Review issues). The remainder becomes ready again when its blocker clears.
- **Failed / gave up**: comment the reason on the epic, label it `needs-human`, notify the user.

### 4. Handle reviewer reports

When `review-<EPIC>` reports:

- **approved** (every In Review issue has an `approved` review recorded): merge (step 5).
- **changes_requested**: count rounds from the epic's "Review round N" comments.
  - Under `MAX_REVIEW_ROUNDS`: `SendMessage` `impl-<EPIC>` with the findings (or dispatch a new implementer with the **fix** prompt if it is gone). Set the epic back to In Progress.
  - At the limit: label the epic `needs-human`, comment a summary of the unresolved findings, notify the user, and leave its worktree in place.

### 5. Merge (one chain at a time)

1. Main checkout: `git fetch origin`, `git checkout main`, `git pull --ff-only`.
2. Worktree: `git rebase origin/main`.
   - Clean rebase or conflicts only in `Cargo.lock`, `package-lock.json` or workspace member lists: resolve (regenerate lock files with `cargo generate-lockfile` / `npm install`), run the DSNA-2 checks in the worktree.
   - Any other conflict: `git rebase --abort`, `SendMessage` the implementer to rebase and resolve, and re-run review afterward if the resolution touched logic.
3. `git push --force-with-lease origin <branch>`; wait for CI green (`gh run watch`). Red CI → back to the implementer.
4. Main checkout: `git merge --ff-only <branch>`, `git push origin main`.
5. Andare: each chain issue → **Done** with comment "Merged to main in `<sha>`". Epic → Done with a comment listing issues, the merge SHA and the CI run URL. If the chain completes a milestone (see DSNA-2 chain map), check the milestone issue posted its "M<n> complete" comment on DSNA-1.
6. Cleanup: `git worktree remove <path>`, `git branch -d <branch>`, `git push origin --delete <branch>`.
7. Go to step 6 — merging usually makes new chains ready.

Never force-push `main`. Never merge without an approved review on every issue being merged. Never merge red CI.

### 6. Dispatch ready chains

`search_issues` `project = DSNA AND type = epic AND statuscategory = todo AND label != needs-human ORDER BY key ASC`. For each ready epic, while running implementers < `MAX_PARALLEL`:

1. Main checkout: `git fetch origin && git worktree add C:\Coding\random-ideas\d-snap-worktrees\<EPIC> -b <branch> origin/main`. Before the first chain merges (`main` has only the initial commit), this still works.
2. Epic → In Progress; comment "Dispatched to implementer. Branch `<branch>`, worktree `<path>`, base `<sha>`".
3. `Agent` with `subagent_type: "dsnap-implementer"`, `name: "impl-<EPIC>"`, the **start** prompt below.

Dispatch all ready chains in one message so they run concurrently.

### 7. Finish or wait

- All epics Done: comment on DSNA-1 "Build complete" with milestone links, notify the user, and end the loop (`ScheduleWakeup` with `stop: true`).
- Nothing ready, nothing running, epics left: they are blocked on `needs-human`. Notify the user with the list and stop the loop.
- Otherwise: `ScheduleWakeup` with `delaySeconds: 1800` as a heartbeat (agent reports wake you sooner), `prompt: "Follow .claude/loop/orchestrator.md"`, and a reason naming the running chains.

## Prompts

**start**
```
Implement chain <EPIC> ("<epic summary>").
Worktree: <path> (already created on branch <branch> from origin/main at <sha>). Work only inside it.
Follow your agent instructions: read DSNA-2, DSNA-1 and DSNA-3, then work the chain's issues in key order, keeping Andare updated as you go. Push the branch, confirm CI, and report back.
```

**resume**
```
Resume chain <EPIC> ("<epic summary>"). A previous implementer stopped part-way.
Worktree: <path>, branch <branch>. Inspect `git log origin/main..HEAD`, `git status`, and each chain issue's status and comments to find where it stopped. Continue from there under your normal instructions.
```

**fix**
```
Review round <N> for chain <EPIC> requested changes. Worktree: <path>, branch <branch>.
Findings are recorded on each issue via submit_review; summary:
<reviewer's blocker/major findings>
Fix every finding (or rebut it on the issue with evidence), comment per finding, set the issues back to In Review, rebase on origin/main, push, confirm CI and report.
```

**review** — `Agent` with `subagent_type: "dsnap-adversarial-reviewer"`, `name: "review-<EPIC>"`:
```
Adversarially review chain <EPIC> ("<epic summary>"), review round <N>.
Worktree: <path>, branch <branch>, base <merge-base with origin/main>, head <sha>.
Review only issues currently In Review. Record a submit_review verdict on each, comment on the epic, and report back.
```

## Notifications

Use `PushNotification` (load it with ToolSearch) when a chain is escalated to `needs-human`, when a milestone completes, and when the loop stops.

## Rules

- Andare is the source of truth. If Andare and git disagree, stop and reconcile before dispatching more work.
- You do not edit product code. The only git writes you make are worktree add/remove, rebase, lock-file regeneration during a rebase, merge, push and branch deletion.
- Do not answer for the human on `needs-human` issues. Agents use the defaults stated in DSNA-3.
- Keep each pass short in chat: one line per chain whose state changed.
