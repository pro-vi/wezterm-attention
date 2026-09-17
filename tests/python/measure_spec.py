#!/usr/bin/env python3

import importlib.util
import pathlib
import tempfile
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("measure.py")


def load_measure():
    spec = importlib.util.spec_from_file_location("attention_measure", MODULE_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load measurement module")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class MeasurementCommandTest(unittest.TestCase):
    def test_distinct_explicit_artifacts_produce_distinct_commands(self):
        module = load_measure()
        with tempfile.TemporaryDirectory(prefix="attention-measure-spec-") as directory:
            root = pathlib.Path(directory)
            python_writer = root / "attention.py"
            rust_binary = root / "attention-rs"
            python_writer.write_bytes(b"python baseline")
            rust_binary.write_bytes(b"rust candidate")
            rust_command, identities = module.resolve_measurement_artifacts(
                python_writer, rust_binary
            )
            self.assertEqual(rust_command, [str(rust_binary)])
            self.assertNotEqual(
                identities["python_sha256"], identities["rust_sha256"]
            )
            manifest = root / "v2.json"
            manifest.write_bytes(b"{}")
            staged = module.stage_python_baseline(python_writer, manifest, root / "scratch")
            self.assertEqual(pathlib.Path(staged[-1]).read_bytes(), b"python baseline")
            self.assertEqual(
                (root / "scratch/python-baseline/protocol/v2.json").read_bytes(), b"{}"
            )

    def test_same_artifact_identity_is_rejected(self):
        module = load_measure()
        with tempfile.TemporaryDirectory(prefix="attention-measure-spec-") as directory:
            root = pathlib.Path(directory)
            first = root / "first"
            second = root / "second"
            first.write_bytes(b"same implementation")
            second.write_bytes(b"same implementation")
            with self.assertRaisesRegex(ValueError, "same artifact identity"):
                module.resolve_measurement_artifacts(first, second)


if __name__ == "__main__":
    unittest.main()
