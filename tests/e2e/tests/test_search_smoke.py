from __future__ import annotations

import pytest
from playwright.sync_api import expect

from framework.action_bot import ActionBot
from helpers.results import TestArtifacts
from pages.battle_page import BattlePage
from pages.home_page import HomePage
from pages.search_page import SearchPage

MAX_QUERY_CHARS = 80
MAX_QUERY_WORDS = 8


@pytest.mark.smoke
def test_search_smoke(
    page,
    base_url: str,
    e2e_artifacts: TestArtifacts,
) -> None:
    e2e_artifacts.add_runtime_data("scenario", "search-smoke")

    bot = ActionBot(e2e_artifacts)
    home = HomePage(page, bot, base_url=base_url).load()
    battle = home.first_battle()

    if battle is None:
        home.expect_empty_battles_state()
        pytest.fail(
            "search-smoke requires at least one visible Battle to build a "
            "deterministic user-facing query"
        )

    query = _search_query_from_title(battle.title)
    e2e_artifacts.add_runtime_data("selected_battle_title", battle.title)
    e2e_artifacts.add_runtime_data("selected_battle_href", battle.href)
    e2e_artifacts.add_runtime_data("search_query", query)

    topnav_search_input = page.locator(
        'header.topnav form.search input[name="q"]'
    ).first
    expect(topnav_search_input).to_be_visible()
    bot.fill(
        topnav_search_input,
        query,
        description="Fill top navigation search query",
    )
    bot.record_step("submit_topnav_search", query=query)
    topnav_search_input.press("Enter")
    page.wait_for_load_state("domcontentloaded")
    e2e_artifacts.set_current_url(page.url)

    search_page = SearchPage(page, bot, base_url=base_url)
    search_page.expect_loaded()
    search_page.submit_battle_search(query)
    search_page.expect_results_for(query)
    search_page.open_first_result()

    battle_page = BattlePage(page, bot, base_url=base_url)
    battle_page.expect_loaded()

    search_page.recover_from_result_detail(query)
    e2e_artifacts.set_current_url(page.url)


def _search_query_from_title(title: str) -> str:
    normalized = " ".join(title.split())
    assert normalized, "expected visible Battle title to be non-empty"

    if len(normalized) <= MAX_QUERY_CHARS:
        return normalized

    words = normalized.split()
    query = " ".join(words[:MAX_QUERY_WORDS])
    if len(query) <= MAX_QUERY_CHARS:
        return query

    return query[:MAX_QUERY_CHARS].rstrip()
