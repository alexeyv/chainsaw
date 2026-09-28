import re
import unittest

from tests.support import PROJECT_ROOT

SRC = PROJECT_ROOT / "src"
#: Each layer and the layers below it that its sources may name. A layer may
#: always name itself. Anything above is off limits, which is what keeps the
#: dependency graph acyclic.
LAYERS = {
    "domain": set(),
    "infra": {"domain"},
    "persistence": {"domain", "infra"},
    "run": {"domain", "infra", "persistence"},
}


def sources_of(layer):
    paths = [SRC / f"{layer}.rs"]
    paths += sorted((SRC / layer).rglob("*.rs"))
    return [path for path in paths if path.exists()]


def crate_modules_named_in(path):
    return set(re.findall(r"\bcrate::(\w+)", path.read_text()))


class LayeringTests(unittest.TestCase):
    def test_lower_layers_only_depend_downward(self):
        for layer, below in LAYERS.items():
            allowed = below | {layer}
            for path in sources_of(layer):
                named = crate_modules_named_in(path)
                self.assertLessEqual(
                    named, allowed,
                    f"{path.relative_to(PROJECT_ROOT)} reaches above its layer",
                )

    def test_every_layer_has_sources(self):
        for layer in LAYERS:
            self.assertTrue(sources_of(layer), layer)


if __name__ == "__main__":
    unittest.main()
