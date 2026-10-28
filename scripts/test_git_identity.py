"""Integration checks for the publication guard; uses disposable Git repositories."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parent.parent
NAME = "stephanemermet"
EMAIL = "51788154+stephanemermet@users.noreply.github.com"


class PrivacyChecks(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name) / "repo"
        self.repo.mkdir()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        self.env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
                        GIT_AUTHOR_NAME=NAME, GIT_AUTHOR_EMAIL=EMAIL,
                        GIT_COMMITTER_NAME=NAME, GIT_COMMITTER_EMAIL=EMAIL)
        self.git("init", "--initial-branch=main")

    def git(self, *args, env=None, check=True):
        return subprocess.run(["git", *args], cwd=self.repo, env=env or self.env,
                              capture_output=True, check=check, text=True)

    def commit(self, env=None, message="Synthetic verification fixture"):
        return self.git("commit", "--allow-empty", "-m", message, env=env)

    def check(self, expected, *refs, stdin=None):
        result = subprocess.run([sys.executable, str(SOURCE / "scripts/check_git_identity.py"), *refs],
                                cwd=self.repo, env=self.env, input=stdin, capture_output=True, text=True)
        self.assertEqual(result.returncode, expected, result.stderr)
        self.assertNotIn("private@example.test", result.stdout + result.stderr)

    def test_public_history_and_tags(self):
        self.commit()
        self.git("tag", "-a", "test", "-m", "Public tag")
        self.check(0)

    def test_private_author(self):
        self.commit(env={**self.env, "GIT_AUTHOR_EMAIL": "private@example.test"})
        self.check(1, "HEAD")

    def test_private_committer(self):
        self.commit(env={**self.env, "GIT_COMMITTER_EMAIL": "private@example.test"})
        self.check(1, "HEAD")

    def test_private_tagger(self):
        self.commit()
        self.git("tag", "-a", "test", "-m", "Test", env={**self.env, "GIT_COMMITTER_EMAIL": "private@example.test"})
        self.check(1, "test")
        self.check(0, "HEAD")

    def test_private_name(self):
        self.commit(env={**self.env, "GIT_AUTHOR_NAME": "Unapproved personal name"})
        self.check(1)

    def test_private_ancestor_cannot_be_hidden_by_a_clean_tip(self):
        self.commit(env={**self.env, "GIT_AUTHOR_EMAIL": "private@example.test"})
        self.commit()
        self.check(1, "HEAD")

    def test_private_trailer(self):
        self.commit(message="Test\n\nCo-authored-by: Example <private@example.test>")
        self.check(1)

    def test_push_input_and_deletion(self):
        self.commit()
        oid = self.git("rev-parse", "HEAD").stdout.strip()
        self.check(0, "--pre-push", stdin=f"refs/heads/main {oid} refs/heads/main {'0' * 40}\n")
        self.check(0, "--pre-push", stdin=f"(delete) {'0' * 40} refs/heads/main {oid}\n")
        self.check(1, "--pre-push", stdin="malformed\n")

    def test_real_push_is_rejected_then_clean_push_works(self):
        (self.repo / "scripts").mkdir()
        shutil.copy2(SOURCE / "scripts/check_git_identity.py", self.repo / "scripts")
        shutil.copytree(SOURCE / ".githooks", self.repo / ".githooks")
        self.git("config", "core.hooksPath", ".githooks")
        remote = Path(self.temp.name) / "remote.git"
        self.git("init", "--bare", str(remote))
        self.commit(env={**self.env, "GIT_AUTHOR_EMAIL": "private@example.test"})
        self.assertNotEqual(self.git("push", str(remote), "HEAD:main", check=False).returncode, 0)
        self.assertEqual(self.git("ls-remote", str(remote)).stdout, "")
        self.git("commit", "--amend", "--allow-empty", "--reset-author", "--no-edit")
        self.git("push", str(remote), "HEAD:main")
        self.assertIn("refs/heads/main", self.git("ls-remote", str(remote)).stdout)


if __name__ == "__main__":
    unittest.main()
