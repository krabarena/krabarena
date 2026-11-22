from __future__ import annotations

from dataclasses import dataclass
from urllib.parse import urlparse

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
class SearchResultSummary:
    title: str
    href: str
    path: str


class SearchPage(LoadableComponent):
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
    def main_content(self):
        return self.page.locator("#main-content")

    @property
    def title(self):
        return self.page.locator(".feed-title")

    @property
    def search_form(self):
        return self.page.locator("header.topnav form.search")

    @property
    def query_input(self):
        return self.search_form.locator('input[name="q"][data-search-input]')

    @property
    def kind_value(self):
        return self.search_form.locator('input[name="kind"][data-kind-value]')

    @property
    def kind_trigger(self):
        return self.search_form.locator("[data-kind-trigger]")

    @property
    def battles_kind_option(self):
        return self.search_form.locator('[data-kind-option="battles"]')

    @property
    def submit_button(self):
        return self.search_form.locator(".c-search-submit")

    @property
    def results_summary(self):
        return self.page.locator(".c-search-summary")

    @property
    def result_list(self):
        return self.page.locator(".c-search-results")

    @property
    def result_items(self):
        return self.page.locator(".c-search-result")

    @property
    def result_titles(self):
        return self.result_items.locator(".c-search-title")

    @property
    def empty_state(self):
        return self.page.locator(".empty")

    def _load(self) -> None:
        if self.path is None:
            return

        self.response = self.bot.open_page(
            self.page,
            self.path,
            base_url=self.base_url,
            description="Open search page",
        )

    def expect_loaded(self) -> None:
        self._expect_page_health()
        expect(self.main_content).to_be_visible()
        expect(self.title).to_be_visible()
        expect(self.title).to_contain_text("Search")
        expect(self.search_form).to_be_visible()
        expect(self.query_input).to_be_visible()
        expect(self.kind_trigger).to_be_visible()
        expect(self.submit_button).to_be_visible()
        self.bot.artifacts.set_current_url(self.page.url)

    def submit_battle_search(self, query: str) -> None:
        self.expect_loaded()
        self.bot.fill(
            self.query_input,
            query,
            description="Fill search page query",
        )
        self.bot.record_step("select_search_kind", kind="battles")
        self.page.wait_for_load_state("load")
        expect(self.kind_trigger).to_be_visible()
        expect(self.kind_trigger).to_be_enabled()
        self.kind_trigger.click()
        expect(self.battles_kind_option).to_be_visible()
        self.battles_kind_option.click()
        self.bot.click(self.submit_button, description="Submit Battle search")
        self.page.wait_for_load_state("domcontentloaded")
        self.response = None
        self.bot.artifacts.add_runtime_data("search_kind", "battles")
        self.bot.artifacts.add_runtime_data("search_results_url", self.page.url)
        self.bot.artifacts.set_current_url(self.page.url)

    def expect_results_for(self, query: str) -> None:
        self.expect_loaded()
        expect(self.query_input).to_have_value(query)
        expect(self.kind_value).to_have_value("battles")
        expect(self.results_summary).to_be_visible()
        expect(self.results_summary).to_contain_text(query)
        expect(self.result_list).to_be_visible()

        result_count = self.result_items.count()
        assert result_count > 0, (
            f"expected at least one search result for known Battle query {query!r}"
        )

        self.bot.artifacts.add_runtime_data("visible_result_count", result_count)
        self.bot.artifacts.add_runtime_data("search_results_url", self.page.url)
        self.bot.artifacts.set_current_url(self.page.url)

    def first_result(self) -> SearchResultSummary:
        self.expect_results_for(self.query_input.input_value())

        first_link = self.result_titles.first
        expect(first_link).to_be_visible()

        title = first_link.inner_text().strip()
        href = first_link.get_attribute("href") or ""
        path = _href_path(href)

        assert title, "expected first search result title to be non-empty"
        assert path.startswith("/battles/"), (
            f"expected Battle result link, got {href!r}"
        )

        return SearchResultSummary(title=title, href=href, path=path)

    def open_first_result(self) -> SearchResultSummary:
        result = self.first_result()
        self.bot.artifacts.add_runtime_data("first_result_title", result.title)
        self.bot.artifacts.add_runtime_data("first_result_href", result.href)
        self.bot.artifacts.add_runtime_data("first_result_path", result.path)

        result_origin = _href_origin(result.href)
        if result_origin is None or result_origin == _href_origin(self.page.url):
            self.bot.click(
                self.result_titles.first, description="Open first search result"
            )
            self.page.wait_for_load_state("domcontentloaded")
        else:
            self.response = self.bot.open_page(
                self.page,
                result.path,
                base_url=self.base_url,
                description="Open first search result path on target stand",
            )

        self.bot.artifacts.add_runtime_data("result_detail_url", self.page.url)
        self.bot.artifacts.set_current_url(self.page.url)
        return result

    def recover_from_result_detail(self, query: str) -> None:
        self.bot.record_step("recover_to_search_results")
        self.response = self.page.go_back(wait_until="domcontentloaded")
        self.bot.artifacts.add_runtime_data("recovery_url", self.page.url)
        self.expect_results_for(query)

    def _expect_page_health(self) -> None:
        if self.response is not None:
            assert_page_is_healthy(self.page, self.response)
            return

        assert_page_has_visible_body(self.page)
        assert_no_generic_server_error(self.page)
        assert_no_client_side_crash(self.page)


def _href_path(href: str) -> str:
    parsed = urlparse(href)
    return parsed.path if parsed.scheme else href


def _href_origin(href: str) -> str | None:
    parsed = urlparse(href)
    if not parsed.scheme:
        return None
    return f"{parsed.scheme}://{parsed.netloc}"
