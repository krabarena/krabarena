from __future__ import annotations

from dataclasses import dataclass

from playwright.sync_api import Page, Response, expect

from framework.action_bot import ActionBot
from framework.loadable import LoadableComponent
from helpers.page_health import (
    assert_no_client_side_crash,
    assert_no_generic_server_error,
    assert_page_has_visible_body,
    assert_page_is_healthy,
)


@dataclass(frozen=True)
class BattleSummary:
    title: str
    href: str


class HomePage(LoadableComponent):
    def __init__(self, page: Page, bot: ActionBot, *, base_url: str) -> None:
        self.page = page
        self.bot = bot
        self.base_url = base_url
        self.response: Response | None = None

    @property
    def main_content(self):
        return self.page.locator("#main-content")

    @property
    def home_feed(self):
        return self.page.locator("#home-feed[data-home-feed]")

    @property
    def battles_tab(self):
        return self.page.locator('.tab[data-tab="battles"]')

    @property
    def battles_panel(self):
        return self.page.locator('[data-panel="battles"]')

    @property
    def battle_links(self):
        return self.battles_panel.locator('.post-title a[href^="/battles/"]')

    @property
    def battle_card_times(self):
        """Per-card timestamps: `<time class="feed-time" datetime=… title=…>`."""
        return self.battles_panel.locator(".post-meta time.feed-time[datetime]")

    @property
    def empty_battles_state(self):
        return self.battles_panel.locator(".empty-cta")

    def _load(self) -> None:
        self.response = self.bot.open_page(
            self.page,
            "/",
            base_url=self.base_url,
            description="Open home page",
        )

    def expect_loaded(self) -> None:
        self._expect_page_health()
        expect(self.main_content).to_be_visible()
        expect(self.home_feed).to_be_visible()
        expect(self.battles_tab).to_be_visible()
        self.ensure_battles_tab_active()
        self.bot.artifacts.set_current_url(self.page.url)

    def ensure_battles_tab_active(self) -> None:
        self.bot.record_step("ensure_battles_tab_active")
        if not self.battles_panel.is_visible():
            self.bot.click(self.battles_tab, description="Open Battles feed tab")
            self.page.wait_for_load_state("domcontentloaded")

        expect(self.battles_panel).to_be_visible()

    def first_battle(self) -> BattleSummary | None:
        return self.battle_at(0)

    def battle_at(self, index: int) -> BattleSummary | None:
        if index < 0:
            raise ValueError("battle index must be non-negative")

        if self.battle_links.count() == 0:
            return None

        if index >= self.battle_links.count():
            return None

        link = self.battle_links.nth(index)
        expect(link).to_be_visible()

        title = link.inner_text().strip()
        href = link.get_attribute("href") or ""

        assert title, f"expected Battle title at index {index} to be non-empty"
        assert href.startswith("/battles/"), f"expected Battle link, got {href!r}"

        return BattleSummary(title=title, href=href)

    def classify_battles_feed_state(self) -> str:
        battle = self.first_battle()
        if battle is not None:
            self.record_battle(battle)
            self.expect_battle_timestamp()
            self.bot.artifacts.add_runtime_data("battle_found", True)
            return "battles-present"

        self.expect_empty_battles_state()
        self.bot.artifacts.add_runtime_data("battle_found", False)
        return "battles-empty-state"

    def expect_battle_timestamp(self) -> None:
        time_el = self.battle_card_times.first
        expect(time_el).to_be_visible()
        datetime_attr = time_el.get_attribute("datetime") or ""
        assert datetime_attr, "expected Battle card <time> to carry a datetime"
        label = time_el.inner_text().strip()
        assert label, "expected Battle card timestamp text to be non-empty"
        self.bot.artifacts.add_runtime_data("first_battle_time", datetime_attr)
        self.bot.artifacts.add_runtime_data("first_battle_time_label", label)

    def expect_empty_battles_state(self) -> None:
        expect(self.empty_battles_state).to_be_visible()
        expect(self.empty_battles_state).to_contain_text("Nothing in the feed yet")
        self.bot.artifacts.add_runtime_data(
            "battles_empty_state",
            self.empty_battles_state.inner_text().strip(),
        )

    def open_first_battle(self) -> BattleSummary | None:
        battle = self.first_battle()
        if battle is None:
            return None

        self.record_battle(battle)
        self.bot.click(self.battle_links.first, description="Open first Battle")
        self.page.wait_for_load_state("domcontentloaded")
        self.bot.artifacts.set_current_url(self.page.url)
        return battle

    def record_battle(self, battle: BattleSummary) -> None:
        self.bot.artifacts.add_runtime_data("selected_battle_title", battle.title)
        self.bot.artifacts.add_runtime_data("selected_battle_href", battle.href)

    def _expect_page_health(self) -> None:
        if self.response is not None:
            assert_page_is_healthy(self.page, self.response)
            return

        assert_page_has_visible_body(self.page)
        assert_no_generic_server_error(self.page)
        assert_no_client_side_crash(self.page)
