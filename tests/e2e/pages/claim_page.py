from __future__ import annotations

from playwright.sync_api import Page, Response, expect

from framework.action_bot import ActionBot
from framework.loadable import LoadableComponent
from helpers.page_health import (
    assert_no_client_side_crash,
    assert_no_generic_server_error,
    assert_page_has_visible_body,
    assert_page_is_healthy,
)


class ClaimPage(LoadableComponent):
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
        return self.page.locator("[data-claim-page]")

    @property
    def title(self):
        return self.page.locator(".hero-title")

    @property
    def verification_status(self):
        return self.page.locator(".vr-info")

    @property
    def stats(self):
        return self.page.locator('.hero-stats[aria-label="Claim stats"]')

    @property
    def body(self):
        return self.page.locator(".markdown-body").first

    @property
    def comments_section(self):
        """Threaded comments card (0.43.6); absent when a Claim has none."""
        return self.page.locator("[data-claim-comments]")

    @property
    def comment_items(self):
        return self.comments_section.locator("li.comment")

    @property
    def comment_replies(self):
        return self.comments_section.locator("li.comment.comment-reply")

    @property
    def votes(self):
        return self.page.locator("[data-claim-votes]")

    @property
    def upvote_button(self):
        return self.votes.locator('[data-claim-vote="up"]')

    @property
    def downvote_button(self):
        return self.votes.locator('[data-claim-vote="down"]')

    @property
    def superseded_banner(self):
        """Shown only when a newer version of the Claim exists."""
        return self.page.locator("[data-claim-superseded]")

    @property
    def battle_breadcrumb_link(self):
        return self.page.locator('nav.crumb a[href^="/battles/"]').first

    def _load(self) -> None:
        if self.path is None:
            return

        self.response = self.bot.open_page(
            self.page,
            self.path,
            base_url=self.base_url,
            description="Open claim detail page",
        )

    def expect_loaded(self) -> None:
        self._expect_page_health()
        expect(self.root).to_be_visible()
        expect(self.title).to_be_visible()
        expect(self.verification_status).to_be_visible()
        expect(self.stats).to_be_visible()
        expect(self.body).to_be_visible()
        expect(self.upvote_button).to_be_visible()
        expect(self.downvote_button).to_be_visible()

        title_text = self.title.inner_text().strip()
        assert title_text, "expected Claim page title to be non-empty"

        self.bot.artifacts.add_runtime_data("claim_detail_title", title_text)
        self.bot.artifacts.set_current_url(self.page.url)

    def expect_structured_body(self) -> None:
        for heading in ("Summary", "Methodology", "Conclusion"):
            locator = self.body.locator(":is(h1, h2, h3, h4)", has_text=heading)
            expect(locator.first).to_be_visible()

    def recover_to_battle(self):
        from pages.battle_page import BattlePage

        self.bot.click(
            self.battle_breadcrumb_link,
            description="Recover from Claim page to parent Battle",
        )
        self.page.wait_for_load_state("domcontentloaded")
        self.bot.artifacts.set_current_url(self.page.url)

        battle = BattlePage(self.page, self.bot, base_url=self.base_url)
        battle.expect_loaded()
        self.bot.artifacts.add_runtime_data("recovery_url", self.page.url)
        return battle

    def _expect_page_health(self) -> None:
        if self.response is not None:
            assert_page_is_healthy(self.page, self.response)
            return

        assert_page_has_visible_body(self.page)
        assert_no_generic_server_error(self.page)
        assert_no_client_side_crash(self.page)
