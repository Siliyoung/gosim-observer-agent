import unittest
from types import SimpleNamespace

from agent_core.planner import Planner


class PlannerPriorityTests(unittest.TestCase):
    def test_required_penalty_is_restored_on_last_visible_night(self):
        planner = Planner.__new__(Planner)
        planner.request_goals = {}
        planner.state = SimpleNamespace(
            factor=[0.0],
            required=[True],
            last_night=[4],
            scoring=SimpleNamespace(required_threshold=0.5, required_penalty=50.0),
        )

        self.assertEqual(planner._required_completion_value(0, 3, 0.1), 6.0)
        self.assertEqual(planner._required_completion_value(0, 4, 0.1), 60.0)

    def test_fast_mode_fibers_follow_layout_size(self):
        planner = Planner.__new__(Planner)
        for side in (3, 4, 5, 10):
            planner.grid = SimpleNamespace(side=side, n=side * side)
            fibers = planner._representative_fibers()
            self.assertTrue(fibers)
            self.assertTrue(all(0 <= fiber < side * side for fiber in fibers))
            self.assertIn((side // 2) * side + side // 2, fibers)


if __name__ == "__main__":
    unittest.main()
