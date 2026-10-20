# Witness in CI

Witness computes the review plan from git and the session logs committed
with the change, so CI only needs the full history and the `witness`
binary.

## GitHub Actions

```yaml
name: Witness
on: pull_request
permissions:
  contents: read
  pull-requests: write      # to post the review plan comment
jobs:
  review-plan:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0     # the merge base must be in the clone
      - uses: radical-studios/witness@v0
        with:
          fail-on: failing   # optional: fail the check when tests failed after the agent's last edit
```

The action installs the release binary, annotates the lines to review
closely in the diff, posts (or updates) one comment on the pull request,
and writes the plan to the job summary.

| Input | Default | |
| --- | --- | --- |
| `version` | `latest` | Release tag to install, or `none` to use a `witness` already on `PATH`. |
| `base` | the PR's base branch | Ref to compare with. |
| `comment` | `true` | Post or update the comment; with `false`, only the job summary is written. |
| `annotations` | `true` | Inline annotations for close-review sections. |
| `fail-on` | | Statuses or signals that fail the step: `failing`, `guessed`, `unrecorded`, `unverified`, `compiled`, `tested`, `test-changed-after-failure`, `assertions-removed`, `high-risk-area`, `blind-edit`, `thrash`, `filtered-test-run`, `human-modified`. |
| `token` | `github.token` | Token for commenting. |

**Pull requests from forks** run with a read-only token, so the comment
can't be posted. Witness then prints a warning, keeps the plan in the job
summary and annotations, and does not fail the step.

**GitHub Enterprise Server**: `GITHUB_API_URL` is read from the runner.

**Code scanning**: upload SARIF to show the plan in the Security tab and
on the diff:

```yaml
      - run: witness report --format sarif > witness.sarif
      - uses: github/codeql-action/upload-sarif@v3
        with:
          sarif_file: witness.sarif
```

(This needs `security-events: write`.)

## GitLab CI

```yaml
include:
  - remote: https://raw.githubusercontent.com/radical-studios/witness/main/ci/gitlab/witness.yml
```

The job runs on merge request pipelines, publishes a Code Quality report
(shown in the merge request widget and on the diff) and, when the masked
variable `WITNESS_GITLAB_TOKEN` holds a project access token with the `api`
scope, posts or updates one note on the merge request. CI job tokens can't
write notes.

## Other CI systems

Install with `install.sh` (Linux, macOS) or `install.ps1` (Windows), fetch
full history, and run:

```bash
witness report --base origin/main --format markdown   # or text, json, sarif
witness report --base origin/main --fail-on failing    # exits 1 on a match
```

`witness post --platform github|gitlab` works anywhere given the
repository, number and token: `--repo`, `--number`, `GITHUB_TOKEN` or
`WITNESS_GITLAB_TOKEN`, and `GITHUB_API_URL` / `CI_API_V4_URL` for
self-hosted servers.

## How the base and head are found

- Base: `--base`, else `WITNESS_BASE`, else the CI's target branch
  (`GITHUB_BASE_REF`, `CI_MERGE_REQUEST_DIFF_BASE_SHA`,
  `CI_MERGE_REQUEST_TARGET_BRANCH_NAME`), else origin's default branch,
  else `main` or `master`.
- Head: `HEAD`. GitHub builds pull requests on a temporary merge commit;
  the diff is the same, and links in the comment point at the PR's own
  head commit.
- A shallow clone can lack the merge base; Witness says so and asks for
  `fetch-depth: 0` (GitHub) or `GIT_DEPTH: 0` (GitLab).

## How posting works

Posting uses the `curl` binary, so proxies (`HTTPS_PROXY`) and the system
certificate store work as for any other tool on the runner. The token is
passed to curl on stdin, never on its command line. The comment carries a
hidden `<!-- witness:report -->` marker; later runs update that comment
instead of adding new ones.
