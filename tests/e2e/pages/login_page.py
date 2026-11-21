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

ProviderName = str


class LoginPage(LoadableComponent):
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
        return self.page.locator("[data-login-page]")

    @property
    def login_options(self):
        return self.page.locator("#login-options")

    @property
    def email_input(self):
        return self.page.locator("#email-input")

    @property
    def email_send_button(self):
        return self.page.locator("[data-email-send]")

    @property
    def topnav_login(self):
        return self.page.locator("#topnav-login")

    @property
    def brand_link(self):
        return self.page.locator('header.topnav a.brand[href="/"]').first

    def provider_link(self, provider: ProviderName):
        return self.page.locator(
            f'.login-provider[data-login-method="{provider}"]'
        ).first

    def provider_available(self, provider: ProviderName) -> bool:
        return (
            self.page.locator(
                f'.login-provider[data-login-method="{provider}"]'
            ).count()
            > 0
        )

    def _load(self) -> None:
        if self.path is None:
            return

        self.response = self.bot.open_page(
            self.page,
            self.path,
            base_url=self.base_url,
            description="Open login page",
        )

    def open_from_home(self):
        expect(self.topnav_login).to_be_visible()
        self.bot.click(self.topnav_login, description="Open login page from topnav")
        self.page.wait_for_load_state("domcontentloaded")
        self.response = None
        self.bot.artifacts.add_runtime_data("login_url", self.page.url)
        self.bot.artifacts.set_current_url(self.page.url)
        self.expect_loaded()
        return self

    def expect_loaded(self) -> None:
        self._expect_page_health()
        expect(self.root).to_be_visible()
        assert self.login_options.count() == 1, "expected login options container"
        expect(self.email_input).to_be_visible()
        expect(self.email_send_button).to_be_visible()
        self.bot.artifacts.add_runtime_data("login_url", self.page.url)
        self.bot.artifacts.set_current_url(self.page.url)

    def open_provider(self, provider: ProviderName) -> None:
        link = self.provider_link(provider)
        expect(link).to_be_visible()

        href = link.get_attribute("href") or ""
        assert href, f"expected {provider} provider link to have href"

        self.bot.artifacts.add_runtime_data("provider", provider)
        self.bot.artifacts.add_runtime_data("provider_link_href", href)
        self.bot.click(link, description=f"Open {provider} OAuth provider")
        self.page.wait_for_load_state("domcontentloaded", timeout=20_000)
        self.bot.artifacts.add_runtime_data("provider_url", self.page.url)
        self.bot.artifacts.set_current_url(self.page.url)

    def recover_to_home(self):
        from pages.home_page import HomePage

        self.bot.click(self.brand_link, description="Recover from login page to home")
        self.page.wait_for_load_state("domcontentloaded")
        self.bot.artifacts.add_runtime_data("recovery_url", self.page.url)
        self.bot.artifacts.set_current_url(self.page.url)

        home = HomePage(self.page, self.bot, base_url=self.base_url)
        home.expect_loaded()
        return home

    def _expect_page_health(self) -> None:
        if self.response is not None:
            assert_page_is_healthy(self.page, self.response)
            return

        assert_page_has_visible_body(self.page)
        assert_no_generic_server_error(self.page)
        assert_no_client_side_crash(self.page)
