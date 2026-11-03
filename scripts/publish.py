#!/usr/bin/env python3
"""Publish the workspace's crates to crates.io.

Shuttle's crates are versioned independently: the wrapper crates mirror the
version of the crate they wrap (`shuttle-tokio` is 1.x because tokio is 1.x),
and the internal `-impl`/`-inner` crates sit on their own 0.1.x lines. So a
release is never "bump the workspace to X" -- it is "some subset of crates got
new versions, publish exactly those".

This script works out that subset by comparing each crate's local version
against what is already on crates.io, then hands the subset to `cargo publish`,
which packages, verifies and uploads them in dependency order.

Everything that can go wrong with a publish -- a stale version requirement on a
sibling crate, missing metadata, a file the packaged crate needs but doesn't
include -- is already diagnosed by cargo, so this script deliberately does not
re-implement any of it. Run with `--dry-run` to get those diagnostics without
uploading.

Usage:
    scripts/publish.py                  # show the plan, change nothing
    scripts/publish.py --dry-run        # plan, then package and verify it
    scripts/publish.py --execute        # plan, then really upload
    scripts/publish.py --all --dry-run  # verify every crate, not just the subset

Publishing is permanent: a version can never be replaced or removed. `--execute`
is the only mode that uploads, and it is a no-op when nothing needs publishing,
so it is safe to re-run after a partial failure.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from typing import NoReturn

SPARSE_INDEX = "https://index.crates.io"
USER_AGENT = "shuttle-publish-script (https://github.com/awslabs/shuttle)"
INDEX_TIMEOUT_SECS = 30
INDEX_ATTEMPTS = 3
INDEX_WORKERS = 8


@dataclass
class Crate:
    name: str
    version: str
    # Names of workspace crates this one depends on, excluding dev-dependencies.
    # Dev-dependencies are irrelevant here: they don't constrain publish order,
    # because cargo drops path-only dev-deps when it packages a crate.
    deps: set[str] = field(default_factory=set)


def die(message: str) -> NoReturn:
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def workspace_crates() -> tuple[list[Crate], list[Crate]]:
    """Return (publishable, skipped) crates.

    Read from `cargo metadata` rather than the `members` list in the root
    Cargo.toml: cargo pulls the path dependencies of members into the workspace
    too, so several crates here are members without being listed. At the time of
    writing 6 of the 30 are, and reading the `members` list would silently miss
    them.
    """
    try:
        proc = subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            capture_output=True,
            text=True,
            check=True,
        )
    except FileNotFoundError:
        die("`cargo` not found on PATH")
    except subprocess.CalledProcessError as exc:
        die(f"`cargo metadata` failed:\n{exc.stderr.strip()}")

    packages = json.loads(proc.stdout)["packages"]
    names = {pkg["name"] for pkg in packages}

    publishable: list[Crate] = []
    skipped: list[Crate] = []
    for pkg in packages:
        crate = Crate(name=pkg["name"], version=pkg["version"])
        for dep in pkg["dependencies"]:
            if dep["name"] in names and dep.get("kind") != "dev":
                crate.deps.add(dep["name"])

        # `publish` is None when unrestricted, [] for `publish = false`, or a
        # list of allowed registries.
        allowed = pkg.get("publish")
        if allowed is None or "crates-io" in allowed:
            publishable.append(crate)
        else:
            skipped.append(crate)

    publishable.sort(key=lambda c: c.name)
    skipped.sort(key=lambda c: c.name)
    return publishable, skipped


def index_path(name: str) -> str:
    """Path to a crate's file in the sparse index, per the registry index spec."""
    lowered = name.lower()
    if len(lowered) <= 2:
        return f"{len(lowered)}/{lowered}"
    if len(lowered) == 3:
        return f"3/{lowered[0]}/{lowered}"
    return f"{lowered[:2]}/{lowered[2:4]}/{lowered}"


def published_versions(name: str) -> set[str]:
    """Versions of `name` already on crates.io. Empty if never published."""
    url = f"{SPARSE_INDEX}/{index_path(name)}"
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})

    last_error: Exception | None = None
    for attempt in range(INDEX_ATTEMPTS):
        try:
            with urllib.request.urlopen(request, timeout=INDEX_TIMEOUT_SECS) as response:
                body = response.read().decode("utf-8")
            break
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                return set()  # never published
            last_error = exc
        except urllib.error.URLError as exc:
            last_error = exc
        if attempt == INDEX_ATTEMPTS - 1:
            die(
                f"could not reach the crates.io index for {name!r} "
                f"after {INDEX_ATTEMPTS} attempts: {last_error}"
            )
        time.sleep(2**attempt)

    versions = set()
    for line in body.splitlines():
        if line.strip():
            versions.add(json.loads(line)["vers"])
    return versions


def plan(crates: list[Crate], consult_registry: bool) -> tuple[list[Crate], list[Crate]]:
    """Split `crates` into (to publish, already on crates.io)."""
    if not consult_registry:
        return list(crates), []

    with ThreadPoolExecutor(max_workers=INDEX_WORKERS) as pool:
        published = dict(
            zip(
                (c.name for c in crates),
                pool.map(lambda c: published_versions(c.name), crates),
            )
        )

    pending = [c for c in crates if c.version not in published[c.name]]
    existing = [c for c in crates if c.version in published[c.name]]
    return pending, existing


