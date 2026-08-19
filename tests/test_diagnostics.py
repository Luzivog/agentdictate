from __future__ import annotations

import logging.handlers
import tempfile
import unittest
from pathlib import Path

from agentdictate.diagnostics import configure_logging, log_event, shutdown_logging


class DiagnosticsTests(unittest.TestCase):
    def tearDown(self) -> None:
        shutdown_logging()

    def test_rotating_diagnostics_redact_secrets_and_transcript_content(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            log_path = configure_logging(Path(directory))

            log_event(
                "dictation_failed",
                job_id=42,
                stage="transcribing",
                error="request failed with sk-super-secret",
                transcript="words that must stay private",
            )

            contents = log_path.read_text(encoding="utf-8")
            handlers = logging.getLogger("agentdictate").handlers

        self.assertIn('"event": "dictation_failed"', contents)
        self.assertIn('"timestamp":', contents)
        self.assertIn('"job_id": 42', contents)
        self.assertIn('"stage": "transcribing"', contents)
        self.assertIn("[redacted]", contents)
        self.assertNotIn("sk-super-secret", contents)
        self.assertNotIn("words that must stay private", contents)
        self.assertTrue(any(isinstance(handler, logging.handlers.RotatingFileHandler) for handler in handlers))


if __name__ == "__main__":
    unittest.main()
