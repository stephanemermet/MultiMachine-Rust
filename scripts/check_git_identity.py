#!/usr/bin/env python3
"""Check raw Git identities and email trailers before publishing this repository.

Run without arguments for all local refs, with refs to audit a publication, or
with --pre-push to consume Git's pre-push input. Does not inspect file contents.
Never print a rejected identity: it may itself contain personal information.
"""
import re
import subprocess
import sys

PUBLIC_IDENTITIES = {
    (b"stephanemermet", b"51788154+stephanemermet@users.noreply.github.com"),
    (b"GitHub", b"noreply@github.com"),
}
PUBLIC_EMAILS = {email for _, email in PUBLIC_IDENTITIES}
EMAIL = re.compile(rb"[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
IDENTITY = re.compile(rb"(author|committer|tagger) (.*?) <([^<>]+)> -?\d+ [+-]\d{4}")


def git(*args):
    return subprocess.check_output(["git", *args], stderr=subprocess.PIPE)


def check_object(oid, kind):
    raw = git("cat-file", kind, oid)
    header, _ = raw.split(b"\n\n", 1)
    required = {b"author", b"committer"} if kind == "commit" else {b"tagger"}
    seen = set()
    for line in header.splitlines():
        role = line.split(b" ", 1)[0]
        if role in required:
            match = IDENTITY.fullmatch(line)
            if not match or (match[2], match[3]) not in PUBLIC_IDENTITIES:
                raise ValueError(f"{oid[:12]}: {role.decode()} identity is not approved for publication")
            seen.add(role)
    if seen != required:
        raise ValueError(f"{oid[:12]}: incomplete Git identity")
    if any(email not in PUBLIC_EMAILS for email in EMAIL.findall(raw)):
        raise ValueError(f"{oid[:12]}: unapproved email in metadata or message")


def check_refs(refs):
    commits = git("rev-list", *refs, "--").decode().splitlines()
    for oid in commits:
        check_object(oid, "commit")
    tags = set()
    for ref in refs:
        oid = git("rev-parse", "--verify", ref).decode().strip()
        while git("cat-file", "-t", oid).strip() == b"tag":
            if oid in tags:
                break
            check_object(oid, "tag")
            tags.add(oid)
            oid = git("cat-file", "tag", oid).splitlines()[0].split()[1].decode()
    return len(commits), len(tags)


def main():
    refs = sys.argv[1:]
    if refs == ["--pre-push"]:
        refs = []
        for line in sys.stdin:
            fields = line.split()
            if len(fields) != 4 or not re.fullmatch(r"[0-9a-f]{40,64}", fields[1]):
                raise ValueError("Malformed pre-push input")
            if fields[1].strip("0"):
                refs.append(fields[1])
    elif not refs:
        refs = git("for-each-ref", "--format=%(refname)").decode().splitlines()
    if refs:
        commits, tags = check_refs(refs)
        print(f"Git privacy check passed: {commits} commits, {tags} annotated tags.")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, subprocess.CalledProcessError, OSError):
        print("Publication blocked: Git metadata contains an unapproved identity or could not be verified. "
              "Use the public identity documented in AGENTS.md; do not bypass this check.", file=sys.stderr)
        sys.exit(1)
