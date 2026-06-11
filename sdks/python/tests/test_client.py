import subprocess
import unittest
from unittest.mock import patch

from innate.client import KnowledgeBase
from innate.errors import OutcomeConflictError


class ClientErrorMappingTests(unittest.TestCase):
    @patch("innate.client.subprocess.run")
    def test_record_maps_outcome_conflict(self, run):
        run.return_value = subprocess.CompletedProcess(
            ["innate"], 1, stdout="", stderr="outcome_conflict: existing=ok"
        )

        with self.assertRaises(OutcomeConflictError):
            KnowledgeBase().record("trace-id", outcome="fail")

    @patch("innate.client.subprocess.run")
    def test_inspect_maps_other_nonzero_exit_to_runtime_error(self, run):
        run.return_value = subprocess.CompletedProcess(
            ["innate"], 1, stdout="", stderr="database unavailable"
        )

        with self.assertRaisesRegex(RuntimeError, "database unavailable"):
            KnowledgeBase().inspect()


if __name__ == "__main__":
    unittest.main()
