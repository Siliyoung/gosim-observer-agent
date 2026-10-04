import os
import unittest
from unittest.mock import patch

from agent_core.llm_client import MissingAPIKeyError, llm_enabled, require_api_key


class LLMConfigurationTests(unittest.TestCase):
    def test_llm_is_disabled_by_default_and_needs_no_key(self):
        with patch.dict(os.environ, {}, clear=True):
            self.assertFalse(llm_enabled())
            require_api_key()

    def test_opt_in_llm_requires_a_key(self):
        with patch.dict(os.environ, {"AGENT_USE_LLM": "1"}, clear=True):
            self.assertTrue(llm_enabled())
            with self.assertRaises(MissingAPIKeyError):
                require_api_key()

    def test_opt_in_llm_accepts_kimi_key(self):
        with patch.dict(os.environ, {"AGENT_USE_LLM": "true", "KIMI_API_KEY": "test"}, clear=True):
            require_api_key()


if __name__ == "__main__":
    unittest.main()
