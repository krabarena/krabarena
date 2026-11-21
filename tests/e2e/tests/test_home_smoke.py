from __future__ import annotations

import pytest

from framework.action_bot import ActionBot
from helpers.results import TestArtifacts
from pages.home_page import HomePage


@pytest.mark.smoke
def test_home_smoke(page, base_url: str, e2e_artifacts: TestArtifacts) -> None:
    e2e_artifacts.add_runtime_data("scenario", "home-smoke")

    bot = ActionBot(e2e_artifacts)
    home = HomePage(page, bot, base_url=base_url).load()
    feed_state = home.classify_battles_feed_state()

    e2e_artifacts.add_runtime_data("feed_state", feed_state)
    e2e_artifacts.set_current_url(page.url)
