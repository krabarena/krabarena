from __future__ import annotations

import pytest

from framework.action_bot import ActionBot
from helpers.results import TestArtifacts
from pages.battle_page import BattlePage
from pages.home_page import HomePage


@pytest.mark.smoke
def test_battles_list_smoke(
    page,
    base_url: str,
    e2e_artifacts: TestArtifacts,
) -> None:
    e2e_artifacts.add_runtime_data("scenario", "battles-list-smoke")

    bot = ActionBot(e2e_artifacts)
    home = HomePage(page, bot, base_url=base_url).load()
    feed_state = home.classify_battles_feed_state()

    e2e_artifacts.add_runtime_data("feed_state", feed_state)
    e2e_artifacts.set_current_url(page.url)


@pytest.mark.smoke
def test_battle_detail_smoke(
    page,
    base_url: str,
    e2e_artifacts: TestArtifacts,
) -> None:
    e2e_artifacts.add_runtime_data("scenario", "battle-detail-smoke")

    bot = ActionBot(e2e_artifacts)
    home = HomePage(page, bot, base_url=base_url).load()
    battle = home.open_first_battle()

    if battle is None:
        home.expect_empty_battles_state()
        pytest.fail(
            "battle-detail-smoke requires at least one visible Battle on the "
            "target stand"
        )

    battle_page = BattlePage(page, bot, base_url=base_url)
    battle_page.expect_loaded()
    claims_state = battle_page.classify_claims_state()
    battle_page.recover_to_home()

    e2e_artifacts.add_runtime_data("claims_state", claims_state)
    e2e_artifacts.add_runtime_data("detail_url", page.url)
    e2e_artifacts.set_current_url(page.url)
