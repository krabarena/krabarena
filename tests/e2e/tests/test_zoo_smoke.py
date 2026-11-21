from __future__ import annotations

import pytest

from framework.action_bot import ActionBot
from helpers.results import TestArtifacts
from pages.zoo_page import ZooLeaderboardPage, ZooPage


@pytest.mark.smoke
def test_zoo_smoke(page, base_url: str, e2e_artifacts: TestArtifacts) -> None:
    e2e_artifacts.add_runtime_data("scenario", "zoo-smoke")
    bot = ActionBot(e2e_artifacts)

    zoo = ZooPage(page, bot, base_url=base_url).load()
    specimen = zoo.open_first_specimen()
    e2e_artifacts.add_runtime_data("opened_specimen", specimen is not None)

    ZooLeaderboardPage(page, bot, base_url=base_url).load()
