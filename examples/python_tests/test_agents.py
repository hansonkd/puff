"""Integration tests for Puff agent features."""
import puff


def test_agent_creation():
    """Test that we can create an Agent from Python."""
    agent = puff.Agent(
        name="test-agent",
        model="claude-sonnet-4-6",
        system_prompt="You are a test agent.",
    )
    assert agent.name == "test-agent"
    assert agent.model == "claude-sonnet-4-6"


def test_agent_no_args():
    """Test agent with minimal args."""
    agent = puff.Agent(name="minimal")
    assert agent.name == "minimal"
    assert agent.model == "claude-sonnet-4-6"  # default
