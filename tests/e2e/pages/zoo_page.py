from __future__ import annotations

import re

from playwright.sync_api import Page, Response, expect

from framework.action_bot import ActionBot
from framework.loadable import LoadableComponent
from helpers.page_health import assert_page_is_healthy


class ZooPage(LoadableComponent):
    """The Crawler Zoo home (`/zoo`)."""

    def __init__(self, page: Page, bot: ActionBot, *, base_url: str) -> None:
        self.page = page
        self.bot = bot
        self.base_url = base_url
        self.response: Response | None = None

    @property
    def gate_total(self):
        return self.page.get_by_test_id("zoo-gate-total")

    @property
    def register_links(self):
        return self.page.get_by_test_id("zoo-register").locator("a")

    @property
    def trapdoor(self):
        return self.page.locator("a.c-zoo-trapdoor")

    def _load(self) -> None:
        self.response = self.bot.open_page(
            self.page, "/zoo", base_url=self.base_url, description="Open Crawler Zoo"
        )

    def expect_loaded(self) -> None:
        assert_page_is_healthy(self.page, self.response)
        expect(self.gate_total).to_be_visible()
        # The trap door must exist in the DOM but stay invisible to people.
        expect(self.trapdoor).to_have_count(1)
        expect(self.trapdoor).to_have_attribute("rel", "nofollow")
        og_image = self.page.locator('meta[property="og:image"]')
        expect(og_image).to_have_attribute("content", re.compile(r"/og/zoo\?d="))
        self.bot.artifacts.set_current_url(self.page.url)

    def open_first_specimen(self) -> ZooSpecimenPage | None:
        if self.register_links.count() == 0:
            return None
        self.bot.click(
            self.register_links.first, description="Open first register card"
        )
        self.page.wait_for_load_state("domcontentloaded")
        specimen = ZooSpecimenPage(self.page, self.bot, base_url=self.base_url)
        specimen.expect_loaded()
        return specimen


class ZooSpecimenPage(LoadableComponent):
    """One species' specimen file (`/zoo/bots/{slug}`)."""

    def __init__(self, page: Page, bot: ActionBot, *, base_url: str) -> None:
        self.page = page
        self.bot = bot
        self.base_url = base_url

    @property
    def specimen(self):
        return self.page.get_by_test_id("zoo-specimen")

    def _load(self) -> None:  # reached by navigation from ZooPage
        return None

    def expect_loaded(self) -> None:
        expect(self.specimen).to_be_visible()
        expect(self.specimen.locator("h1")).not_to_be_empty()
        expect(self.specimen).to_contain_text("How to block")
        # Food bowl + vending machine (read-only check: smoke never feeds).
        bowl = self.page.get_by_test_id("zoo-bowl")
        expect(bowl).to_be_visible()
        expect(bowl.get_by_test_id("zoo-vend-slot").locator("option")).to_have_count(12)
        expect(bowl.get_by_test_id("zoo-tokens")).to_contain_text("tokens left today")
        self.bot.artifacts.set_current_url(self.page.url)


class ZooLeaderboardPage(LoadableComponent):
    """`/zoo/leaderboard`."""

    def __init__(self, page: Page, bot: ActionBot, *, base_url: str) -> None:
        self.page = page
        self.bot = bot
        self.base_url = base_url
        self.response: Response | None = None

    def _load(self) -> None:
        self.response = self.bot.open_page(
            self.page,
            "/zoo/leaderboard",
            base_url=self.base_url,
            description="Open Crawler Zoo leaderboard",
        )

    def expect_loaded(self) -> None:
        assert_page_is_healthy(self.page, self.response)
        expect(self.page.locator(".feed-title")).to_be_visible()
        expect(self.page.locator(".c-zoo-tabs a")).to_have_count(6)
        self.bot.artifacts.set_current_url(self.page.url)
