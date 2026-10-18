"""Static checks for explicit task approval; no Cargo build or live broker needed."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]


def source(name):
    return (ROOT / "src" / name).read_text()


def function(text, name):
    start = text.index("{", text.index(f"fn {name}("))
    depth = 1
    end = start + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start:end]


class ExplicitAgentStart(unittest.TestCase):
    def test_start_is_a_separate_client_call(self):
        events = source("events.rs")
        start = function(events, "start_agent_task")
        self.assertIn("self.direct_agent.clone()", start)
        self.assertIn("self.agent_tasks.start(&agent)", start)
        self.assertEqual(events.count("self.agent_tasks.start("), 1)
        self.assertIn("Hit::AgentStart =>", events)

    def test_paint_and_measure_share_the_readiness_gated_control(self):
        render = source("render_chat.rs")
        control = function(render, "agent_task_run_control")
        self.assertIn("tasks.can_start()", control)
        self.assertIn("Hit::AgentStart", control)
        self.assertIn("tasks.can_stop()", control)
        self.assertIn("Hit::AgentStop", control)
        self.assertGreaterEqual(render.count("self.agent_task_run_control(tasks)"), 2)
        self.assertIn("AgentTaskReceipt::Started", render)

    def test_semantic_effect_is_explicit_not_a_chat_record_mutation(self):
        effects = source("effects.rs")
        self.assertRegex(effects, r"#\[effect\(device_only\)\]\s+StartAgentTask,")
        dispatch = function(effects, "answer_effect")
        self.assertIn(
            "ChatEffect::StartAgentTask => self.answer_task_effect(TaskEffect::StartAgentTask)",
            dispatch,
        )
        task_dispatch = function(effects, "answer_task_effect")
        self.assertRegex(task_dispatch, r"TaskEffect::StartAgentTask => \{\s+if !self.start_agent_task\(\)")
        self.assertIn("ChatEffect::START_AGENT_TASK", function(effects, "effect_statuses"))
        # surface.toml describes kept record verbs, not device effects.
        self.assertNotIn('id = "start-agent-task"', (ROOT / "surface.toml").read_text())

    def test_task_controls_are_registered_on_the_live_resource_tree(self):
        effects = source("effects.rs")
        row = function(effects, "task_list_row")
        for control in ("NewTask", "SaveTaskGoal", "StartAgentTask", "StopAgentTask", "SetupTaskModels"):
            self.assertIn(f".effect(&ChatEffect::{control})", row)
        for control in ("SetTaskGoal", "QueryTasks", "FilterTasks"):
            self.assertIn(f"ChatEffect::{control}", row)
        published = function(effects, "published_rows")
        self.assertIn("let task_list = Self::task_list_row();", published)
        self.assertIn(".chain(std::iter::once(task_list))", published)
        self.assertIn("self.published_rows()", function(effects, "published_row"))

    def test_pointer_accessibility_and_semantic_target_agree(self):
        app = source("app.rs")
        self.assertIn('Self::AgentStart => "chat.agent-start".into()', app)
        self.assertIn('Hit::AgentStart => (crate::i18n::t("chat-agent-start"), Role::Button, false)', app)
        self.assertRegex(app, r"(?s)matches!\(hit, Hit::AgentStart\).*?return self.start_agent_task\(\)")
        catalog = (ROOT / "locales" / "en-US.ftl").read_text()
        self.assertIn("chat-agent-start = Start", catalog)
        self.assertIn("chat-agent-task-started = Start approved", catalog)


if __name__ == "__main__":
    unittest.main()
