from __future__ import annotations

import pytest

from framework.action_bot import ActionBot
from helpers.config import PRODUCTION_BASE_URL
from helpers.oauth import assert_provider_redirect
from helpers.results import TestArtifacts
from pages.home_page import HomePage
from pages.login_page import LoginPage

REQUIRED_PRODUCTION_PROVIDERS = ("google", "github")


@pytest.mark.smoke
def test_login_entrypoint_smoke(
    page,
    base_url: str,
    e2e_artifacts: TestArtifacts,
) -> None:
    e2e_artifacts.add_runtime_data("scenario", "login-entrypoint-smoke")

    bot = ActionBot(e2e_artifacts)
    HomePage(page, bot, base_url=base_url).load()

    login_page = LoginPage(page, bot, base_url=base_url).open_from_home()
    missing_providers = _missing_providers(login_page, REQUIRED_PRODUCTION_PROVIDERS)

    e2e_artifacts.add_runtime_data(
        "available_oauth_providers",
        [
            provider
            for provider in REQUIRED_PRODUCTION_PROVIDERS
            if provider not in missing_providers
        ],
    )

    if missing_providers:
        message = f"missing OAuth provider link(s): {', '.join(missing_providers)}"
        if _is_production_base_url(base_url):
            pytest.fail(message)
        e2e_artifacts.add_runtime_data("local_oauth_configuration_blocker", message)

    login_page.recover_to_home()
    e2e_artifacts.set_current_url(page.url)


@pytest.mark.smoke
def test_google_oauth_smoke(
    page,
    base_url: str,
    e2e_artifacts: TestArtifacts,
) -> None:
    _run_provider_oauth_smoke(
        page,
        base_url=base_url,
        e2e_artifacts=e2e_artifacts,
        provider="google",
    )


@pytest.mark.smoke
def test_github_oauth_smoke(
    page,
    base_url: str,
    e2e_artifacts: TestArtifacts,
) -> None:
    _run_provider_oauth_smoke(
        page,
        base_url=base_url,
        e2e_artifacts=e2e_artifacts,
        provider="github",
    )


def _run_provider_oauth_smoke(
    page,
    *,
    base_url: str,
    e2e_artifacts: TestArtifacts,
    provider: str,
) -> None:
    e2e_artifacts.add_runtime_data("scenario", f"{provider}-oauth-smoke")
    e2e_artifacts.add_runtime_data("provider", provider)

    bot = ActionBot(e2e_artifacts)
    HomePage(page, bot, base_url=base_url).load()
    login_page = LoginPage(page, bot, base_url=base_url).open_from_home()

    if not login_page.provider_available(provider):
        message = f"{provider} OAuth provider link is not configured on target stand"
        if _is_production_base_url(base_url):
            pytest.fail(message)

        e2e_artifacts.add_runtime_data("local_oauth_configuration_blocker", message)
        pytest.skip(message)

    login_page.open_provider(provider)
    assert_provider_redirect(page, provider=provider, artifacts=e2e_artifacts)
    e2e_artifacts.set_current_url(page.url)


def _missing_providers(
    login_page: LoginPage,
    providers: tuple[str, ...],
) -> list[str]:
    return [
        provider
        for provider in providers
        if not login_page.provider_available(provider)
    ]


def _is_production_base_url(base_url: str) -> bool:
    return base_url.rstrip("/") == PRODUCTION_BASE_URL
