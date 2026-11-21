# Public and private repositories

KrabArena lives in two GitHub repositories with the same code:

| Repo | Visibility | What happens there |
|---|---|---|
| [`krabarena/krabarena`](https://github.com/krabarena/krabarena) | public | All development: issues, pull requests, reviews, CI |
| `krabarena/krabarena-private` | private | Deploys: Build & Push, Deploy to Test, Deploy, Release, secrets |

The public repo is the source of truth for code. The private repo holds
the deploy secrets and the full pre-open-source history, and ships
whatever the public `main` contains.

```
contributor PR ──► public main ──(Sync from Public, hourly)──► sync PR in private
                                                                   │ merge
                                                                   ▼
                                     Build & Push ─► Deploy to Test + E2E ─► Deploy (prod)
```

## Rules

1. **Change code in public.** Open every PR against `krabarena/krabarena`.
2. **Private stays equal to public.** The private repo must not carry its
   own code changes, or the next sync conflicts. The only exception is an
   emergency hotfix, which you upstream right away (below).
3. **Secrets never go in either repo.** They live in the private repo's
   GitHub Actions secrets and are mirrored to Secret Manager (see
   AGENTS.md, "Secrets: GitHub is the source of truth").
4. **Deploy workflows run only in private.** Their jobs are guarded by
   `if: github.repository == 'krabarena/krabarena-private'`, so in the
   public repo they are skipped. GCP Workload Identity trusts only the
   private repo, so the public repo cannot obtain deploy credentials.

## Public → private (every change)

Automatic: the `Sync from Public` workflow in the private repo runs every
hour (or on demand) and calls `scripts/sync-from-public.sh --ci`:

1. fetches public `main`;
2. stops if it is already merged;
3. otherwise merges it into a `sync/public-<sha>` branch and opens a PR in
   the private repo listing the public commits.

A maintainer merges that PR; the normal pipeline ships it to test and
prod. To trigger it right away:

```sh
gh workflow run "Sync from Public" --repo krabarena/krabarena-private
```

Or do the same from a local clone of the private repo:

```sh
scripts/sync-from-public.sh
```

If the script reports a merge conflict, the private repo has drifted
(rule 2). Resolve it locally on the `sync/public-<sha>` branch, push,
and open the PR by hand.

## Private → public (emergency hotfix only)

If a fix had to land in private first, send the same change to public
so the next sync brings it back cleanly:

```sh
scripts/upstream-to-public.sh <commit>          # one commit
scripts/upstream-to-public.sh main~3..main      # a range; merge commits skipped
```

The script cherry-picks the commits onto a branch from public `main`,
runs `gitleaks` over them (or asks you to review the diff when gitleaks
is not installed), pushes the branch to the public repo and opens a PR
there. Never upstream anything that references secrets, private
infrastructure or customer data.

## Local setup for maintainers

```sh
git clone git@github.com:krabarena/krabarena-private.git
cd krabarena-private
git remote add public git@github.com:krabarena/krabarena.git
```

Contributors only need the public repo.

## How the histories are connected

The public repo starts from a single snapshot commit of the private
`main`. The private repo merged that snapshot once with
`git merge -s ours --allow-unrelated-histories`, which records it as the
common ancestor without changing any file. From then on every public
commit merges into private as an ordinary merge.
