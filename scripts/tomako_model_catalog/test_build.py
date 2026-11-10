import unittest

from build import assemble


class CatalogTests(unittest.TestCase):
    def test_upgrade_prefers_new_bundled_descriptor_and_keeps_new_models(self):
        bundled = {"m": {"slug": "m", "version": 2}, "new": {"slug": "new"}}
        stale = {"m": {"slug": "m", "version": 1}}
        actual = assemble(bundled, stale, {"m": "m", "a/b/m": "m"}, ["a/b/m"])
        self.assertEqual(actual, {**bundled, "a/b/m": {"slug": "a/b/m", "version": 2}})

    def test_release_requires_exact_route_even_if_prefix_would_match(self):
        with self.assertRaisesRegex(ValueError, "without exact metadata"):
            assemble({"m": {"slug": "m"}}, {}, {}, ["m-v2"])

    def test_missing_source_and_shadowed_alias_fail_closed(self):
        with self.assertRaisesRegex(ValueError, "missing descriptor"):
            assemble({"old": {"slug": "old"}}, {}, {"new": "unknown"}, ["new"])
        models = {"m": {"slug": "m", "mode": 1}, "x": {"slug": "x", "mode": 2}}
        with self.assertRaisesRegex(ValueError, "collides"):
            assemble(models, {}, {"x": "m"}, ["x"])


if __name__ == "__main__":
    unittest.main()
