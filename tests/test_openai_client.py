from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from agentdictate.openai_client import OpenAIClient, extract_response_text


class OpenAITests(unittest.TestCase):
    def test_extract_response_text(self) -> None:
        payload = {
            "output": [
                {"type": "reasoning", "summary": []},
                {
                    "type": "message",
                    "content": [
                        {"type": "output_text", "text": "Cleaned prompt."},
                    ],
                },
            ]
        }
        self.assertEqual(extract_response_text(payload), "Cleaned prompt.")

    @patch("agentdictate.openai_client.requests.post")
    def test_transcription_request_shape(self, post: Mock) -> None:
        response = Mock()
        response.ok = True
        response.text = "raw transcript"
        post.return_value = response
        with tempfile.TemporaryDirectory() as directory:
            audio_path = Path(directory) / "speech.wav"
            audio_path.write_bytes(b"RIFFfake")
            client = OpenAIClient("sk-test")
            text = client.transcribe(
                audio_path, "gpt-4o-transcribe", language="en", prompt="coding terms"
            )
        self.assertEqual(text, "raw transcript")
        self.assertEqual(client.last_transcription_model, "gpt-4o-transcribe")
        self.assertEqual(post.call_args.args[0], "https://api.openai.com/v1/audio/transcriptions")
        data = post.call_args.kwargs["data"]
        self.assertEqual(data["model"], "gpt-4o-transcribe")
        self.assertEqual(data["response_format"], "text")
        self.assertEqual(data["language"], "en")
        self.assertIn("Transcribe the entire recording", data["prompt"])
        self.assertIn("coding terms", data["prompt"])

    @patch("agentdictate.openai_client.requests.post")
    def test_transcription_retries_single_sentence_long_recording_with_whisper(
        self, post: Mock
    ) -> None:
        first_response = Mock()
        first_response.ok = True
        first_response.text = "This is only the first sentence."
        second_response = Mock()
        second_response.ok = True
        second_response.text = "This is only the first sentence. This is the second sentence too."
        post.side_effect = [first_response, second_response]
        with tempfile.TemporaryDirectory() as directory:
            audio_path = Path(directory) / "speech.wav"
            audio_path.write_bytes(b"RIFFfake")
            client = OpenAIClient("sk-test")
            text = client.transcribe(
                audio_path,
                "gpt-4o-transcribe",
                audio_duration_seconds=8.0,
            )
        self.assertEqual(text, "This is only the first sentence. This is the second sentence too.")
        self.assertEqual(client.last_transcription_model, "whisper-1")
        self.assertEqual(post.call_args_list[0].kwargs["data"]["model"], "gpt-4o-transcribe")
        self.assertEqual(post.call_args_list[1].kwargs["data"]["model"], "whisper-1")

    @patch("agentdictate.openai_client.requests.post")
    def test_cleanup_request_shape(self, post: Mock) -> None:
        response = Mock()
        response.ok = True
        response.json.return_value = {
            "output": [
                {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "Cleaned prompt."}],
                }
            ]
        }
        post.return_value = response
        text = OpenAIClient("sk-test").cleanup("raw", "gpt-5.4-nano", "instruction")
        self.assertEqual(text, "Cleaned prompt.")
        self.assertEqual(post.call_args.args[0], "https://api.openai.com/v1/responses")
        payload = json.loads(post.call_args.kwargs["data"])
        self.assertEqual(payload["model"], "gpt-5.4-nano")
        self.assertNotIn("reasoning", payload)

    @patch("agentdictate.openai_client.requests.post")
    def test_cleanup_reasoning_effort_request_shape(self, post: Mock) -> None:
        response = Mock()
        response.ok = True
        response.json.return_value = {
            "output": [
                {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "Cleaned prompt."}],
                }
            ]
        }
        post.return_value = response
        text = OpenAIClient("sk-test").cleanup(
            "raw", "gpt-5.4-nano", "instruction", reasoning_effort="high"
        )
        self.assertEqual(text, "Cleaned prompt.")
        payload = json.loads(post.call_args.kwargs["data"])
        self.assertEqual(payload["reasoning"]["effort"], "high")
