from __future__ import annotations

import pytest

from framework.action_bot import ActionBot
from helpers.results import TestArtifacts
from pages.battle_page import BattlePage
from pages.claim_page import ClaimPage
from pages.home_page import HomePage

MAX_BATTLES_TO_SCAN = 5


@pytest.mark.smoke
def test_claim_detail_smoke(
    page,
    base_url: str,
    e2e_artifacts: TestArtifacts,
) -> None:
    e2e_artifacts.add_runtime_data("scenario", "claim-detail-smoke")
    e2e_artifacts.add_runtime_data("max_battles_to_scan", MAX_BATTLES_TO_SCAN)

    bot = ActionBot(e2e_artifacts)
    home = HomePage(page, bot, base_url=base_url).load()
    scanned_battles = []

    for index in range(MAX_BATTLES_TO_SCAN):
        battle = home.battle_at(index)
        if battle is None:
            break

        scanned_battles.append({"title": battle.title, "href": battle.href})
        e2e_artifacts.add_runtime_data("scanned_battles", scanned_battles)
        e2e_artifacts.add_runtime_data("scan_index", index)

        bot.click(
            home.battle_links.nth(index),
            description=f"Open Battle candidate {index + 1}",
        )
        page.wait_for_load_state("domcontentloaded")
        e2e_artifacts.set_current_url(page.url)

        battle_page = BattlePage(page, bot, base_url=base_url)
        battle_page.expect_loaded()
        claim = battle_page.open_first_claim()

        if claim is not None:
            e2e_artifacts.add_runtime_data("selected_battle_title", battle.title)
            e2e_artifacts.add_runtime_data("selected_battle_href", battle.href)
            e2e_artifacts.add_runtime_data("selected_claim_title", claim.title)
            e2e_artifacts.add_runtime_data("selected_claim_href", claim.href)
            e2e_artifacts.add_runtime_data("scanned_count", len(scanned_battles))

            claim_page = ClaimPage(page, bot, base_url=base_url)
            claim_page.expect_loaded()
            claim_page.expect_structured_body()
            claim_page.recover_to_battle()
            e2e_artifacts.set_current_url(page.url)
            return

        home = battle_page.recover_to_home()

    skip_reason = (
        f"no visible public Claims found after scanning "
        f"{len(scanned_battles)} Battle(s)"
    )
    e2e_artifacts.add_runtime_data("skip_reason", skip_reason)
    e2e_artifacts.add_runtime_data("scanned_count", len(scanned_battles))
    pytest.skip(skip_reason)
