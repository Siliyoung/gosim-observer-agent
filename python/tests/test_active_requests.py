import unittest
from types import SimpleNamespace

from agent_core.geometry import parse_utc
from agent_core.planner import Planner


class ActiveRequestSchedulingTests(unittest.TestCase):
    def make_planner(self):
        planner = Planner.__new__(Planner)
        planner.state = SimpleNamespace(
            index_of={"A": 0, "B": 1},
            hmax=[30.0, 30.0],
            active=[],
            scoring=SimpleNamespace(required_threshold=0.5),
        )
        planner.request_goals = {}
        planner._request_signature = ()
        planner.log = lambda _message: None
        return planner

    def test_unfinished_targets_are_prioritized_and_reactivated(self):
        planner = self.make_planner()
        now = parse_utc("2026-11-10T00:00:00Z")
        payload = {
            "active_requests": [{
                "request_id": "RQ1",
                "deadline_utc": "2026-11-11T08:00:00Z",
                "target_ids": ["A", "B"],
                "minimum_completed": 1,
                "completed_target_ids": [],
                "completed_count": 0,
                "completion_factor_threshold": 0.5,
                "completion_reward": 100.0,
            }]
        }

        planner._sync_active_requests(payload, now)

        self.assertEqual(set(planner.request_goals), {0, 1})
        self.assertEqual(set(planner.state.active), {0, 1})
        self.assertGreater(planner._request_value(0), 0.0)
        self.assertGreater(planner._request_gain(0, 0.5, now, 900), 0.0)

    def test_completed_or_late_exposures_receive_no_request_value(self):
        planner = self.make_planner()
        now = parse_utc("2026-11-10T00:00:00Z")
        request = {
            "request_id": "RQ1",
            "deadline_utc": "2026-11-10T00:10:00Z",
            "target_ids": ["A", "B"],
            "minimum_completed": 1,
            "completed_target_ids": [],
            "completed_count": 0,
            "completion_factor_threshold": 0.5,
            "completion_reward": 100.0,
        }
        planner._sync_active_requests({"active_requests": [request]}, now)

        self.assertEqual(planner._request_gain(0, 0.49, now, 300), 0.0)
        self.assertEqual(planner._request_gain(0, 0.8, now, 900), 0.0)

        request["completed_target_ids"] = ["A"]
        request["completed_count"] = 1
        planner._sync_active_requests({"active_requests": [request]}, now)
        self.assertEqual(planner.request_goals, {})


if __name__ == "__main__":
    unittest.main()