def report(
    pending: list[Crate],
    existing: list[Crate],
    unpublishable: list[Crate],
    consult_registry: bool,
) -> None:
    width = max((len(c.name) for c in pending), default=0)

    if pending:
        heading = (
            "Crates to publish (cargo decides the order):"
            if consult_registry
            else "Crates to process (--all, ignoring what is already published):"
        )
        print(heading)
        for crate in pending:
            print(f"  {crate.name:<{width}}  {crate.version}")
    else:
        print("Nothing to publish: every crate's version is already on crates.io.")

    if existing:
        print(f"\nAlready on crates.io, skipping ({len(existing)}):")
        print("  " + ", ".join(f"{c.name} {c.version}" for c in existing))

    if unpublishable:
        print("\nNot published by this workspace (publish = false):")
        print("  " + ", ".join(c.name for c in unpublishable))

    # A crate can be republished without its dependents being republished, and
    # here that is usually deliberate: a dependent whose requirement still
    # matches the new version picks it up on its own, which is how the tokio
    # wrapper releases have worked. Worth surfacing so it stays a decision
    # rather than an oversight.
    if consult_registry and pending:
        names = {c.name for c in pending}
        stale = sorted(
            {
                (dependent.name, dep)
                for dependent in existing
                for dep in dependent.deps
                if dep in names
            }
        )
        if stale:
            print("\nNote: these crates depend on something being published but")
            print("are not being republished themselves. Downstream users pick the")
            print("new version up only if the existing requirement still matches it:")
            for dependent, dep in stale:
                print(f"  {dependent} -> {dep}")


def markdown(
    pending: list[Crate],
    existing: list[Crate],
    unpublishable: list[Crate],
) -> str:
    """Render the plan as the summary a maintainer approves against.

    This is what lands on the workflow run page, so it has to be enough on its
    own to decide yes or no.
    """
    if not pending:
        return (
            "## Nothing to publish\n\n"
            "Every crate's version is already on crates.io, so there is nothing "
            "to approve.\n"
        )

    out = ["## Ready to publish\n"]
    out.append("| Crate | Version |")
    out.append("| --- | --- |")
    for crate in pending:
        out.append(f"| `{crate.name}` | {crate.version} |")

    count = len(pending)
    out.append(
        f"\nApproving the `crates-io` deployment on this run publishes "
        f"{'this crate' if count == 1 else f'these {count} crates'} and nothing "
        f"else. Uploads to crates.io are permanent: a version can never be "
        f"replaced or removed.\n"
    )

    names = {c.name for c in pending}
    stale = sorted(
        {
            (dependent.name, dep)
            for dependent in existing
            for dep in dependent.deps
            if dep in names
        }
    )
    if stale:
        out.append("> [!NOTE]")
        out.append(
            "> These crates depend on something in the list but are not being "
            "republished themselves."
        )
        out.append(
            "> Downstream users pick the new version up only if the existing "
            "requirement still matches it:"
        )
        for dependent, dep in stale:
            out.append(f"> - `{dependent}` &rarr; `{dep}`")
        out.append("")

    if existing:
        out.append(
            f"<details><summary>{len(existing)} crates already on crates.io, "
            f"being skipped</summary>\n"
        )
        out.append(", ".join(f"`{c.name}` {c.version}" for c in existing))
        out.append("\n</details>\n")

    if unpublishable:
        out.append(
            "Not published by this workspace (`publish = false`): "
            + ", ".join(f"`{c.name}`" for c in unpublishable)
            + "\n"
        )

    return "\n".join(out)


def run_cargo(pending: list[Crate], execute: bool, verify: bool, locked: bool) -> int:
    command = ["cargo", "publish"]
    if not execute:
        command.append("--dry-run")
    if not verify:
        command.append("--no-verify")
    if locked:
        command.append("--locked")
    for crate in pending:
        command += ["--package", crate.name]

    print("\n$ " + " ".join(command), flush=True)
    return subprocess.run(command).returncode


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--dry-run",
        action="store_true",
        help="package and verify the crates without uploading",
    )
    mode.add_argument(
        "--execute",
        action="store_true",
        help="really upload to crates.io (permanent, and requires credentials)",
    )
    parser.add_argument(
        "--all",
        action="store_true",
        help="operate on every publishable crate instead of only the unpublished "
        "ones; for validating manifests in CI, and refused with --execute",
    )
    parser.add_argument(
        "--no-verify",
        action="store_true",
        help="skip cargo's build-from-tarball check (faster, weaker)",
    )
    parser.add_argument(
        "--locked",
        action="store_true",
        help="assert Cargo.lock is already up to date; off by default because "
        "this workspace does not commit a lockfile",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="print the plan as JSON instead of running anything",
    )
    parser.add_argument(
        "--markdown",
        action="store_true",
        help="print the plan as markdown instead of running anything; this is "
        "the summary maintainers approve against on the workflow run page",
    )
    args = parser.parse_args()

    if args.all and args.execute:
        die("--all cannot be combined with --execute: a release publishes the "
            "crates that were bumped, not every crate in the workspace")

    publishable, unpublishable = workspace_crates()
    pending, existing = plan(publishable, consult_registry=not args.all)

    if args.json:
        print(
            json.dumps(
                {
                    "publish": [{"name": c.name, "version": c.version} for c in pending],
                    "skip": [{"name": c.name, "version": c.version} for c in existing],
                    "unpublishable": [c.name for c in unpublishable],
                },
                indent=2,
            )
        )
        return 0

    if args.markdown:
        print(markdown(pending, existing, unpublishable))
        return 0

    report(pending, existing, unpublishable, consult_registry=not args.all)

    if not pending:
        return 0
    if not (args.dry_run or args.execute):
        print("\nThis was a plan only. Re-run with --dry-run to verify it, or")
        print("--execute to publish.")
        return 0

    if args.execute:
        print(f"\nPublishing {len(pending)} crate(s) to crates.io. This is permanent.")

    return run_cargo(
        pending,
        execute=args.execute,
        verify=not args.no_verify,
        locked=args.locked,
    )


if __name__ == "__main__":
    sys.exit(main())
