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
class ClaimSummary:
    title: str
    href: str


class BattlePage(LoadableComponent):
    def __init__(
        self,
        page: Page,
        bot: ActionBot,
        *,
        base_url: str,
        path: str | None = None,
    ) -> None:
        self.page = page
        self.bot = bot
        self.base_url = base_url
        self.path = path
        self.response: Response | None = None

    @property
    def root(self):
        return self.page.locator("[data-battle-page]")

    @property
    def title(self):
        return self.page.locator(".hero-title")

    @property
    def stats(self):
        return self.page.locator(".hero-stats")

    @property
    def claims_section(self):
        return self.page.locator("#claims")

    @property
    def claim_rows(self):
        return self.claims_section.locator(".battle-claim-card")

    @property
    def empty_claims_state(self):
        return self.claims_section.locator(".empty-state")

    @property
    def comments_section(self):
        """Threaded Battle comments; absent when the Battle has none."""
        return self.page.locator("[data-battle-comments]")

    @property
    def comment_items(self):
        return self.comments_section.locator("li.comment")

    @property
    def sub_battles_section(self):
        """Sub-battle list; absent when the Battle has none."""
        return self.page.locator("[data-battle-sub-battles]")

    @property
    def parent_battle_link(self):
        """Link to the parent Battle; present only on sub-battles."""
        return self.page.locator("[data-battle-parent] a")

    def _load(self) -> None:
        if self.path is None:
            return

        self.response = self.bot.open_page(
            self.page,
            self.path,
            base_url=self.base_url,
            description="Open battle detail page",
        )

    def expect_loaded(self) -> None:
        self._expect_page_health()
        expect(self.root).to_be_visible()
        expect(self.title).to_be_visible()
        expect(self.stats).to_be_visible()
        expect(self.claims_section).to_be_visible()

        title_text = self.title.inner_text().strip()
        assert title_text, "expected Battle page title to be non-empty"

        self.bot.artifacts.add_runtime_data("battle_detail_title", title_text)
        self.bot.artifacts.set_current_url(self.page.url)

    def classify_claims_state(self) -> str:
        claim = self.first_claim()
        if claim is not None:
            self.bot.artifacts.add_runtime_data("claims_state", "claims-present")
            self.bot.artifacts.add_runtime_data("first_claim_title", claim.title)
            self.bot.artifacts.add_runtime_data("first_claim_href", claim.href)
            self.bot.artifacts.add_runtime_data(
                "visible_claim_count",
                self.claim_rows.count(),
            )
            return "claims-present"

        expect(self.empty_claims_state).to_be_visible()
        expect(self.empty_claims_state).to_contain_text("First claim wins")
        self.bot.artifacts.add_runtime_data("claims_state", "claims-empty-state")
        self.bot.artifacts.add_runtime_data("visible_claim_count", 0)
        return "claims-empty-state"

    def first_claim(self) -> ClaimSummary | None:
        if self.claim_rows.count() == 0:
            return None

        first_claim = self.claim_rows.first
        claim_title = first_claim.locator(".bcc-title")
        score_area = first_claim.locator(".bcc-vr")

        expect(first_claim).to_be_visible()
        expect(claim_title).to_be_visible()
        expect(score_area).to_be_visible()

        title = claim_title.inner_text().strip()
        href = first_claim.get_attribute("href") or ""

        assert title, "expected first Claim title to be non-empty"
        assert href.startswith("/claims/"), f"expected Claim link, got {href!r}"

        return ClaimSummary(title=title, href=href)

    def open_first_claim(self) -> ClaimSummary | None:
        claim = self.first_claim()
        if claim is None:
            return None

        self.bot.artifacts.add_runtime_data("selected_claim_title", claim.title)
        self.bot.artifacts.add_runtime_data("selected_claim_href", claim.href)
        self.bot.click(self.claim_rows.first, description="Open first Claim")
        self.page.wait_for_load_state("domcontentloaded")
        self.bot.artifacts.set_current_url(self.page.url)
        return claim

    def recover_to_home(self):
        from pages.home_page import HomePage

        home_link = self.page.locator('nav.crumb a[href="/"]').first
        self.bot.click(home_link, description="Recover from Battle page to home feed")
        self.page.wait_for_load_state("domcontentloaded")
        self.bot.artifacts.set_current_url(self.page.url)

        home = HomePage(self.page, self.bot, base_url=self.base_url)
        home.expect_loaded()
        self.bot.artifacts.add_runtime_data("recovery_url", self.page.url)
        return home

    def _expect_page_health(self) -> None:
        if self.response is not None:
            assert_page_is_healthy(self.page, self.response)
            return

        assert_page_has_visible_body(self.page)
        assert_no_generic_server_error(self.page)
        assert_no_client_side_crash(self.page)
